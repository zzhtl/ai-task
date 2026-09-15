//! 资源采样从 agent 一路落到 `run_metrics` 的端到端验证。
//!
//! 需要 `AI_TASK_TEST_DATABASE_URL` 和一个已构建的 `target/debug/ai-task-agent`，
//! 缺任何一个就跳过。

use ai_task_proto::{DagSpec, RunEventBody, RunStatus, TriggerKind, WorkspaceId};
use ai_task_runtime::{Command, HostExecConfig, run_command};
use ai_task_store::{NewRun, NewTask, PendingEvent, Store, StoreConfig};
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
        sqlx::query(AssertSqlSafe(format!(r#"CREATE DATABASE "{db}""#)))
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

    async fn drop_db(self) {
        drop(self.store);
        if let Ok(mut admin) = PgConnection::connect(&self.admin_url).await {
            let sql = format!(r#"DROP DATABASE IF EXISTS "{}" WITH (FORCE)"#, self.db);
            let _ = sqlx::query(AssertSqlSafe(sql)).execute(&mut admin).await;
        }
    }
}

fn agent_path() -> Option<std::path::PathBuf> {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/ai-task-agent");
    path.exists().then_some(path)
}

#[tokio::test]
async fn every_sample_the_agent_emits_reaches_the_database() {
    let Some(harness) = Harness::create().await else {
        common::skip_or_fail(
            "every_sample_the_agent_emits_reaches_the_database",
            "未设 AI_TASK_TEST_DATABASE_URL",
        );
        return;
    };
    let Some(agent) = agent_path() else {
        // 这个测试要驱动真实的 agent 二进制。没建就跳过的话，"资源归因跑通了"
        // 和"资源归因根本没测"在日志里是同一行字。
        common::skip_or_fail(
            "every_sample_the_agent_emits_reaches_the_database",
            "target/debug/ai-task-agent 不存在，先 cargo build -p ai-task-agent",
        );
        harness.drop_db().await;
        return;
    };

    let (task, version) = harness
        .store
        .create_task(NewTask {
            workspace_id: harness.workspace,
            name: "metrics".into(),
            description: None,
            spec: DagSpec {
                nodes: vec![],
                edges: vec![],
                input_schema: None,
                budget_usd: None,
                timeout_s: None,
            },
            rules: vec![],
            rules_hash: "h".into(),
            enabled: true,
        })
        .await
        .expect("建任务");
    let run = harness
        .store
        .create_run(
            NewRun {
                workspace_id: harness.workspace,
                task_id: task.id,
                task_version_id: version.id,
                trigger: TriggerKind::Manual,
                dry_run: false,
                inputs: None,
                compare_to: None,
            },
            PendingEvent::run(RunEventBody::RunQueued {
                task_version_id: version.id,
                trigger: TriggerKind::Manual,
                inputs: None,
                dry_run: false,
            }),
        )
        .await
        .expect("建 run");

    // 跑满 5 秒。agent 是 1Hz 采样，落库端攒批 —— 攒批实现里
    // 一旦"顺手看看还有没有"用了会取走消息的接口，点数就会掉一半。
    let seconds = 5;
    let outcome = run_command(
        &harness.store,
        &HostExecConfig {
            local_agent: agent,
            remote_agent: None,
            known_hosts: "/dev/null".into(),
            host_key_policy: ai_task_exec::remote::HostKeyPolicy::Strict,
        },
        Command {
            workspace_id: harness.workspace,
            run_id: run.id,
            node_key: "burn",
            selector: None,
            command: &format!(
                "end=$(( $(date +%s) + {seconds} )); while [ $(date +%s) -lt $end ]; do :; done"
            ),
            cwd: "/tmp",
            timeout_ms: 60_000,
            limits: None,
            roots: &["/tmp".to_owned()],
        },
    )
    .await
    .expect("执行");

    assert_eq!(outcome.result.exit_code, Some(0));

    let rows = harness.store.read_metrics(run.id).await.expect("读采样");
    assert_eq!(
        rows.len(),
        outcome.samples,
        "落库条数要和上报条数一致：攒批时丢点在这里会露出来"
    );
    // 1Hz 跑 5 秒。给一条的余量吸收调度抖动。
    assert!(
        rows.len() >= seconds - 1,
        "跑了 {seconds} 秒只落了 {} 条采样，中间有点被丢了",
        rows.len()
    );
    assert!(rows.iter().all(|(key, _)| key == "burn"));

    // CPU 是累计量，必须单调不减；跌回去说明采的不是同一个 cgroup
    let cpu: Vec<i64> = rows.iter().map(|(_, s)| s.cpu_usec).collect();
    assert!(
        cpu.windows(2).all(|w| w[1] >= w[0]),
        "累计 CPU 必须单调不减：{cpu:?}"
    );
    assert!(
        *cpu.last().expect("有采样") >= 3_000_000,
        "跑满 {seconds} 秒的忙循环至少该记到 3 CPU-秒，实际 {} 微秒",
        cpu.last().copied().unwrap_or_default()
    );

    // run 本身不该被采样影响
    let state = harness
        .store
        .get_run(harness.workspace, run.id)
        .await
        .expect("读 run");
    assert_eq!(state.status, RunStatus::Queued);

    harness.drop_db().await;
}

#[tokio::test]
async fn a_command_shorter_than_the_sampling_interval_still_gets_one_point() {
    // 跑得比一个采样周期还快的命令，曲线不能是空的——否则界面上
    // "这个节点跑过吗"没有答案。
    let Some(harness) = Harness::create().await else {
        common::skip_or_fail(
            "a_command_shorter_than_the_sampling_interval_still_gets_one_point",
            "未设 AI_TASK_TEST_DATABASE_URL",
        );
        return;
    };
    let Some(agent) = agent_path() else {
        // 这个测试要驱动真实的 agent 二进制。没建就跳过的话，"资源归因跑通了"
        // 和"资源归因根本没测"在日志里是同一行字。
        common::skip_or_fail(
            "a_command_shorter_than_the_sampling_interval_still_gets_one_point",
            "target/debug/ai-task-agent 不存在，先 cargo build -p ai-task-agent",
        );
        harness.drop_db().await;
        return;
    };

    let (task, version) = harness
        .store
        .create_task(NewTask {
            workspace_id: harness.workspace,
            name: "quick".into(),
            description: None,
            spec: DagSpec {
                nodes: vec![],
                edges: vec![],
                input_schema: None,
                budget_usd: None,
                timeout_s: None,
            },
            rules: vec![],
            rules_hash: "h".into(),
            enabled: true,
        })
        .await
        .expect("建任务");
    let run = harness
        .store
        .create_run(
            NewRun {
                workspace_id: harness.workspace,
                task_id: task.id,
                task_version_id: version.id,
                trigger: TriggerKind::Manual,
                dry_run: false,
                inputs: None,
                compare_to: None,
            },
            PendingEvent::run(RunEventBody::RunQueued {
                task_version_id: version.id,
                trigger: TriggerKind::Manual,
                inputs: None,
                dry_run: false,
            }),
        )
        .await
        .expect("建 run");

    let outcome = run_command(
        &harness.store,
        &HostExecConfig {
            local_agent: agent,
            remote_agent: None,
            known_hosts: "/dev/null".into(),
            host_key_policy: ai_task_exec::remote::HostKeyPolicy::Strict,
        },
        Command {
            workspace_id: harness.workspace,
            run_id: run.id,
            node_key: "quick",
            selector: None,
            command: "true",
            cwd: "/tmp",
            timeout_ms: 30_000,
            limits: None,
            roots: &["/tmp".to_owned()],
        },
    )
    .await
    .expect("执行");

    assert_eq!(outcome.result.exit_code, Some(0));
    let rows = harness.store.read_metrics(run.id).await.expect("读采样");
    assert_eq!(rows.len(), 1, "秒退的命令也要有一条终态用量");

    harness.drop_db().await;
}
