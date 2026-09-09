//! 引擎的端到端测试：假执行器 → 事件日志 → 重放。
//!
//! 用假执行器而不是真跑 claude，是为了能把「执行器吐出某种事件序列时，run 应当
//! 落成什么状态」钉死，而且不花钱、CI 里能跑。真实 CLI 的事件形状由
//! `ai-task-exec` 的录制样本覆盖，两边合起来才完整。
//!
//! 需要 `AI_TASK_TEST_DATABASE_URL`，没设就跳过。

use std::sync::Arc;

use ai_task_core::RunState;
use ai_task_exec::{
    ExecError, ExecEvent, ExecHandle, ExecOutcome, ExecRequest, Executor, ToolOutcome,
};
use ai_task_proto::{
    NodeKey, NodeStatus, RunEventBody, RunStatus, TriggerKind, UsdMicros, WorkspaceId,
};
use ai_task_runtime::{RunEngine, reap_orphaned_runs};
use ai_task_store::{NewRun, NewTask, PendingEvent, RunRecord, Store, StoreConfig};
use sqlx::{AssertSqlSafe, Connection, PgConnection};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------- 假执行器

/// 按脚本回放事件的执行器。
struct ScriptedExecutor {
    script: Vec<ExecEvent>,
    /// 每条事件之间的间隔，用来测取消。
    delay: std::time::Duration,
    /// spawn 直接失败，模拟"目标上根本没有这个 CLI"。
    unstartable: bool,
}

impl ScriptedExecutor {
    fn new(script: Vec<ExecEvent>) -> Self {
        Self {
            script,
            delay: std::time::Duration::ZERO,
            unstartable: false,
        }
    }

    fn slow(script: Vec<ExecEvent>) -> Self {
        Self {
            script,
            delay: std::time::Duration::from_millis(200),
            unstartable: false,
        }
    }

    /// 起不来的执行器：CLI 没装、workdir 不在，都是这个形状。
    fn unstartable() -> Self {
        Self {
            script: Vec::new(),
            delay: std::time::Duration::ZERO,
            unstartable: true,
        }
    }
}

#[async_trait::async_trait]
impl Executor for ScriptedExecutor {
    fn name(&self) -> &'static str {
        "scripted"
    }

    fn command_line(&self, request: &ExecRequest) -> String {
        format!("scripted --node {}", request.node_key)
    }

    async fn spawn(&self, _request: ExecRequest) -> Result<ExecHandle, ExecError> {
        if self.unstartable {
            return Err(ExecError::MissingWorkdir("/nonexistent".into()));
        }
        let (tx, rx) = mpsc::channel(64);
        let cancel = CancellationToken::new();
        let script = self.script.clone();
        let delay = self.delay;
        let child_cancel = cancel.clone();

        tokio::spawn(async move {
            for event in script {
                if child_cancel.is_cancelled() {
                    let _ = tx.send(ExecEvent::Finished(ExecOutcome::Cancelled)).await;
                    return;
                }
                if !delay.is_zero() {
                    tokio::select! {
                        () = tokio::time::sleep(delay) => {}
                        () = child_cancel.cancelled() => {
                            let _ = tx.send(ExecEvent::Finished(ExecOutcome::Cancelled)).await;
                            return;
                        }
                    }
                }
                if tx.send(event).await.is_err() {
                    return;
                }
            }
        });

        Ok(ExecHandle { events: rx, cancel })
    }
}

fn started() -> ExecEvent {
    ExecEvent::Started {
        session_id: "sess-1".into(),
        model: "claude-haiku-4-5".into(),
        tools: vec!["Read".into(), "Glob".into(), "Grep".into()],
        cli_version: Some("2.1.263".into()),
    }
}

fn usage(cost: i64) -> ExecEvent {
    ExecEvent::Usage {
        model: "claude-haiku-4-5".into(),
        input_tokens: 959,
        output_tokens: 713,
        cache_read_tokens: 46_461,
        cache_creation_tokens: 12_200,
        cost_usd: UsdMicros(cost),
    }
}

// ---------------------------------------------------------------- 夹具

struct Harness {
    store: Store,
    workspace: WorkspaceId,
    _workdir: tempfile::TempDir,
    workdir_path: std::path::PathBuf,
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

        let workdir = tempfile::tempdir().expect("临时目录");
        let workdir_path = workdir.path().to_path_buf();
        Some(Self {
            store,
            workspace,
            _workdir: workdir,
            workdir_path,
            admin_url,
            db,
        })
    }

    fn engine(&self, executor: ScriptedExecutor) -> RunEngine {
        RunEngine::new(
            self.store.clone(),
            Arc::new(executor),
            self.workdir_path.clone(),
            "test-worker",
        )
    }

    async fn seed_run(&self) -> RunRecord {
        let (task, version) = self
            .store
            .create_task(NewTask {
                workspace_id: self.workspace,
                name: format!("t-{}", uuid::Uuid::new_v4().simple()),
                description: None,
                spec: spec(),
                rules: vec!["禁止删除文件".into()],
                rules_hash: "h1".into(),
                enabled: true,
            })
            .await
            .expect("建任务");

        self.store
            .create_run(
                NewRun {
                    workspace_id: self.workspace,
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
            .expect("建 run")
    }

    async fn replay(&self, run_id: ai_task_proto::RunId) -> RunState {
        let events = self
            .store
            .read_events_after(run_id, 0, 100_000)
            .await
            .expect("读事件");
        RunState::replay(&events).expect("重放")
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
        AiNode, DagSpec, ExecutorKind, NodeConfig, NodeSpec, OnFailure, RetryPolicy,
    };
    DagSpec {
        nodes: vec![NodeSpec {
            key: NodeKey::parse("probe").expect("key"),
            name: None,
            config: NodeConfig::Ai(AiNode {
                prompt: "统计行数".into(),
                executor: ExecutorKind::ClaudeCode,
                cli: None,
                model: Some("claude-haiku-4-5".into()),
                effort: None,
                skills: vec![],
                tools: vec![],
                max_turns: None,
                budget_usd: Some(UsdMicros(200_000)),
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

macro_rules! engine_test {
    ($name:ident, |$h:ident| $body:block) => {
        #[tokio::test]
        async fn $name() {
            let Some($h) = Harness::create().await else {
                eprintln!("跳过 {}：未设置 AI_TASK_TEST_DATABASE_URL", stringify!($name));
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

engine_test!(a_successful_run_is_fully_reconstructible_from_events, |h| {
    let run = h.seed_run().await;
    h.engine(ScriptedExecutor::new(vec![
        started(),
        ExecEvent::Thinking {
            text: "先找 .rs 文件".into(),
        },
        ExecEvent::ToolRequested {
            tool_use_id: "t1".into(),
            tool: "Glob".into(),
            input: serde_json::json!({"pattern": "*.rs"}),
        },
        ExecEvent::ToolCompleted {
            tool_use_id: "t1".into(),
            outcome: ToolOutcome::Ok,
            output_preview: "a.rs\nb.rs".into(),
        },
        ExecEvent::Text { text: "4".into() },
        usage(23_259),
        ExecEvent::Finished(ExecOutcome::Success {
            result: "4".into(),
            turns: 5,
        }),
    ]))
    .execute(h.workspace, run.id, CancellationToken::new())
    .await;

    // M1 的核心验收：只用 run_events 就能重建出终态
    let state = h.replay(run.id).await;
    assert_eq!(state.status, RunStatus::Succeeded);
    assert_eq!(state.cost, UsdMicros(23_259));

    let node = &state.nodes[&NodeKey("probe".into())];
    assert_eq!(node.status, NodeStatus::Succeeded);
    assert_eq!(node.tool_calls, 1);
    assert_eq!(node.output, Some(serde_json::json!(4)));

    // 投影列必须和重放结果一致
    let stored = h.store.get_run(h.workspace, run.id).await.expect("取 run");
    assert_eq!(stored.status, state.status);
    assert_eq!(stored.cost, state.cost);
    assert_eq!(stored.cli_version.as_deref(), Some("2.1.263"));
    assert_eq!(stored.output, Some(serde_json::json!(4)));
});

// `started_at` 曾经被终态前那次"顺手写 cli_version"的 start_run 调用覆盖成
// 当前时间，run 的耗时看起来只有几毫秒。诊断字段的写入不能碰生命周期时间戳。
engine_test!(
    started_at_reflects_when_execution_began_not_when_it_ended,
    |h| {
        let run = h.seed_run().await;
        let created = run.created_at;
        // 让执行占掉一点真实时间，好把"起止时间被压平"和"确实很快"区分开
        h.engine(ScriptedExecutor::slow(vec![
            started(),
            ExecEvent::Text { text: "4".into() },
            usage(1_000),
            ExecEvent::Finished(ExecOutcome::Success {
                result: "4".into(),
                turns: 1,
            }),
        ]))
        .execute(h.workspace, run.id, CancellationToken::new())
        .await;

        let stored = h.store.get_run(h.workspace, run.id).await.expect("取 run");
        let started = stored.started_at.expect("必须有开始时间");
        let finished = stored.finished_at.expect("必须有结束时间");

        assert!(started >= created, "开始不能早于创建");
        assert!(finished > started, "结束必须晚于开始");
        let duration = finished - started;
        assert!(
            duration.num_milliseconds() >= 500,
            "执行至少占了 4×200ms，实测耗时却只有 {duration}——started_at 被覆盖了"
        );
        assert_eq!(
            stored.cli_version.as_deref(),
            Some("2.1.263"),
            "诊断字段仍要写进去"
        );
    }
);

engine_test!(
    budget_exhaustion_fails_the_run_but_keeps_the_spend_on_record,
    |h| {
        let run = h.seed_run().await;
        h.engine(ScriptedExecutor::new(vec![
            started(),
            usage(5_519),
            ExecEvent::Finished(ExecOutcome::BudgetExceeded),
        ]))
        .execute(h.workspace, run.id, CancellationToken::new())
        .await;

        let state = h.replay(run.id).await;
        assert_eq!(state.status, RunStatus::Failed);
        assert_eq!(
            state.cost,
            UsdMicros(5_519),
            "熔断掉的钱已经花出去了，账上不能当没发生"
        );
        let stored = h.store.get_run(h.workspace, run.id).await.expect("取 run");
        assert!(
            stored.error.as_deref().is_some_and(|e| e.contains("预算")),
            "错误信息要说清是被预算掐掉的：{:?}",
            stored.error
        );
    }
);

engine_test!(a_denied_tool_call_is_recorded_as_a_policy_decision, |h| {
    let run = h.seed_run().await;
    h.engine(ScriptedExecutor::new(vec![
        started(),
        ExecEvent::ToolRequested {
            tool_use_id: "t1".into(),
            tool: "Bash".into(),
            input: serde_json::json!({"command": "rm -rf /"}),
        },
        ExecEvent::PermissionDenied {
            tool_use_id: "t1".into(),
            tool: "Bash".into(),
            reason: "Contains command_substitution".into(),
        },
        ExecEvent::ToolCompleted {
            tool_use_id: "t1".into(),
            outcome: ToolOutcome::Error,
            output_preview: "denied".into(),
        },
        usage(1_000),
        ExecEvent::Finished(ExecOutcome::Success {
            result: "ok".into(),
            turns: 2,
        }),
    ]))
    .execute(h.workspace, run.id, CancellationToken::new())
    .await;

    let state = h.replay(run.id).await;
    let node = &state.nodes[&NodeKey("probe".into())];
    assert_eq!(node.denied_tool_calls, 1, "被拒的调用要能在界面上标出来");

    // 审计链路上必须能查到"哪次调用被拒、为什么"
    let events = h
        .store
        .read_events_after(run.id, 0, 1_000)
        .await
        .expect("读事件");
    let denial = events
        .iter()
        .find(|e| e.kind() == "policy_decided")
        .expect("必须留下策略判决");
    let RunEventBody::PolicyDecided {
        reason,
        tool_use_id,
        ..
    } = &denial.body
    else {
        panic!("类型不对");
    };
    assert_eq!(tool_use_id, "t1");
    assert!(reason.contains("command_substitution"));
});

engine_test!(
    cancelling_a_run_stops_the_executor_and_lands_a_cancelled_status,
    |h| {
        let run = h.seed_run().await;
        let engine = h.engine(ScriptedExecutor::slow(vec![
            started(),
            ExecEvent::Text { text: "1".into() },
            ExecEvent::Text { text: "2".into() },
            ExecEvent::Text { text: "3".into() },
            usage(1_000),
            ExecEvent::Finished(ExecOutcome::Success {
                result: "done".into(),
                turns: 9,
            }),
        ]));

        let cancel = CancellationToken::new();
        let handle = {
            let cancel = cancel.clone();
            let workspace = h.workspace;
            let run_id = run.id;
            tokio::spawn(async move { engine.execute(workspace, run_id, cancel).await })
        };

        tokio::time::sleep(std::time::Duration::from_millis(350)).await;
        cancel.cancel();
        tokio::time::timeout(std::time::Duration::from_secs(10), handle)
            .await
            .expect("取消后引擎必须及时收敛")
            .expect("引擎任务");

        let state = h.replay(run.id).await;
        assert_eq!(state.status, RunStatus::Cancelled);
        assert_eq!(
            state.nodes[&NodeKey("probe".into())].status,
            NodeStatus::Cancelled
        );
    }
);

engine_test!(
    an_executor_that_never_finishes_still_lands_a_terminal_status,
    |h| {
        // 执行器流断了但没给终态。run 不能永远停在 running——
        // 那会把调度器的并发额度占死。
        let run = h.seed_run().await;
        h.engine(ScriptedExecutor::new(vec![
            started(),
            ExecEvent::Text { text: "…".into() },
        ]))
        .execute(h.workspace, run.id, CancellationToken::new())
        .await;

        let state = h.replay(run.id).await;
        assert!(state.status.is_terminal());
        assert_eq!(state.status, RunStatus::Failed);
    }
);

engine_test!(restart_reaps_orphaned_runs_and_preserves_their_spend, |h| {
    // 模拟「执行到一半服务被 kill」：run 停在 running，事件里已经有花掉的钱
    let run = h.seed_run().await;
    h.store
        .start_run(
            run.id,
            Some("2.1.263"),
            None,
            PendingEvent::run(RunEventBody::RunStarted {
                worker: "old".into(),
            }),
        )
        .await
        .expect("开始");
    h.store
        .append_events(
            run.id,
            &[PendingEvent::new(
                Some("probe".into()),
                RunEventBody::Usage {
                    model: "claude-haiku-4-5".into(),
                    input_tokens: 100,
                    output_tokens: 10,
                    cache_read_tokens: 0,
                    cache_creation_tokens: 0,
                    cost_usd: UsdMicros(7_000),
                },
            )],
        )
        .await
        .expect("写用量");

    let reaped = reap_orphaned_runs(&h.store, "new-worker")
        .await
        .expect("收尾");
    assert_eq!(reaped, 1);

    let state = h.replay(run.id).await;
    assert_eq!(state.status, RunStatus::Failed);
    assert_eq!(
        state.cost,
        UsdMicros(7_000),
        "重启前已经花掉的钱不能在账上消失"
    );
    let stored = h.store.get_run(h.workspace, run.id).await.expect("取 run");
    assert!(
        stored
            .error
            .as_deref()
            .is_some_and(|e| e.contains("无法续跑")),
        "要说清为什么失败：{:?}",
        stored.error
    );

    // 收尾过的 run 不该被再次拉起
    assert_eq!(
        reap_orphaned_runs(&h.store, "new-worker")
            .await
            .expect("再扫"),
        0
    );
});

engine_test!(
    the_command_is_on_the_record_even_when_the_cli_never_starts,
    |h| {
        let run = h.seed_run().await;
        h.engine(ScriptedExecutor::unstartable())
            .execute(h.workspace, run.id, CancellationToken::new())
            .await;

        let events = h
            .store
            .read_events_after(run.id, 0, 100_000)
            .await
            .expect("读事件");

        // 起不来的时候恰恰是最需要看这条命令的时候。发在 spawn 之后
        // 就等于永远看不到——这个断言是那个设计决定的全部理由。
        let invoked: Vec<(i64, &str)> = events
            .iter()
            .filter_map(|e| match &e.body {
                RunEventBody::AgentInvoked { command, .. } => Some((e.seq, command.as_str())),
                _ => None,
            })
            .collect();
        assert_eq!(invoked.len(), 1, "没留下命令：{events:#?}");
        assert!(
            invoked[0].1.contains("--node probe"),
            "命令不像是这个节点的：{}",
            invoked[0].1
        );

        let failed_at = events
            .iter()
            .find(|e| matches!(e.body, RunEventBody::NodeFinished { .. }))
            .map(|e| e.seq)
            .expect("有 node_finished");
        assert!(invoked[0].0 < failed_at, "命令得排在失败之前");

        let state = h.replay(run.id).await;
        assert_eq!(state.status, RunStatus::Failed);
    }
);

engine_test!(the_recorded_command_is_the_one_that_actually_ran, |h| {
    let run = h.seed_run().await;
    h.engine(ScriptedExecutor::new(vec![
        started(),
        ExecEvent::Text { text: "4".into() },
        ExecEvent::Finished(ExecOutcome::Success {
            result: "4".into(),
            turns: 1,
        }),
    ]))
    .execute(h.workspace, run.id, CancellationToken::new())
    .await;

    let events = h
        .store
        .read_events_after(run.id, 0, 100_000)
        .await
        .expect("读事件");
    let invoked = events
        .iter()
        .find_map(|e| match &e.body {
            RunEventBody::AgentInvoked { command, ran_on } => Some((command.clone(), *ran_on)),
            _ => None,
        })
        .expect("有 agent_invoked");
    assert!(invoked.0.contains("--node probe"), "{}", invoked.0);
    // 中心驱动：进程跑在中心，不该指向任何主机
    assert_eq!(invoked.1, None);
});
