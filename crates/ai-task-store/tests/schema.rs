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

// ---------------------------------------------------------------- 夹具

struct Ids {
    workspace: uuid::Uuid,
    task: uuid::Uuid,
    version: uuid::Uuid,
    schedule: uuid::Uuid,
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
