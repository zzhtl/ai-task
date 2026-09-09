//! 集成测试的共用夹具。
//!
//! 需要 `AI_TASK_TEST_DATABASE_URL` 指向一个可建库的 PostgreSQL：
//!
//! ```sh
//! docker compose -f deploy/docker-compose.yml up -d
//! AI_TASK_TEST_DATABASE_URL=postgres://ai_task@127.0.0.1:55432/postgres \
//!   cargo test -p ai-task-store
//! ```
//!
//! 没设这个变量时测试会打印提示后跳过——本仓库不用 testcontainers。
//! 每个测试用例拿一个**独立的一次性数据库**，跑完就删，所以并行执行也互不干扰。

#![allow(dead_code)]

use ai_task_proto::{DagSpec, RunEventBody, TaskId, TaskVersionId, TriggerKind, WorkspaceId};
use ai_task_store::{NewRun, NewTask, PendingEvent, RunRecord, Store, StoreConfig};
use sqlx::{AssertSqlSafe, Connection, PgConnection};

/// 一个装好 schema、带默认 workspace 的一次性数据库。
pub struct Fixture {
    pub store: Store,
    pub workspace: WorkspaceId,
    admin_url: String,
    db: String,
}

impl Fixture {
    /// 返回 `None` 表示没配环境变量，调用方应当跳过。
    pub async fn create() -> Option<Self> {
        let admin_url = std::env::var("AI_TASK_TEST_DATABASE_URL").ok()?;
        let db = format!("ai_task_test_{}", uuid::Uuid::new_v4().simple());

        let mut admin = PgConnection::connect(&admin_url)
            .await
            .expect("连接管理库失败");
        // db 名来自 `Uuid::new_v4().simple()`，只可能是 32 个十六进制字符，
        // 注入面为零；sqlx 0.9 要求显式声明已审计。
        let create = format!(r#"CREATE DATABASE "{db}""#);
        sqlx::query(AssertSqlSafe(create))
            .execute(&mut admin)
            .await
            .expect("建库失败");

        let store = Store::connect(&StoreConfig {
            url: swap_database(&admin_url, &db),
            ..Default::default()
        })
        .await
        .expect("连接一次性库失败");
        store.migrate().await.expect("迁移失败");

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

    /// 另开一个 workspace，用于跨租户隔离的测试。
    pub async fn new_workspace(&self) -> WorkspaceId {
        let id = WorkspaceId::new();
        sqlx::query("INSERT INTO workspaces (id, name) VALUES ($1, 'other')")
            .bind(uuid::Uuid::from(id))
            .execute(self.store.pool())
            .await
            .expect("建 workspace");
        id
    }

    /// 建一个任务并触发一个待执行的 run。
    pub async fn seed_run(&self) -> RunRecord {
        let (task, version) = self.seed_task().await;
        self.store
            .create_run(
                NewRun {
                    workspace_id: self.workspace,
                    task_id: task,
                    task_version_id: version,
                    trigger: TriggerKind::Manual,
                    dry_run: false,
                    inputs: None,
                    compare_to: None,
                },
                PendingEvent::run(RunEventBody::RunQueued {
                    task_version_id: version,
                    trigger: TriggerKind::Manual,
                    inputs: None,
                    dry_run: false,
                }),
            )
            .await
            .expect("建 run")
    }

    pub async fn seed_task(&self) -> (TaskId, TaskVersionId) {
        let (task, version) = self
            .store
            .create_task(NewTask {
                workspace_id: self.workspace,
                // 任务名有唯一约束，用随机后缀免得同一个夹具里建第二个就撞
                name: format!("task-{}", uuid::Uuid::new_v4().simple()),
                description: None,
                spec: minimal_spec(),
                rules: vec![],
                rules_hash: "h0".into(),
                enabled: true,
            })
            .await
            .expect("建任务");
        (task.id, version.id)
    }

    pub async fn cleanup(self) {
        // 先关掉池，否则 DROP DATABASE 会被自己的连接挡住
        self.store.pool().close().await;
        if let Ok(mut admin) = PgConnection::connect(&self.admin_url).await {
            let sql = format!(r#"DROP DATABASE IF EXISTS "{}" WITH (FORCE)"#, self.db);
            let _ = sqlx::query(AssertSqlSafe(sql)).execute(&mut admin).await;
        }
    }
}

/// 一个只有单节点、能通过 DAG 校验的最小 spec。
pub fn minimal_spec() -> DagSpec {
    use ai_task_proto::{
        AiNode, ExecutorKind, NodeConfig, NodeKey, NodeSpec, OnFailure, RetryPolicy,
    };
    DagSpec {
        nodes: vec![NodeSpec {
            key: NodeKey::parse("probe").expect("key"),
            name: None,
            config: NodeConfig::Ai(AiNode {
                prompt: "hi".into(),
                executor: ExecutorKind::ClaudeCode,
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

/// 把连接串里的库名换掉，其余部分（用户、口令、host、查询参数）保持原样。
fn swap_database(url: &str, db: &str) -> String {
    match url.rsplit_once('/') {
        Some((prefix, tail)) => {
            let query = tail.find('?').map_or("", |i| &tail[i..]);
            format!("{prefix}/{db}{query}")
        }
        None => format!("{url}/{db}"),
    }
}

/// 捕获 panic，好在断言失败时也能把临时库删掉。
pub async fn catch<F: std::future::Future<Output = ()>>(
    fut: std::panic::AssertUnwindSafe<F>,
) -> Result<(), Box<dyn std::any::Any + Send>> {
    use futures_util::FutureExt;
    fut.catch_unwind().await
}

/// 声明一个需要真实数据库的测试。没配环境变量就跳过。
#[macro_export]
macro_rules! db_test {
    ($name:ident, |$f:ident| $body:block) => {
        #[tokio::test]
        async fn $name() {
            let Some($f) = $crate::common::Fixture::create().await else {
                eprintln!("跳过 {}：未设置 AI_TASK_TEST_DATABASE_URL", stringify!($name));
                return;
            };
            let outcome = $crate::common::catch(std::panic::AssertUnwindSafe(async $body)).await;
            $f.cleanup().await;
            if let Err(payload) = outcome {
                std::panic::resume_unwind(payload);
            }
        }
    };
}
