//! 针对真实 PostgreSQL 的 schema 契约测试。
//!
//! 开关与夹具见 `common/mod.rs`。这里锁住的是**结构性不变量**，不是 CRUD：
//! 定时触发的幂等、互相引用的表能否在一个事务里建起来、非法枚举值会不会被
//! CHECK 拦下、续传查询会不会退化成全表扫描。这些都用 psql 手工验过一遍，
//! 落成测试是为了以后改 schema 时不会悄悄退化。

mod common;

use sqlx::Row;

db_test!(migration_is_idempotent, |f| {
    // 服务每次启动都会调 migrate()，重复跑不能报错
    f.store.migrate().await.expect("重复迁移应当无副作用");
});

db_test!(monthly_partitions_exist_and_are_not_recreated, |f| {
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_class c
         JOIN pg_inherits i ON i.inhrelid = c.oid
         JOIN pg_class p ON p.oid = i.inhparent
         WHERE p.relname = 'run_events'",
    )
    .fetch_one(f.store.pool())
    .await
    .expect("查询分区");
    // 当月 + 未来 3 个月 + DEFAULT
    assert_eq!(count, 5, "run_events 应当有 5 个分区，实际 {count}");

    let created = f.store.ensure_partitions(3).await.expect("再次确保分区");
    assert_eq!(created, 0, "已存在的分区不应被重复创建");
});

db_test!(schedule_double_fire_is_impossible_by_constraint, |f| {
    let ids = seed(&f).await;
    let insert = "INSERT INTO runs (id, workspace_id, task_id, task_version_id, schedule_id, fire_at, trigger, status)
                  VALUES (gen_random_uuid(), $1, $2, $3, $4, $5, 'schedule', 'queued')";
    let fire_at = chrono::Utc::now();

    sqlx::query(insert)
        .bind(ids.workspace)
        .bind(ids.task)
        .bind(ids.version)
        .bind(ids.schedule)
        .bind(fire_at)
        .execute(f.store.pool())
        .await
        .expect("第一个副本应当插入成功");

    // 第二个副本算出同一个触发点
    let err = sqlx::query(insert)
        .bind(ids.workspace)
        .bind(ids.task)
        .bind(ids.version)
        .bind(ids.schedule)
        .bind(fire_at)
        .execute(f.store.pool())
        .await
        .expect_err("重复触发必须被唯一约束挡下");
    assert_eq!(
        err.as_database_error().and_then(|e| e.code()).as_deref(),
        Some("23505"),
        "应当是唯一键冲突，实际：{err}"
    );

    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM runs WHERE schedule_id = $1")
        .bind(ids.schedule)
        .fetch_one(f.store.pool())
        .await
        .expect("计数");
    assert_eq!(n, 1);
});

db_test!(manual_runs_are_not_constrained_by_the_schedule_key, |f| {
    let ids = seed(&f).await;
    // schedule_id 为 NULL 时唯一约束天然不生效，连点三次触发要产生三个 run
    for _ in 0..3 {
        sqlx::query(
            "INSERT INTO runs (id, workspace_id, task_id, task_version_id, trigger, status)
             VALUES (gen_random_uuid(), $1, $2, $3, 'manual', 'queued')",
        )
        .bind(ids.workspace)
        .bind(ids.task)
        .bind(ids.version)
        .execute(f.store.pool())
        .await
        .expect("手动触发不应受限");
    }
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM runs WHERE trigger = 'manual'")
        .fetch_one(f.store.pool())
        .await
        .expect("计数");
    assert_eq!(n, 3);
});

db_test!(task_and_its_first_version_commit_atomically, |f| {
    let workspace = uuid::Uuid::from(f.workspace);
    let task = uuid::Uuid::now_v7();
    let version = uuid::Uuid::now_v7();

    // tasks 与 task_versions 互相引用；FK 延迟到提交时校验，因此能在一个事务里建起来
    let mut tx = f.store.pool().begin().await.expect("开事务");
    sqlx::query(
        "INSERT INTO tasks (id, workspace_id, name, current_version_id) VALUES ($1,$2,'t',$3)",
    )
    .bind(task)
    .bind(workspace)
    .bind(version)
    .execute(&mut *tx)
    .await
    .expect("插任务");
    sqlx::query("INSERT INTO task_versions (id, task_id, version_no, dag_spec, rules_hash) VALUES ($1,$2,1,'{}','h')")
        .bind(version).bind(task)
        .execute(&mut *tx).await.expect("插版本");
    tx.commit().await.expect("提交");

    let ok: bool =
        sqlx::query_scalar("SELECT current_version_id IS NOT NULL FROM tasks WHERE id = $1")
            .bind(task)
            .fetch_one(f.store.pool())
            .await
            .expect("查询");
    assert!(ok);
});

db_test!(dangling_version_reference_fails_at_commit, |f| {
    let workspace = uuid::Uuid::from(f.workspace);
    let mut tx = f.store.pool().begin().await.expect("开事务");
    sqlx::query(
        "INSERT INTO tasks (id, workspace_id, name, current_version_id) VALUES ($1,$2,'t',$3)",
    )
    .bind(uuid::Uuid::now_v7())
    .bind(workspace)
    .bind(uuid::Uuid::now_v7())
    .execute(&mut *tx)
    .await
    .expect("延迟校验，插入本身不会立刻报错");
    let err = tx.commit().await.expect_err("提交时必须发现悬空引用");
    assert_eq!(
        err.as_database_error().and_then(|e| e.code()).as_deref(),
        Some("23503"),
        "应当是外键违例，实际：{err}"
    );
});

db_test!(
    check_constraints_reject_values_outside_the_proto_enums,
    |f| {
        let ids = seed(&f).await;
        // 大小写不同也必须被拒：CHECK 的取值要与 ai-task-proto 的 serde tag 逐字一致
        let err = sqlx::query(
            "INSERT INTO runs (id, workspace_id, task_id, task_version_id, trigger, status)
         VALUES (gen_random_uuid(), $1, $2, $3, 'manual', 'RUNNING')",
        )
        .bind(ids.workspace)
        .bind(ids.task)
        .bind(ids.version)
        .execute(f.store.pool())
        .await
        .expect_err("非法状态必须被 CHECK 拦下");
        assert_eq!(
            err.as_database_error().and_then(|e| e.code()).as_deref(),
            Some("23514"),
            "应当是 CHECK 违例，实际：{err}"
        );
    }
);

db_test!(event_replay_query_does_not_degrade_to_a_seq_scan, |f| {
    let ids = seed(&f).await;
    let run: uuid::Uuid = sqlx::query_scalar(
        "INSERT INTO runs (id, workspace_id, task_id, task_version_id, trigger, status)
         VALUES (gen_random_uuid(), $1, $2, $3, 'manual', 'running') RETURNING id",
    )
    .bind(ids.workspace)
    .bind(ids.task)
    .bind(ids.version)
    .fetch_one(f.store.pool())
    .await
    .expect("建 run");

    sqlx::query(
        "INSERT INTO run_events (run_id, seq, ts, node_key, kind, payload)
         SELECT $1, s, now(), 'a', 'agent_text', jsonb_build_object('kind','agent_text','text','x')
         FROM generate_series(1, 2000) s",
    )
    .bind(run)
    .execute(f.store.pool())
    .await
    .expect("写事件");
    sqlx::query("ANALYZE run_events")
        .execute(f.store.pool())
        .await
        .expect("ANALYZE");

    // SSE 续传的热查询。分区表 + Merge Append 是预期形态，退化成顺序扫描不是。
    let plan: String = sqlx::query(
        "EXPLAIN SELECT seq, payload FROM run_events
         WHERE run_id = $1 AND seq > 1000 ORDER BY seq LIMIT 200",
    )
    .bind(run)
    .fetch_all(f.store.pool())
    .await
    .expect("EXPLAIN")
    .iter()
    .map(|r| r.get::<String, _>(0))
    .collect::<Vec<_>>()
    .join("\n");

    assert!(
        plan.contains("Index Scan") || plan.contains("Index Only Scan"),
        "续传查询没走索引：\n{plan}"
    );
    assert!(!plan.contains("Seq Scan"), "续传查询出现顺序扫描：\n{plan}");
});

db_test!(deleting_a_run_takes_its_event_stream_with_it, |f| {
    // run_events / run_metrics 是分区表且没有指向 runs 的外键，级联是手写的。
    // 漏掉的话事件表里会留下永远读不到、也永远不会变小的孤儿行——而界面上
    // 明明写着"会同时删掉它们的完整事件流"。
    let ids = seed(&f).await;
    let run = insert_finished_run(&f, &ids).await;
    insert_event(&f, run, 1).await;
    insert_event(&f, run, 2).await;
    insert_metric(&f, run).await;

    let done = f
        .store
        .delete_run(f.workspace, ai_task_proto::RunId(run))
        .await
        .expect("删 run");
    assert_eq!(done, Some(true));

    assert_eq!(orphans(&f).await, (0, 0), "事件或采样没被一起删掉");
});

db_test!(deleting_a_task_takes_its_runs_event_streams_with_it, |f| {
    let ids = seed(&f).await;
    let run = insert_finished_run(&f, &ids).await;
    insert_event(&f, run, 1).await;
    insert_metric(&f, run).await;

    assert!(
        f.store
            .delete_task(f.workspace, ai_task_proto::TaskId(ids.task))
            .await
            .expect("删任务")
    );

    // runs 随 tasks 级联走了，事件和采样必须跟着一起走
    assert_eq!(orphans(&f).await, (0, 0), "删任务留下了孤儿事件");
});

db_test!(a_running_run_refuses_to_be_deleted, |f| {
    // 执行器还在往它的事件流里写。删掉之后那些写入就成了新的孤儿
    let ids = seed(&f).await;
    let run = uuid::Uuid::now_v7();
    sqlx::query(
        "INSERT INTO runs (id, workspace_id, task_id, task_version_id, status, trigger)
         VALUES ($1,$2,$3,$4,'running','manual')",
    )
    .bind(run)
    .bind(ids.workspace)
    .bind(ids.task)
    .bind(ids.version)
    .execute(f.store.pool())
    .await
    .expect("插 run");

    let done = f
        .store
        .delete_run(f.workspace, ai_task_proto::RunId(run))
        .await
        .expect("删 run");
    assert_eq!(done, Some(false), "正在跑的 run 不该被删掉");
});

db_test!(deleting_a_run_that_is_already_gone_is_not_an_error, |f| {
    // 重复 DELETE 要幂等：调用方要的结果已经达成了
    let done = f
        .store
        .delete_run(f.workspace, ai_task_proto::RunId(uuid::Uuid::now_v7()))
        .await
        .expect("删不存在的 run");
    assert_eq!(done, None);
});

// ---------------------------------------------------------------- 夹具

struct Ids {
    workspace: uuid::Uuid,
    task: uuid::Uuid,
    version: uuid::Uuid,
    schedule: uuid::Uuid,
}

/// 已经跑完的 run。只有终态才允许删。
async fn insert_finished_run(f: &common::Fixture, ids: &Ids) -> uuid::Uuid {
    let run = uuid::Uuid::now_v7();
    sqlx::query(
        "INSERT INTO runs (id, workspace_id, task_id, task_version_id, status, trigger, finished_at)
         VALUES ($1,$2,$3,$4,'succeeded','manual', now())",
    )
    .bind(run)
    .bind(ids.workspace)
    .bind(ids.task)
    .bind(ids.version)
    .execute(f.store.pool())
    .await
    .expect("插 run");
    run
}

async fn insert_event(f: &common::Fixture, run: uuid::Uuid, seq: i64) {
    sqlx::query(
        "INSERT INTO run_events (run_id, seq, ts, kind, payload)
         VALUES ($1,$2, now(), 'log', '{\"kind\":\"log\"}')",
    )
    .bind(run)
    .bind(seq)
    .execute(f.store.pool())
    .await
    .expect("插事件");
}

async fn insert_metric(f: &common::Fixture, run: uuid::Uuid) {
    sqlx::query(
        "INSERT INTO run_metrics (run_id, node_key, ts, cpu_usec, rss_bytes)
         VALUES ($1,'probe', now(), 1, 1)",
    )
    .bind(run)
    .execute(f.store.pool())
    .await
    .expect("插采样");
}

/// (孤儿事件数, 孤儿采样数)
async fn orphans(f: &common::Fixture) -> (i64, i64) {
    let events: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM run_events e WHERE NOT EXISTS (SELECT 1 FROM runs r WHERE r.id = e.run_id)",
    )
    .fetch_one(f.store.pool())
    .await
    .expect("数孤儿事件");
    let metrics: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM run_metrics m WHERE NOT EXISTS (SELECT 1 FROM runs r WHERE r.id = m.run_id)",
    )
    .fetch_one(f.store.pool())
    .await
    .expect("数孤儿采样");
    (events, metrics)
}

/// 在夹具的默认 workspace 下建一套 task / version / schedule。
async fn seed(f: &common::Fixture) -> Ids {
    let workspace = uuid::Uuid::from(f.workspace);
    let task = uuid::Uuid::now_v7();
    let version = uuid::Uuid::now_v7();
    let schedule = uuid::Uuid::now_v7();

    let mut tx = f.store.pool().begin().await.expect("开事务");
    sqlx::query(
        "INSERT INTO tasks (id, workspace_id, name, current_version_id) VALUES ($1,$2,'probe',$3)",
    )
    .bind(task)
    .bind(workspace)
    .bind(version)
    .execute(&mut *tx)
    .await
    .expect("插任务");
    sqlx::query("INSERT INTO task_versions (id, task_id, version_no, dag_spec, rules_hash) VALUES ($1,$2,1,'{}','h')")
        .bind(version).bind(task).execute(&mut *tx).await.expect("插版本");
    sqlx::query(
        "INSERT INTO schedules (id, workspace_id, task_id, cron) VALUES ($1,$2,$3,'*/1 * * * *')",
    )
    .bind(schedule)
    .bind(workspace)
    .bind(task)
    .execute(&mut *tx)
    .await
    .expect("插定时");
    tx.commit().await.expect("提交");

    Ids {
        workspace,
        task,
        version,
        schedule,
    }
}

db_test!(retention_bounds_the_partition_count, |f| {
    // 上面那条 `monthly_partitions_exist_and_are_not_recreated` 只断言"分区建出来了"，
    // 分区数涨到几十张它照样绿。而 SSE 的续传查询没有 ts 谓词，
    // 每多一张分区就多扫一张表——所以真正要守的是**上界**。
    async fn count_partitions(store: &ai_task_store::Store) -> i64 {
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM pg_inherits i
             JOIN pg_class p ON p.oid = i.inhparent
             WHERE p.relname = 'run_events'",
        )
        .fetch_one(store.pool())
        .await
        .expect("数分区")
    }

    // 造一批很老的月度分区，模拟跑了两年的实例
    for ym in ["202301", "202302", "202303", "202304"] {
        let sql = format!(
            "CREATE TABLE run_events_{ym} PARTITION OF run_events
             FOR VALUES FROM ('{}-01') TO ('{}-01')",
            format_args!("{}-{}", &ym[..4], &ym[4..]),
            // 下个月
            if &ym[4..] == "12" {
                format!("{}-01", ym[..4].parse::<i32>().expect("year") + 1)
            } else {
                format!(
                    "{}-{:02}",
                    &ym[..4],
                    ym[4..].parse::<i32>().expect("month") + 1
                )
            }
        );
        sqlx::query(sqlx::AssertSqlSafe(sql))
            .execute(f.store.pool())
            .await
            .expect("建老分区");
    }

    let before = count_partitions(&f.store).await;
    assert!(before >= 5, "至少有兜底分区 + 四张老分区，实际 {before}");

    let dropped = f.store.drop_old_partitions(6).await.expect("清理");
    assert_eq!(dropped, 4, "四张 2023 年的分区都该被丢掉");

    let after = count_partitions(&f.store).await;
    assert_eq!(after, before - 4);

    // 兜底分区永远保留：它是"维护任务没跑"时唯一的安全网，丢了会让写入直接失败
    let has_default: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_class WHERE relname = 'run_events_default')",
    )
    .fetch_one(f.store.pool())
    .await
    .expect("查兜底分区");
    assert!(has_default, "DEFAULT 分区不能被保留策略丢掉");

    // 再跑一次是幂等的
    assert_eq!(f.store.drop_old_partitions(6).await.expect("再清理"), 0);
});

db_test!(status_filtered_run_listing_uses_the_status_index, |f| {
    // 加索引**不等于**索引会被用上。`list_runs` 原来把三个可选条件挤在
    // 一条 `($n IS NULL OR ...)` 里，planner 在计划期看不出 status 会不会参与，
    // 于是照样全扫再过滤。这条断言守的就是"查询形状拆开了"这件事。
    let ids = seed(&f).await;
    // **选择性要真实。** 四种状态各占 25% 的话，沿着 created_at 索引走几十行
    // 就能凑够 LIMIT，planner 选全扫再过滤是对的。这个索引真正要救的是
    // 「几万条里只有几条是这个状态」——那时候不走索引要扫到天亮。
    sqlx::query(
        "INSERT INTO runs (id, workspace_id, task_id, task_version_id, trigger, status, created_at)
         SELECT gen_random_uuid(), $1, $2, $3, 'manual',
                CASE WHEN s % 5000 = 0 THEN 'resource_exceeded' ELSE 'succeeded' END,
                now() - make_interval(secs => s)
         FROM generate_series(1, 30000) s",
    )
    .bind(ids.workspace)
    .bind(ids.task)
    .bind(ids.version)
    .execute(f.store.pool())
    .await
    .expect("写 run");
    sqlx::query("ANALYZE runs")
        .execute(f.store.pool())
        .await
        .expect("ANALYZE");

    let plan: String = sqlx::query(
        "EXPLAIN SELECT id FROM runs
         WHERE workspace_id = $1 AND status = ANY(ARRAY['resource_exceeded'])
         ORDER BY created_at DESC, id DESC LIMIT 50",
    )
    .bind(ids.workspace)
    .fetch_all(f.store.pool())
    .await
    .expect("EXPLAIN")
    .iter()
    .map(|row| row.get::<String, _>(0))
    .collect::<Vec<_>>()
    .join("\n");

    assert!(
        plan.contains("runs_status_recent_idx"),
        "状态筛选没走 runs_status_recent_idx，计划是：\n{plan}"
    );
});

db_test!(a_time_window_on_the_run_list_is_an_index_range, |f| {
    // 执行记录页的"最近 24 小时 / 7 天"。`created_at >= since` 必须成为
    // runs_recent_idx 上的范围条件（Index Cond），而不是沿着索引一路扫下去再过滤——
    // 后者在窗口之外的行很多时会把整张表的历史读一遍。
    let ids = seed(&f).await;
    sqlx::query(
        "INSERT INTO runs (id, workspace_id, task_id, task_version_id, trigger, status, created_at)
         SELECT gen_random_uuid(), $1, $2, $3, 'manual', 'succeeded',
                now() - make_interval(secs => s * 60)
         FROM generate_series(1, 30000) s",
    )
    .bind(ids.workspace)
    .bind(ids.task)
    .bind(ids.version)
    .execute(f.store.pool())
    .await
    .expect("写 run");
    sqlx::query("ANALYZE runs")
        .execute(f.store.pool())
        .await
        .expect("ANALYZE");

    let plan: String = sqlx::query(
        "EXPLAIN SELECT id FROM runs
         WHERE workspace_id = $1 AND created_at >= now() - interval '1 day'
         ORDER BY created_at DESC, id DESC LIMIT 50",
    )
    .bind(ids.workspace)
    .fetch_all(f.store.pool())
    .await
    .expect("EXPLAIN")
    .iter()
    .map(|row| row.get::<String, _>(0))
    .collect::<Vec<_>>()
    .join("\n");

    assert!(
        plan.contains("runs_recent_idx"),
        "时间窗口没走 runs_recent_idx，计划是：\n{plan}"
    );
    let index_cond = plan
        .lines()
        .find(|line| line.contains("Index Cond"))
        .unwrap_or_default();
    assert!(
        index_cond.contains("created_at"),
        "created_at 应当是索引条件而不是过滤条件，计划是：\n{plan}"
    );
});

db_test!(metrics_by_time_can_be_read_without_a_sort, |f| {
    // 原来的索引是 (run_id, node_key, ts)，中间隔着 node_key，
    // 所以 `WHERE run_id = $1 ORDER BY ts` 出来的顺序是按节点分组的，还得再排一次。
    //
    // **断言的是"索引能满足这个查询形状"，不是"planner 一定会选它"。**
    // 选不选取决于表有多大、数据多稀疏——在几百行的测试库上全扫再排序
    // 完全可能更便宜，那时候 planner 不用索引是对的。写死计划形状的断言
    // 会在数据量一变就红，那种红不说明任何问题。
    let ids = seed(&f).await;
    let run: uuid::Uuid = sqlx::query_scalar(
        "INSERT INTO runs (id, workspace_id, task_id, task_version_id, trigger, status)
         VALUES (gen_random_uuid(), $1, $2, $3, 'manual', 'running') RETURNING id",
    )
    .bind(ids.workspace)
    .bind(ids.task)
    .bind(ids.version)
    .fetch_one(f.store.pool())
    .await
    .expect("建 run");

    sqlx::query(
        "INSERT INTO run_metrics (run_id, node_key, ts, cpu_usec, rss_bytes, pids)
         SELECT $1, 'n' || (s % 5), now() - make_interval(secs => s), s, s, 1
         FROM generate_series(1, 2000) s",
    )
    .bind(run)
    .execute(f.store.pool())
    .await
    .expect("写采样");
    sqlx::query("ANALYZE run_metrics")
        .execute(f.store.pool())
        .await
        .expect("ANALYZE");

    // 索引必须存在**且有效**。CONCURRENTLY 失败会留下一个 indisvalid = false 的
    // 索引——它不报错、也永远不被使用，比没建更难发现。
    let valid: bool = sqlx::query_scalar(
        "SELECT i.indisvalid FROM pg_class c JOIN pg_index i ON i.indexrelid = c.oid
         WHERE c.relname = 'run_metrics_run_ts_idx'",
    )
    .fetch_one(f.store.pool())
    .await
    .expect("查索引");
    assert!(valid, "run_metrics_run_ts_idx 不是有效索引");

    // 把全扫这条路堵上，逼 planner 表态：这个索引到底能不能满足
    // 「按 run_id 取、按 ts 有序」而不额外排序。
    //
    // **必须钉在同一条连接上。** SET 是会话级的，而池子下一次可能给另一条连接；
    // SET LOCAL 更糟——不在事务里就是空操作，还不报错。
    let mut conn = f.store.pool().acquire().await.expect("取连接");
    sqlx::query("SET enable_seqscan = off")
        .execute(&mut *conn)
        .await
        .expect("关掉全扫");
    let plan: String =
        sqlx::query("EXPLAIN SELECT node_key, ts FROM run_metrics WHERE run_id = $1 ORDER BY ts")
            .bind(run)
            .fetch_all(&mut *conn)
            .await
            .expect("EXPLAIN")
            .iter()
            .map(|row| row.get::<String, _>(0))
            .collect::<Vec<_>>()
            .join("\n");

    // 分区表上，父索引会在每张分区上生成一个**自己的名字**
    // （run_metrics_202609_run_id_ts_idx），计划里出现的是那些名字，不是父索引名。
    assert!(
        plan.contains("run_id_ts_idx"),
        "堵掉全扫之后仍然没走 (run_id, ts) 索引：\n{plan}"
    );
    // 真正要的是这个：Merge Append 直接产出有序结果，**没有单独的 Sort 节点**。
    // 老索引 (run_id, node_key, ts) 给不出这个顺序，必然多一次排序。
    assert!(
        !plan
            .lines()
            .any(|line| line.trim_start().starts_with("Sort  ")),
        "计划里还有排序节点，说明索引没能直接给出 ts 顺序：\n{plan}"
    );
});

db_test!(the_sse_query_prunes_partitions_it_cannot_contain, |f| {
    // SSE 的续传查询只有 run_id 和 seq 两个条件时，planner 只能 Merge Append
    // 扫过**所有**月度分区——而分区数每月 +1。给一个 ts 下界就能裁掉更早的那些。
    let ids = seed(&f).await;
    let run: uuid::Uuid = sqlx::query_scalar(
        "INSERT INTO runs (id, workspace_id, task_id, task_version_id, trigger, status)
         VALUES (gen_random_uuid(), $1, $2, $3, 'manual', 'running') RETURNING id",
    )
    .bind(ids.workspace)
    .bind(ids.task)
    .bind(ids.version)
    .fetch_one(f.store.pool())
    .await
    .expect("建 run");

    let touched = |plan: &str| plan.matches("run_events_").count();

    let without: String = sqlx::query(
        "EXPLAIN SELECT seq FROM run_events WHERE run_id = $1 AND seq > 0 ORDER BY seq LIMIT 200",
    )
    .bind(run)
    .fetch_all(f.store.pool())
    .await
    .expect("EXPLAIN")
    .iter()
    .map(|r| r.get::<String, _>(0))
    .collect::<Vec<_>>()
    .join("\n");

    let with: String = sqlx::query(
        "EXPLAIN SELECT seq FROM run_events
         WHERE run_id = $1 AND seq > 0 AND ts >= now() - interval '1 minute'
         ORDER BY seq LIMIT 200",
    )
    .bind(run)
    .fetch_all(f.store.pool())
    .await
    .expect("EXPLAIN")
    .iter()
    .map(|r| r.get::<String, _>(0))
    .collect::<Vec<_>>()
    .join("\n");

    assert!(
        touched(&with) <= touched(&without),
        "加了 ts 下界之后接触的分区不该变多：\n带下界 {}\n{with}\n\n不带 {}\n{without}",
        touched(&with),
        touched(&without)
    );
});
