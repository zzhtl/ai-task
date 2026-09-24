//! ai-task 服务入口。

mod assets;
mod bus;
mod config;
mod error;
mod extract;
mod hook;
mod idempotency;
mod middleware;
mod observability;
mod routes;
mod state;
mod supervisor;

use std::sync::Arc;
use std::time::Duration;

use ai_task_exec::claude_code::ClaudeCodeExecutor;
use ai_task_proto::WorkspaceId;
use ai_task_runtime::{HookSettings, RunEngine, Scheduler, maintenance, reap_orphaned_runs};
use ai_task_store::Store;
use anyhow::Context as _;
use clap::Parser as _;
use sqlx::Row as _;

use crate::bus::EventBus;
use crate::config::AppConfig;
use crate::state::AppState;

/// 命令行入口。
#[derive(Debug, clap::Parser)]
#[command(name = "ai-task", version, about = "AI 执行控制平面")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    #[command(flatten)]
    serve: AppConfig,
}

#[derive(Debug, clap::Subcommand)]
enum Command {
    /// Claude Code 的 PreToolUse hook：把工具调用交给策略层判决。
    ///
    /// 由各个 run 工作目录里的 hook 配置自动调用，不需要人手敲。
    PolicyHook(hook::HookArgs),

    /// 远端工具代理（MCP server，跑在 stdin/stdout 上）。
    ///
    /// 由各个 run 工作目录里的 mcp 配置自动拉起，不需要人手敲。
    /// 这个进程里**没有任何主机凭据**：它只能请中心代为执行。
    McpProxy(McpProxyArgs),
}

#[derive(Debug, clap::Args)]
struct McpProxyArgs {
    /// 中心的 `/internal/remote/call` 地址。
    #[arg(long)]
    endpoint: String,
    #[arg(long)]
    run_id: uuid::Uuid,
    /// 目标主机由这个节点的任务定义决定。代理不能自己选主机。
    #[arg(long)]
    node_key: String,
    #[arg(long, hide_env_values = true)]
    token: String,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // hook 是个短命子进程：不连数据库、不起服务，判完就退。
    //
    // **必须在建立 tokio 运行时之前处理。** hook 里用的是 `reqwest::blocking`，
    // 它内部自带一个运行时；在异步上下文里 drop 它会 panic
    // （"Cannot drop a runtime in a context where blocking is not allowed"），
    // 而 PreToolUse hook 失败会被 CLI 当成"无决策"放行——那是 fail-open，
    // 等于策略层整个失效。
    if let Some(Command::PolicyHook(args)) = &cli.command {
        hook::run(args);
    }

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("建立 tokio 运行时失败")?;

    match cli.command {
        // 代理不连数据库、不起 HTTP 服务，只是一条 stdio 上的转发管道
        Some(Command::McpProxy(args)) => runtime.block_on(async move {
            ai_task_mcp::serve_stdio(ai_task_mcp::ProxyConfig {
                endpoint: args.endpoint,
                token: args.token,
                run_id: ai_task_proto::RunId(args.run_id),
                node_key: args.node_key,
            })
            .await
        }),
        // PolicyHook 在建运行时之前就处理掉了，走不到这里
        Some(Command::PolicyHook(_)) | None => runtime.block_on(serve(cli.serve)),
    }
}

async fn serve(config: AppConfig) -> anyhow::Result<()> {
    observability::init(&config.log_filter);

    config
        .validate()
        .map_err(|detail| anyhow::anyhow!(detail))?;

    // 加密密钥要在**碰数据库之前**装载：缺了就直接退出。
    // 服务照常起来、只是写不了主机凭据的话，问题会推迟到第一次加机器时才暴露，
    // 那时候更难判断该不该"先明文存着"。
    let key = config.encryption_key.as_deref().ok_or_else(|| {
        anyhow::anyhow!(
            "缺少 AI_TASK_ENCRYPTION_KEY（64 位十六进制）。\n\
             主机私钥必须加密落库，没有密钥就没有安全的降级选项。\n\
             生成一个：openssl rand -hex 32"
        )
    })?;
    ai_task_store::crypto::init(key).map_err(|detail| anyhow::anyhow!("{detail}"))?;

    let store = Store::connect(&config.store_config())
        .await
        .context("连接数据库失败。先起库：docker compose -f deploy/docker-compose.yml up -d")?;

    if config.auto_migrate {
        store.migrate().await.context("跑迁移失败")?;
    }

    let workspace_id = ensure_default_workspace(&store)
        .await
        .context("初始化默认 workspace 失败")?;

    // 上次进程是被 kill 掉的话，会留下一批停在 running 的 run。
    // 它们的状态没丢（事件日志还在），但执行进程已经不在了，只能收尾。
    let reaped = reap_orphaned_runs(&store, &config.worker_id)
        .await
        .context("收尾中断的 run 失败")?;
    if reaped > 0 {
        tracing::warn!(reaped, "上次运行留下的 run 已标记失败，可以手动重跑");
    }

    tokio::fs::create_dir_all(&config.workdir_root)
        .await
        .with_context(|| format!("创建工作目录 {} 失败", config.workdir_root.display()))?;

    let bus = EventBus::new();
    bus.spawn_pg_listener(store.clone());

    // 内部接口的令牌每次启动重新生成，只写进各个 run 工作目录里的 hook 配置。
    // 没有它，同机上的任何进程都能替 run 批准工具调用。
    let internal_token: String = {
        use rand::Rng as _;
        let mut bytes = [0u8; 32];
        rand::rng().fill(&mut bytes);
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    };

    // hook 要能重新拉起本程序，所以取自己的绝对路径
    let program = std::env::current_exe().context("取不到自身可执行文件路径")?;
    let engine = RunEngine::new(
        store.clone(),
        Arc::new(ClaudeCodeExecutor::new(&config.claude_binary)),
        config.workdir_root.clone(),
        config.worker_id.clone(),
    )
    .with_hook(HookSettings {
        program,
        endpoint: format!("http://{}/internal/policy/decide", config.listen),
        remote_endpoint: format!("http://{}/internal/remote/call", config.listen),
        token: internal_token.clone(),
    })
    .with_host_exec(config.host_exec());

    let listener = tokio::net::TcpListener::bind(config.listen)
        .await
        .with_context(|| format!("监听 {} 失败", config.listen))?;
    let addr = listener.local_addr().unwrap_or(config.listen);
    let shutdown_timeout = config.shutdown_timeout();

    let state = AppState::new(
        store.clone(),
        bus,
        engine,
        crate::state::Settings {
            workspace_id,
            internal_token: internal_token.clone(),
            host_exec: Some(config.host_exec()),
            require_auth: config.require_auth,
            min_password_len: config.min_password_len,
            max_concurrent_runs: config.max_concurrent_runs(),
        },
    );

    // 调度器、维护任务和 HTTP 服务共享一个关停信号
    let shutdown = tokio_util::sync::CancellationToken::new();
    // 每日维护。少了它 `ensure_partitions` 就只在 migrate 时跑过一次，
    // 连续运行约四个月后所有事件会落进 DEFAULT 兜底分区——那之后很难便宜地补救。
    maintenance::spawn(store.clone(), shutdown.clone());
    {
        let supervisor = state.supervisor.clone();
        Scheduler::new(store).spawn(
            config.scheduler_interval(),
            move |workspace_id, run_id| supervisor.spawn(workspace_id, run_id),
            shutdown.clone(),
        );
    }

    let service = routes::build_service(state);

    tracing::info!(%addr, %workspace_id, "ai-task 已启动");
    axum::serve(
        listener,
        axum::ServiceExt::<axum::extract::Request>::into_make_service(service),
    )
    .with_graceful_shutdown(shutdown_signal(shutdown_timeout))
    .await
    .context("HTTP 服务异常退出")?;

    shutdown.cancel();

    tracing::info!("已退出");
    Ok(())
}

/// 取（必要时建）默认 workspace。
///
/// M1 没有认证，全进程共用一个 workspace。schema 从第一天就带着
/// `workspace_id`，等 M5 接入认证时换成从会话里取即可——补一个 workspace 便宜，
/// 事后给几十张表加租户列很贵。
async fn ensure_default_workspace(store: &Store) -> anyhow::Result<WorkspaceId> {
    if let Some(row) = sqlx::query("SELECT id FROM workspaces ORDER BY created_at LIMIT 1")
        .fetch_optional(store.pool())
        .await?
    {
        return Ok(WorkspaceId(row.try_get("id")?));
    }

    let id = WorkspaceId::new();
    // ON CONFLICT DO NOTHING：多副本同时冷启动时只会有一个插进去
    sqlx::query("INSERT INTO workspaces (id, name) VALUES ($1, 'default') ON CONFLICT DO NOTHING")
        .bind(uuid::Uuid::from(id))
        .execute(store.pool())
        .await?;

    let row = sqlx::query("SELECT id FROM workspaces ORDER BY created_at LIMIT 1")
        .fetch_one(store.pool())
        .await?;
    Ok(WorkspaceId(row.try_get("id")?))
}

/// 收到 SIGINT / SIGTERM 后开始优雅退出。
///
/// 超时是必须的：在途请求可能卡在一个不返回的下游上，没有上限的话容器编排
/// 最后还是会 SIGKILL，反而少了收尾的机会。
async fn shutdown_signal(timeout: Duration) {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            Err(err) => {
                tracing::warn!(error = %err, "注册 SIGTERM 失败，只能靠 Ctrl-C 退出");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {}
        () = terminate => {}
    }

    tracing::info!(
        timeout_s = timeout.as_secs(),
        "收到退出信号，等待在途请求结束"
    );
    tokio::spawn(async move {
        tokio::time::sleep(timeout).await;
        tracing::error!("优雅退出超时，强制结束");
        std::process::exit(1);
    });
}
