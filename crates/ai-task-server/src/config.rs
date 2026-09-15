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

    /// 同时最多跑几个 run。
    ///
    /// 每个 run 会拉起一个 `claude` 子进程——它吃 CPU、吃内存，而且**花钱**。
    /// 之前这里没有上限：一次触发风暴、或者 cron 的 `fire_all` 补偿，
    /// 能把机器上的子进程数顶到没边。超出上限的 run 留在 `queued` 排队，
    /// 不拒绝——那正是现有状态机里已有的语义。
    #[arg(long, env = "AI_TASK_MAX_CONCURRENT_RUNS")]
    pub max_concurrent_runs: Option<usize>,

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

    /// 口令的最小长度。
    ///
    /// 默认 12：这个账号能让系统 SSH 到任意机器执行命令。
    /// **只有监听在回环上时才允许调低**——本机开发实例上逼着记一串长口令是
    /// 纯粹的摩擦，但那个理由在对外暴露时完全不成立，所以启动时会拒绝。
    #[arg(long, env = "AI_TASK_MIN_PASSWORD_LEN", default_value_t = 12)]
    pub min_password_len: usize,
}

/// 口令下限的默认值，同时也是对外暴露时不可放宽的底线。
pub const DEFAULT_MIN_PASSWORD_LEN: usize = 12;

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
    /// 并发上限。默认取 CPU 核数，封到 [1, 32]——
    /// 和连接池一个量级：每个 run 干活时都要用连接。
    pub fn max_concurrent_runs(&self) -> usize {
        self.max_concurrent_runs
            .unwrap_or_else(|| {
                std::thread::available_parallelism()
                    .map(std::num::NonZeroUsize::get)
                    .unwrap_or(4)
            })
            .clamp(1, 32)
    }

    pub fn scheduler_interval(&self) -> Duration {
        Duration::from_millis(self.scheduler_interval_ms.max(100))
    }

    /// 配置本身说不通的地方。启动时检查，不通就拒绝启动。
    ///
    /// 比"启动了但不安全"好：后者要等出事才被发现。
    pub fn validate(&self) -> Result<(), String> {
        if !self.listen.ip().is_loopback() && self.min_password_len < DEFAULT_MIN_PASSWORD_LEN {
            return Err(format!(
                "监听在 {} 却把口令下限调到了 {}。放宽下限的理由只在本机开发时成立，\n\
                 对外暴露时不成立。",
                self.listen, self.min_password_len
            ));
        }
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
    #[test]
    fn the_run_concurrency_cap_stays_in_a_sane_range() {
        // 没配就跟着核数走，但两头都要封住：0 会让所有 run 永远排队，
        // 而几百路并发意味着几百个 claude 子进程同时在花钱。
        let mut config = AppConfig::parse_from(["ai-task"]);
        assert!((1..=32).contains(&config.max_concurrent_runs()));

        config.max_concurrent_runs = Some(0);
        assert_eq!(config.max_concurrent_runs(), 1, "0 会让 run 永远起不来");

        config.max_concurrent_runs = Some(9999);
        assert_eq!(config.max_concurrent_runs(), 32);
    }

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
    fn the_password_floor_can_only_be_lowered_on_loopback() {
        // 本机开发实例上逼着记一串长口令是纯粹的摩擦；
        // 但那个理由在对外暴露时完全不成立。
        let dev = AppConfig::parse_from(["ai-task", "--min-password-len", "5"]);
        assert!(dev.validate().is_ok(), "回环上允许调低");

        let exposed = AppConfig::parse_from([
            "ai-task",
            "--listen",
            "0.0.0.0:8930",
            "--require-auth",
            "true",
            "--min-password-len",
            "5",
        ]);
        let err = exposed.validate().expect_err("对外暴露时必须拒绝");
        assert!(err.contains("口令下限"), "{err}");

        // 不动它的话，对外暴露照常可以起来
        assert!(
            AppConfig::parse_from([
                "ai-task",
                "--listen",
                "0.0.0.0:8930",
                "--require-auth",
                "true",
            ])
            .validate()
            .is_ok()
        );
        assert_eq!(
            AppConfig::parse_from(["ai-task"]).min_password_len,
            DEFAULT_MIN_PASSWORD_LEN
        );
    }

    #[test]
    fn pool_size_override_is_floored_at_one() {
        let c = AppConfig::parse_from(["ai-task", "--db-max-connections", "0"]);
        assert_eq!(c.store_config().max_connections, 1);
    }
}
