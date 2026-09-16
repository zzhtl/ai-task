//! 调度器针对真实 PostgreSQL 的测试。
//!
//! `tick(now)` 吃显式时间，所以「跑 5 分钟」「停机 3 分钟」都是模拟出来的，
//! 不用真等。需要 `AI_TASK_TEST_DATABASE_URL`，没设就跳过。

use ai_task_proto::{
    MisfirePolicy, OverlapPolicy, RunEventBody, RunStatus, TaskId, UsdMicros, WorkspaceId,
};
use ai_task_runtime::{CronSchedule, Scheduler};
use ai_task_store::schedules::NewSchedule;
use ai_task_store::{NewRun, NewTask, PendingEvent, RunOutcome, Store, StoreConfig};
use chrono::{DateTime, Duration, Utc};
use sqlx::{AssertSqlSafe, Connection, PgConnection};

mod common;

struct Harness {
    store: Store,
    workspace: WorkspaceId,
    admin_url: String,
    db: String,
}

impl Harness {
    async fn create() -> Option<Self> {
        let admin_url = std::env::var("AI_TASK_TEST_DATABASE_URL").ok()?;
        let db = format!("ai_task_test_{}", uuid::Uuid::new_v4().simple());
        let mut admin = PgConnection::connect(&admin_url).await.expect("连接管理库");
        let create = format!(r#"CREATE DATABASE "{db}""#);
        sqlx::query(AssertSqlSafe(create))
            .execute(&mut admin)
            .await
            .expect("建库");

        let url = match admin_url.rsplit_once('/') {
            Some((prefix, _)) => format!("{prefix}/{db}"),
            None => format!("{admin_url}/{db}"),
        };
        let store = Store::connect(&StoreConfig {
            url,
            ..Default::default()
        })
        .await
        .expect("连库");
        store.migrate().await.expect("迁移");

        let workspace = WorkspaceId::new();
        sqlx::query("INSERT INTO workspaces (id, name) VALUES ($1, 'default')")
            .bind(uuid::Uuid::from(workspace))
            .execute(store.pool())
            .await
            .expect("建 workspace");

        Some(Self {
            store,
            workspace,
            admin_url,
            db,
        })
    }

    async fn seed_task(&self) -> TaskId {
        let (task, _) = self
            .store
            .create_task(NewTask {
                workspace_id: self.workspace,
                name: format!("t-{}", uuid::Uuid::new_v4().simple()),
                description: None,
                spec: spec(),
                rules: vec![],
                rules_hash: "h0".into(),
                enabled: true,
            })
            .await
            .expect("建任务");
        task.id
    }

    /// 建一条定时配置，第一个触发点是 `first_fire`。
    async fn seed_schedule(
        &self,
        task_id: TaskId,
        cron: &str,
        misfire: MisfirePolicy,
        overlap: OverlapPolicy,
        first_fire: DateTime<Utc>,
    ) {
        self.store
            .create_schedule(NewSchedule {
                workspace_id: self.workspace,
                task_id,
                cron: cron.into(),
                timezone: "UTC".into(),
                misfire,
                overlap,
                jitter_s: 0,
                enabled: true,
                next_fire_at: first_fire,
                next_claim_at: first_fire,
            })
            .await
            .expect("建定时配置");
    }

    async fn runs_of(&self, task_id: TaskId) -> Vec<(DateTime<Utc>, RunStatus)> {
        let rows: Vec<(Option<DateTime<Utc>>, String)> = sqlx::query_as(
            "SELECT fire_at, status FROM runs WHERE task_id = $1 ORDER BY fire_at NULLS FIRST",
        )
        .bind(uuid::Uuid::from(task_id))
        .fetch_all(self.store.pool())
        .await
        .expect("查 run");
        rows.into_iter()
            .map(|(fire_at, status)| {
                (
                    fire_at.unwrap_or_default(),
                    serde_json::from_value(serde_json::Value::String(status)).expect("状态"),
                )
            })
            .collect()
    }

    /// 把一个 run 标成在跑，用来测 overlap。
    async fn mark_running(&self, task_id: TaskId) {
        let (_, version) = self
            .store
            .list_tasks(self.workspace, None, 100)
            .await
            .expect("列任务")
            .into_iter()
            .find(|t| t.id == task_id)
            .map(|t| (t.id, t.current_version_id))
            .expect("任务存在");

        let run = self
            .store
            .create_run(
                NewRun {
                    workspace_id: self.workspace,
                    task_id,
                    task_version_id: version,
                    trigger: ai_task_proto::TriggerKind::Manual,
                    dry_run: false,
                    inputs: None,
                    compare_to: None,
                },
                PendingEvent::run(RunEventBody::RunQueued {
                    task_version_id: version,
                    trigger: ai_task_proto::TriggerKind::Manual,
                    inputs: None,
                    dry_run: false,
                }),
            )
            .await
            .expect("建 run");
        self.store
            .start_run(
                run.id,
                None,
                None,
                PendingEvent::run(RunEventBody::RunStarted { worker: "t".into() }),
            )
            .await
            .expect("开始");
    }

    async fn finish_all(&self, task_id: TaskId) {
        let ids: Vec<uuid::Uuid> = sqlx::query_scalar(
            "SELECT id FROM runs WHERE task_id = $1 AND status IN ('queued','running')",
        )
        .bind(uuid::Uuid::from(task_id))
        .fetch_all(self.store.pool())
        .await
        .expect("查在途 run");
        for id in ids {
            self.store
                .finish_run(
                    ai_task_proto::RunId(id),
                    RunOutcome::new(RunStatus::Succeeded, UsdMicros::ZERO),
                    PendingEvent::run(RunEventBody::RunFinished {
                        status: RunStatus::Succeeded,
                        error: None,
                        cost_usd: UsdMicros::ZERO,
                    }),
                )
                .await
                .expect("收尾");
        }
    }

    async fn cleanup(self) {
        self.store.pool().close().await;
        if let Ok(mut admin) = PgConnection::connect(&self.admin_url).await {
            let sql = format!(r#"DROP DATABASE IF EXISTS "{}" WITH (FORCE)"#, self.db);
            let _ = sqlx::query(AssertSqlSafe(sql)).execute(&mut admin).await;
        }
    }
}

fn spec() -> ai_task_proto::DagSpec {
    use ai_task_proto::{
        AiNode, DagSpec, ExecutorKind, NodeConfig, NodeKey, NodeSpec, OnFailure, RetryPolicy,
    };
    DagSpec {
        nodes: vec![NodeSpec {
            key: NodeKey::parse("probe").expect("key"),
            name: None,
            config: NodeConfig::Ai(AiNode {
                prompt: "x".into(),
                executor: ExecutorKind::ClaudeCode,
                cli: None,
                model: None,
                effort: None,
                skills: vec![],
                tools: vec![],
                max_turns: None,
                budget_usd: None,
            }),
            inputs: Default::default(),
            output_schema: None,
            retry: RetryPolicy::default(),
            on_failure: OnFailure::default(),
            timeout_s: None,
            host: None,
            limits: None,
        }],
        edges: vec![],
        input_schema: None,
        budget_usd: None,
        timeout_s: None,
    }
}

fn at(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s)
        .expect("时间格式")
        .with_timezone(&Utc)
}

macro_rules! sched_test {
    ($name:ident, |$h:ident| $body:block) => {
        #[tokio::test]
        async fn $name() {
            let Some($h) = Harness::create().await else {
                common::skip_or_fail(stringify!($name), "未设置 AI_TASK_TEST_DATABASE_URL");
                return;
            };
            let outcome = {
                use futures_util::FutureExt as _;
                std::panic::AssertUnwindSafe(async $body).catch_unwind().await
            };
            $h.cleanup().await;
            if let Err(payload) = outcome {
                std::panic::resume_unwind(payload);
            }
        }
    };
}

// ---------------------------------------------------------------- 测试

sched_test!(
    five_minutes_of_a_per_minute_schedule_yields_exactly_five_runs,
    |h| {
        let task = h.seed_task().await;
        let start = at("2026-09-08T10:00:00Z");
        h.seed_schedule(
            task,
            "*/1 * * * *",
            MisfirePolicy::FireOnce,
            OverlapPolicy::Allow,
            start + Duration::minutes(1),
        )
        .await;

        let scheduler = Scheduler::new(h.store.clone());
        // 模拟每 250ms 轮询一次，一直跑到 10:05:00（含）——
        // 5 个触发点是 10:01…10:05，区间开着的话最后一个够不到
        let mut created = 0;
        for step in 0..=(5 * 60 * 4) {
            let now = start + Duration::milliseconds(i64::from(step) * 250);
            created += scheduler.tick(now).await.expect("轮询").created.len();
        }

        assert_eq!(created, 5, "5 分钟应当恰好 5 个 run");
        let runs = h.runs_of(task).await;
        assert_eq!(runs.len(), 5);
        let fire_points: Vec<_> = runs.iter().map(|(t, _)| t.to_rfc3339()).collect();
        assert_eq!(
            fire_points,
            (1..=5)
                .map(|m| (start + Duration::minutes(m)).to_rfc3339())
                .collect::<Vec<_>>(),
            "触发点必须落在整分钟上，无重复无遗漏"
        );
    }
);

sched_test!(
    a_three_minute_outage_with_fire_once_replays_exactly_one_run,
    |h| {
        let task = h.seed_task().await;
        let start = at("2026-09-08T10:00:00Z");
        h.seed_schedule(
            task,
            "*/1 * * * *",
            MisfirePolicy::FireOnce,
            OverlapPolicy::Allow,
            start + Duration::minutes(1),
        )
        .await;

        let scheduler = Scheduler::new(h.store.clone());
        // 10:01 正常跑一次
        scheduler
            .tick(start + Duration::seconds(61))
            .await
            .expect("轮询");
        assert_eq!(h.runs_of(task).await.len(), 1);

        // 停机三分钟：10:02 / 10:03 / 10:04 全错过，10:04:20 才恢复
        let resumed = start + Duration::seconds(4 * 60 + 20);
        let tick = scheduler.tick(resumed).await.expect("恢复后轮询");

        assert_eq!(tick.created.len(), 1, "fire_once 只补一次，不管积压多少");
        let runs = h.runs_of(task).await;
        assert_eq!(runs.len(), 2);
        assert_eq!(
            runs[1].0,
            start + Duration::minutes(4),
            "补的应当是最近的那个触发点，不是最早的"
        );
    }
);

sched_test!(fire_all_replays_the_whole_backlog, |h| {
    let task = h.seed_task().await;
    let start = at("2026-09-08T10:00:00Z");
    h.seed_schedule(
        task,
        "*/1 * * * *",
        MisfirePolicy::FireAll,
        OverlapPolicy::Allow,
        start + Duration::minutes(1),
    )
    .await;

    let scheduler = Scheduler::new(h.store.clone());
    let tick = scheduler
        .tick(start + Duration::seconds(4 * 60 + 20))
        .await
        .expect("轮询");
    assert_eq!(tick.created.len(), 4, "10:01–10:04 全补");
});

sched_test!(
    skip_discards_the_backlog_but_still_fires_the_current_one,
    |h| {
        let task = h.seed_task().await;
        let start = at("2026-09-08T10:00:00Z");
        h.seed_schedule(
            task,
            "*/1 * * * *",
            MisfirePolicy::Skip,
            OverlapPolicy::Allow,
            start + Duration::minutes(1),
        )
        .await;

        let scheduler = Scheduler::new(h.store.clone());
        // 10:04:10 恢复：10:04 还在 30s 宽限期内，算准点
        let tick = scheduler
            .tick(start + Duration::seconds(4 * 60 + 10))
            .await
            .expect("轮询");
        assert_eq!(
            tick.created.len(),
            1,
            "skip 丢弃积压，但准点的那个必须跑——否则 skip 就成了永不触发"
        );
        assert_eq!(h.runs_of(task).await[0].0, start + Duration::minutes(4));
    }
);

sched_test!(concurrent_replicas_never_double_fire_the_same_point, |h| {
    // 这条验的是「幂等靠约束不靠锁」：八个副本同时轮询同一个触发点，
    // UNIQUE (schedule_id, fire_at) 保证只有一个能插进去。
    let task = h.seed_task().await;
    let start = at("2026-09-08T10:00:00Z");
    h.seed_schedule(
        task,
        "*/1 * * * *",
        MisfirePolicy::FireOnce,
        OverlapPolicy::Allow,
        start + Duration::minutes(1),
    )
    .await;

    let now = start + Duration::seconds(61);
    let mut handles = Vec::new();
    for _ in 0..8 {
        let scheduler = Scheduler::new(h.store.clone());
        handles.push(tokio::spawn(async move {
            scheduler.tick(now).await.expect("轮询").created.len()
        }));
    }
    let mut total = 0;
    for handle in handles {
        total += handle.await.expect("副本");
    }

    assert_eq!(total, 1, "八个副本同时轮询，只能产生一个 run");
    assert_eq!(h.runs_of(task).await.len(), 1);
});

sched_test!(
    overlap_skip_drops_the_trigger_while_a_run_is_still_going,
    |h| {
        let task = h.seed_task().await;
        let start = at("2026-09-08T10:00:00Z");
        h.seed_schedule(
            task,
            "*/1 * * * *",
            MisfirePolicy::FireOnce,
            OverlapPolicy::Skip,
            start + Duration::minutes(1),
        )
        .await;
        h.mark_running(task).await;

        let scheduler = Scheduler::new(h.store.clone());
        let tick = scheduler
            .tick(start + Duration::seconds(61))
            .await
            .expect("轮询");
        assert!(
            tick.created.is_empty(),
            "上一次还没跑完，本次触发应当被丢弃"
        );

        // 但游标要推进：下一分钟能正常触发（前提是上一次结束了）
        h.finish_all(task).await;
        let tick = scheduler
            .tick(start + Duration::seconds(121))
            .await
            .expect("轮询");
        assert_eq!(tick.created.len(), 1, "上一次结束后应当恢复正常触发");
    }
);

sched_test!(
    overlap_queue_defers_the_trigger_until_the_previous_run_finishes,
    |h| {
        let task = h.seed_task().await;
        let start = at("2026-09-08T10:00:00Z");
        h.seed_schedule(
            task,
            "*/1 * * * *",
            MisfirePolicy::FireOnce,
            OverlapPolicy::Queue,
            start + Duration::minutes(1),
        )
        .await;
        h.mark_running(task).await;

        let scheduler = Scheduler::new(h.store.clone());
        let tick = scheduler
            .tick(start + Duration::seconds(61))
            .await
            .expect("轮询");
        assert_eq!(tick.deferred, 1, "queue 策略应当推迟而不是丢弃");
        assert!(tick.created.is_empty());

        // 上一次结束后，被推迟的触发点要补上——这是 queue 与 skip 的区别
        h.finish_all(task).await;
        let tick = scheduler
            .tick(start + Duration::seconds(75))
            .await
            .expect("轮询");
        assert_eq!(tick.created.len(), 1, "queue 推迟的触发点必须最终补上");
    }
);

sched_test!(
    a_disabled_task_does_not_fire_but_its_cursor_still_advances,
    |h| {
        let task = h.seed_task().await;
        let start = at("2026-09-08T10:00:00Z");
        h.seed_schedule(
            task,
            "*/1 * * * *",
            MisfirePolicy::FireAll,
            OverlapPolicy::Allow,
            start + Duration::minutes(1),
        )
        .await;
        sqlx::query("UPDATE tasks SET enabled = false WHERE id = $1")
            .bind(uuid::Uuid::from(task))
            .execute(h.store.pool())
            .await
            .expect("停用任务");

        let scheduler = Scheduler::new(h.store.clone());
        for step in 1..=5 {
            scheduler
                .tick(start + Duration::seconds(60 * step + 1))
                .await
                .expect("轮询");
        }
        assert!(h.runs_of(task).await.is_empty(), "停用的任务不该产生 run");

        // 游标推进了才不会在重新启用时把这五分钟全补一遍
        let next_fire: Option<DateTime<Utc>> =
            sqlx::query_scalar("SELECT next_fire_at FROM schedules WHERE task_id = $1")
                .bind(uuid::Uuid::from(task))
                .fetch_one(h.store.pool())
                .await
                .expect("查游标");
        assert!(
            next_fire.expect("有值") > start + Duration::minutes(5),
            "游标必须跟着走，否则重新启用时会补一大堆"
        );
    }
);

sched_test!(
    a_broken_expression_backs_off_instead_of_spinning_every_tick,
    |h| {
        let task = h.seed_task().await;
        let start = at("2026-09-08T10:00:00Z");
        h.seed_schedule(
            task,
            "*/1 * * * *",
            MisfirePolicy::FireOnce,
            OverlapPolicy::Allow,
            start,
        )
        .await;
        // 绕过 API 直接把表达式改坏，模拟"改库"或"升级后语义变了"
        sqlx::query("UPDATE schedules SET cron = 'not a cron' WHERE task_id = $1")
            .bind(uuid::Uuid::from(task))
            .execute(h.store.pool())
            .await
            .expect("改坏表达式");

        let scheduler = Scheduler::new(h.store.clone());
        let tick = scheduler
            .tick(start + Duration::seconds(1))
            .await
            .expect("轮询");
        assert_eq!(tick.processed, 1);
        assert!(tick.created.is_empty());

        // 下一个 tick 不应该再领到它——否则日志会被刷满
        let tick = scheduler
            .tick(start + Duration::seconds(2))
            .await
            .expect("轮询");
        assert_eq!(
            tick.processed, 0,
            "坏配置必须退避，不能每个 tick 都重试一次"
        );
    }
);

sched_test!(
    jitter_delays_the_claim_without_moving_the_recorded_fire_point,
    |h| {
        let task = h.seed_task().await;
        let start = at("2026-09-08T10:00:00Z");
        h.store
            .create_schedule(NewSchedule {
                workspace_id: h.workspace,
                task_id: task,
                cron: "0 * * * *".into(),
                timezone: "UTC".into(),
                misfire: MisfirePolicy::FireOnce,
                overlap: OverlapPolicy::Allow,
                jitter_s: 45,
                enabled: true,
                next_fire_at: start + Duration::hours(1),
                next_claim_at: start + Duration::hours(1),
            })
            .await
            .expect("建配置");

        let scheduler = Scheduler::new(h.store.clone());
        scheduler
            .tick(start + Duration::hours(1) + Duration::seconds(1))
            .await
            .expect("轮询");

        // 写进 runs.fire_at 的必须是整点：UNIQUE(schedule_id, fire_at) 全靠它
        let runs = h.runs_of(task).await;
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].0, start + Duration::hours(1));

        let (fire, claim): (DateTime<Utc>, DateTime<Utc>) =
            sqlx::query_as("SELECT next_fire_at, next_claim_at FROM schedules WHERE task_id = $1")
                .bind(uuid::Uuid::from(task))
                .fetch_one(h.store.pool())
                .await
                .expect("查游标");
        assert_eq!(fire, start + Duration::hours(2), "规范触发点不能被抖动挪动");
        assert!((0..=45).contains(&(claim - fire).num_seconds()));
    }
);

sched_test!(
    a_schedule_in_a_dst_timezone_fires_once_on_the_repeated_hour,
    |h| {
        // America/New_York 2026-11-01 的 01:00–02:00 出现两次。
        // 每年秋天多跑一个 run 是很难查的那种 bug。
        let task = h.seed_task().await;
        let cron = CronSchedule::parse("30 1 * * *", "America/New_York").expect("解析");
        let before = at("2026-10-31T12:00:00Z");
        let first = cron.next_after(before).expect("有触发点");

        h.store
            .create_schedule(NewSchedule {
                workspace_id: h.workspace,
                task_id: task,
                cron: "30 1 * * *".into(),
                timezone: "America/New_York".into(),
                misfire: MisfirePolicy::FireAll,
                overlap: OverlapPolicy::Allow,
                jitter_s: 0,
                enabled: true,
                next_fire_at: first,
                next_claim_at: first,
            })
            .await
            .expect("建配置");

        let scheduler = Scheduler::new(h.store.clone());
        // 一路轮询过整个秋回当天
        let mut now = before;
        let end = at("2026-11-02T12:00:00Z");
        while now < end {
            scheduler.tick(now).await.expect("轮询");
            now += Duration::minutes(10);
        }

        let runs = h.runs_of(task).await;
        assert_eq!(
            runs.len(),
            2,
            "10/31 和 11/1 各一次，重复的小时不能多跑：{runs:?}"
        );
    }
);
