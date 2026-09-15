//! 每日维护。
//!
//! 这些清理动作的代码一直都在，只是**没有任何东西调用它们**：
//!
//! - `ensure_partitions` 的注释写着「由运行时每天调用一次」，实际只在 migrate 时
//!   调过一次，`months_ahead = 3`。连续运行约四个月之后，新的事件就全落进
//!   `run_events_default` 兜底分区——而往非空的 DEFAULT 上 ATTACH 新分区
//!   需要全扫它并持 ACCESS EXCLUSIVE 锁。**烧掉之后很难便宜地补救，所以这条最急。**
//! - `purge_expired_sessions` 零调用方，过期会话永久堆积。
//! - `expire_approvals` 之前挂在读路径上（每次 GET /approvals、每 2 秒的长轮询各跑一次
//!   全 workspace 的 UPDATE）。挪到这里之后读路径只按 `expires_at` 过滤。

use std::time::Duration;

use ai_task_store::Store;
use tokio_util::sync::CancellationToken;

/// 跑一轮的间隔。日常清理不需要更密。
const INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

/// 提前建好几个月的分区。比 `ensure_partitions` 默认的 3 更宽松一点：
/// 维护任务万一停了，留出的缓冲期就是唯一的安全边际。
const MONTHS_AHEAD: i32 = 4;

/// 保留多少个月的事件与采样。超过的分区直接丢。
const RETAIN_MONTHS: i32 = 6;

/// 启动每日维护。返回的句柄在进程退出时随 `shutdown` 一起收尾。
pub fn spawn(store: Store, shutdown: CancellationToken) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        tracing::info!(interval_h = INTERVAL.as_secs() / 3600, "维护任务已启动");
        loop {
            // 先跑一轮再睡：进程刚起来时把上次停机期间积压的清理补上
            run_once(&store).await;
            tokio::select! {
                () = shutdown.cancelled() => break,
                () = tokio::time::sleep(INTERVAL) => {}
            }
        }
        tracing::info!("维护任务已停止");
    })
}

/// 一轮清理。**任何一步失败都不中断后面几步**——它们互不依赖，
/// 而且下一轮还会再来。中断的话一个偶发错误会顺带让别的清理也停掉。
async fn run_once(store: &Store) {
    match store.ensure_partitions(MONTHS_AHEAD).await {
        Ok(created) if created > 0 => tracing::info!(created, "新建分区"),
        Ok(_) => tracing::debug!("分区已就绪"),
        Err(err) => tracing::error!(error = %err, "建分区失败，下一轮重试"),
    }

    match store.drop_old_partitions(RETAIN_MONTHS).await {
        Ok(dropped) if dropped > 0 => {
            tracing::info!(dropped, retain_months = RETAIN_MONTHS, "丢弃过期分区")
        }
        Ok(_) => {}
        Err(err) => tracing::error!(error = %err, "丢弃过期分区失败，下一轮重试"),
    }

    match store.expire_approvals(chrono::Utc::now()).await {
        Ok(rows) if !rows.is_empty() => tracing::info!(expired = rows.len(), "收掉过期审批"),
        Ok(_) => {}
        Err(err) => tracing::error!(error = %err, "收过期审批失败，下一轮重试"),
    }

    match ai_task_store::idempotency::purge_expired(store.pool()).await {
        Ok(n) if n > 0 => tracing::info!(purged = n, "清理过期幂等键"),
        Ok(_) => {}
        Err(err) => tracing::error!(error = %err, "清理过期幂等键失败，下一轮重试"),
    }

    match store.purge_expired_sessions().await {
        Ok(n) if n > 0 => tracing::info!(purged = n, "清理过期会话"),
        Ok(_) => {}
        Err(err) => tracing::error!(error = %err, "清理过期会话失败，下一轮重试"),
    }
}
