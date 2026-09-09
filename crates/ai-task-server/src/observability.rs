//! tracing 初始化。

use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// 兜底过滤：给的表达式不合法时不能让进程静默丢日志。
const FALLBACK: &str = "info,ai_task=debug,tower_http=warn,sqlx=warn";

pub fn init(filter: &str) {
    let env_filter = EnvFilter::try_new(filter).unwrap_or_else(|err| {
        eprintln!("日志过滤表达式 {filter:?} 不合法（{err}），回退到 {FALLBACK:?}");
        EnvFilter::new(FALLBACK)
    });

    tracing_subscriber::registry()
        .with(env_filter)
        .with(
            tracing_subscriber::fmt::layer()
                .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr()))
                .with_target(true)
                .with_thread_ids(false)
                .with_file(false)
                // 日志走 stderr：stdout 留给未来可能的结构化输出管道
                .with_writer(std::io::stderr),
        )
        .init();
}
