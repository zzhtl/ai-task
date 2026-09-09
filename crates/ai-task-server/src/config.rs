//! 进程配置。CLI 参数与环境变量是同一套模型。

use std::net::SocketAddr;
use std::time::Duration;

use clap::Parser;

#[derive(Debug, Clone, Parser)]
#[command(name = "ai-task", version, about = "AI 执行控制平面")]
pub struct AppConfig {
    /// 监听地址。
    #[arg(long, env = "AI_TASK_LISTEN", default_value = "127.0.0.1:8930")]
    pub listen: SocketAddr,

    /// PostgreSQL 连接串。
    #[arg(
        long,
        env = "AI_TASK_DATABASE_URL",
        default_value = "postgres://ai_task@127.0.0.1:55432/ai_task"
    )]
    pub database_url: String,

    /// 连接池上限。默认由 CPU 核数推导。
    ///
    /// 不要往大了调：超过 `核数 × 2` 之后并发争用会让吞吐**下降**，
    /// 而且这个数要乘以副本数才是数据库真正承受的连接数。
    #[arg(long, env = "AI_TASK_DB_MAX_CONNECTIONS")]
    pub db_max_connections: Option<u32>,

    /// 启动时自动跑迁移。生产上想把迁移和发布解耦时置 false。
    #[arg(long, env = "AI_TASK_AUTO_MIGRATE", default_value_t = true, action = clap::ArgAction::Set)]
    pub auto_migrate: bool,

    /// 日志过滤表达式。
    #[arg(
        long,
        env = "AI_TASK_LOG",
        default_value = "info,ai_task=debug,tower_http=warn,sqlx=warn"
    )]
    pub log_filter: String,

    /// 优雅退出的等待上限（秒）。超时后强制结束在途请求。
    #[arg(long, env = "AI_TASK_SHUTDOWN_TIMEOUT_S", default_value_t = 20)]
    pub shutdown_timeout_s: u64,

    /// claude 可执行文件。不在 PATH 里时给绝对路径。
    #[arg(long, env = "AI_TASK_CLAUDE_BINARY", default_value = "claude")]
    pub claude_binary: String,

    /// run 工作目录的根。每个 run 在下面开一个以 run id 命名的子目录。
    #[arg(
        long,
        env = "AI_TASK_WORKDIR_ROOT",
        default_value = "/tmp/ai-task/runs"
    )]
    pub workdir_root: std::path::PathBuf,

    /// 本副本的标识，进事件日志。多副本部署时给不同的值才能看出是谁跑的。
    #[arg(long, env = "AI_TASK_WORKER_ID", default_value = "worker-1")]
    pub worker_id: String,

    /// 调度器轮询间隔（毫秒）。
    ///
    /// 决定定时任务最多晚多久触发。给小了只是多几次很轻的查询
    /// （部分索引 + SKIP LOCKED，实测 0.06ms）。
    #[arg(long, env = "AI_TASK_SCHEDULER_INTERVAL_MS", default_value_t = 1000)]
    pub scheduler_interval_ms: u64,

    /// 凭据加密密钥，64 位十六进制（32 字节）。
    ///
    /// **没有它服务拒绝启动。**「先明文存着，以后再加」的数据库一旦落地就再也
    /// 改不回来了——已经存进去的私钥不会自己变成密文。
    /// 生成：`openssl rand -hex 32`。
    #[arg(long, env = "AI_TASK_ENCRYPTION_KEY", hide_env_values = true)]
    pub encryption_key: Option<String>,

    /// 本机执行用的 agent 二进制。
    ///
    /// shell 节点在本机也走 agent，这样本机执行同样有资源归因和上限。
    /// `cargo build -p ai-task-agent --release` 之后指向 `target/release/ai-task-agent`。
    #[arg(long, env = "AI_TASK_LOCAL_AGENT", default_value = "ai-task-agent")]
    pub local_agent: std::path::PathBuf,

    /// 推送到远端的 musl 静态二进制。不配的话远端 shell 节点会明确报错。
    ///
    /// `cargo build -p ai-task-agent --profile agent-release --target x86_64-unknown-linux-musl`
    #[arg(long, env = "AI_TASK_REMOTE_AGENT")]
    pub remote_agent: Option<std::path::PathBuf>,

    /// SSH known_hosts 文件。
    #[arg(long, env = "AI_TASK_KNOWN_HOSTS")]
    pub known_hosts: Option<std::path::PathBuf>,

    /// 首次连接一台新主机时是否记下它的密钥（TOFU）。
    ///
    /// 默认开：否则每加一台机器都要手动 ssh 一次。**密钥变了永远是拒绝**，
    /// 那才是中间人攻击的信号；这个开关只影响"从来没见过"的主机。
    #[arg(long, env = "AI_TASK_SSH_ACCEPT_NEW", default_value_t = true, action = clap::ArgAction::Set)]
    pub ssh_accept_new: bool,

    /// 是否强制登录。
    ///
    /// 默认**关**：默认监听只在回环上，单人自托管时逼着先建账号是纯粹的摩擦。
    /// **一旦把 `--listen` 改到回环之外就必须打开**——这个服务能 SSH 到任意
    /// 机器执行命令，没有认证等于把那台机器交出去。启动时会检查这一点。
    #[arg(long, env = "AI_TASK_REQUIRE_AUTH", default_value_t = false, action = clap::ArgAction::Set)]
    pub require_auth: bool,
}

impl AppConfig {
    #[must_use]
    pub fn store_config(&self) -> ai_task_store::StoreConfig {
        let mut config = ai_task_store::StoreConfig {
            url: self.database_url.clone(),
            ..Default::default()
        };
        if let Some(max) = self.db_max_connections {
            config.max_connections = max.max(1);
        }
        config
    }

    #[must_use]
    pub fn shutdown_timeout(&self) -> Duration {
        Duration::from_secs(self.shutdown_timeout_s)
    }

    #[must_use]
    pub fn scheduler_interval(&self) -> Duration {
        Duration::from_millis(self.scheduler_interval_ms.max(100))
    }

    /// 配置本身说不通的地方。启动时检查，不通就拒绝启动。
    ///
    /// 比"启动了但不安全"好：后者要等出事才被发现。
    pub fn validate(&self) -> Result<(), String> {
        if !self.listen.ip().is_loopback() && !self.require_auth {
            return Err(format!(
                "监听在 {} 却没开认证。这个服务能 SSH 到任意机器执行命令，\n\
                 对外暴露必须配合 AI_TASK_REQUIRE_AUTH=true。",
                self.listen
            ));
        }
        Ok(())
    }

    #[must_use]
    pub fn host_exec(&self) -> ai_task_runtime::HostExecConfig {
        ai_task_runtime::HostExecConfig {
            local_agent: self.local_agent.clone(),
            remote_agent: self.remote_agent.clone(),
            known_hosts: self.known_hosts.clone().unwrap_or_else(default_known_hosts),
            host_key_policy: if self.ssh_accept_new {
                ai_task_exec::remote::HostKeyPolicy::AcceptNew
            } else {
                ai_task_exec::remote::HostKeyPolicy::Strict
            },
        }
    }
}

fn default_known_hosts() -> std::path::PathBuf {
    std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_default()
        .join(".ssh/known_hosts")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_bind_to_loopback_not_all_interfaces() {
        let c = AppConfig::parse_from(["ai-task"]);
        assert!(
            c.listen.ip().is_loopback(),
            "默认监听不能是 0.0.0.0：这个服务能 SSH 到任意机器执行命令，\
             对外暴露必须是显式选择"
        );
    }

    #[test]
    fn a_new_host_is_trusted_on_first_sight_but_a_changed_key_never_is() {
        // TOFU 只放宽"没见过"这一档；密钥变了在任何配置下都是拒绝
        let c = AppConfig::parse_from(["ai-task"]);
        assert!(matches!(
            c.host_exec().host_key_policy,
            ai_task_exec::remote::HostKeyPolicy::AcceptNew
        ));
        let strict = AppConfig::parse_from(["ai-task", "--ssh-accept-new", "false"]);
        assert!(matches!(
            strict.host_exec().host_key_policy,
            ai_task_exec::remote::HostKeyPolicy::Strict
        ));
    }

    #[test]
    fn binding_beyond_loopback_without_auth_is_refused_at_startup() {
        // "启动了但不安全"要等出事才被发现，不如直接不让它起来
        let exposed = AppConfig::parse_from(["ai-task", "--listen", "0.0.0.0:8930"]);
        let err = exposed.validate().expect_err("必须拒绝");
        assert!(err.contains("AI_TASK_REQUIRE_AUTH"), "{err}");

        let guarded = AppConfig::parse_from([
            "ai-task",
            "--listen",
            "0.0.0.0:8930",
            "--require-auth",
            "true",
        ]);
        assert!(guarded.validate().is_ok());

        // 回环上不强制：单人自托管时逼着先建账号是纯粹的摩擦
        assert!(AppConfig::parse_from(["ai-task"]).validate().is_ok());
    }

    #[test]
    fn pool_size_override_is_floored_at_one() {
        let c = AppConfig::parse_from(["ai-task", "--db-max-connections", "0"]);
        assert_eq!(c.store_config().max_connections, 1);
    }
}
