//! 连接池与库级维护。

use std::time::Duration;

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{PgPool, Row};

/// 数据库访问句柄。克隆代价等同克隆一个 `Arc`。
#[derive(Debug, Clone)]
pub struct Store {
    pool: PgPool,
}

/// 连接池参数。
#[derive(Debug, Clone)]
pub struct StoreConfig {
    pub url: String,
    /// 池上限。
    ///
    /// 默认取 `cpu 核数 × 2`，不是「越大越好」：超过这个量级后并发争用会让吞吐
    /// **下降**，而且这个数还要乘以副本数才是数据库真正承受的连接数。
    /// 池耗尽表现为延迟上升而不是报错，直到 acquire 超时才显形。
    pub max_connections: u32,
    pub min_connections: u32,
    pub acquire_timeout: Duration,
    pub connect_timeout: Duration,
}

impl Default for StoreConfig {
    fn default() -> Self {
        let cores = std::thread::available_parallelism()
            .map(std::num::NonZeroUsize::get)
            .unwrap_or(4);
        Self {
            url: String::new(),
            max_connections: u32::try_from(cores * 2).unwrap_or(8).clamp(4, 32),
            min_connections: 1,
            acquire_timeout: Duration::from_secs(10),
            connect_timeout: Duration::from_secs(5),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error(
        "数据库连接失败：{0}。检查 AI_TASK_DATABASE_URL 是否指向可达的 PostgreSQL，以及库是否已创建"
    )]
    Connect(#[source] sqlx::Error),

    #[error("迁移失败：{0}。同一份 migrations 已经在别处以不同内容跑过时会出现校验和不匹配")]
    Migrate(#[from] sqlx::migrate::MigrateError),

    #[error("查询失败：{0}")]
    Query(#[from] sqlx::Error),

    #[error("{what} {id} 不存在")]
    NotFound { what: &'static str, id: String },

    #[error("{what} 与 {id} 冲突")]
    Conflict { what: &'static str, id: String },

    /// 库里的数据解不出来。这是**设计错误**（多半是线上类型出现了破坏性变更），
    /// 不是运行时意外，必须显式炸出来——悄悄跳过会让事件回放得出一个错误的状态。
    #[error("{what} 的持久化数据已损坏或与当前类型不兼容：{detail}")]
    Corrupt { what: &'static str, detail: String },
}

impl Store {
    /// 建池并探活。
    pub async fn connect(config: &StoreConfig) -> Result<Self, StoreError> {
        let options: PgConnectOptions = config.url.parse().map_err(StoreError::Connect)?;
        let pool = PgPoolOptions::new()
            .max_connections(config.max_connections)
            .min_connections(config.min_connections)
            .acquire_timeout(config.acquire_timeout)
            // 长事务会挡住 VACUUM，进而导致膨胀。这里只是兜底，
            // 真正的约束是「事务里不做任何网络 IO」。
            .idle_timeout(Duration::from_secs(600))
            .connect_with(options)
            .await
            .map_err(StoreError::Connect)?;

        tracing::info!(
            max_connections = config.max_connections,
            "已连接 PostgreSQL"
        );
        Ok(Self { pool })
    }

    /// 跑迁移，然后确保分区就绪。
    pub async fn migrate(&self) -> Result<(), StoreError> {
        crate::MIGRATOR.run(&self.pool).await?;
        let created = self.ensure_partitions(3).await?;
        tracing::info!(created, "迁移完成，分区已就绪");
        Ok(())
    }

    /// 确保未来 `months_ahead` 个月的 `run_events` / `run_metrics` 分区存在。
    ///
    /// 由运行时每天调一次。必须**提前**建好：往非空的 DEFAULT 分区上 ATTACH
    /// 新分区要全扫它并持 ACCESS EXCLUSIVE 锁，那会把所有写入堵住。
    pub async fn ensure_partitions(&self, months_ahead: i32) -> Result<i32, StoreError> {
        let row = sqlx::query("SELECT ai_task_ensure_partitions($1) AS created")
            .bind(months_ahead)
            .fetch_one(&self.pool)
            .await?;
        Ok(row.try_get("created")?)
    }

    /// 探活。`readyz` 用。
    pub async fn ping(&self) -> Result<(), StoreError> {
        sqlx::query("SELECT 1").execute(&self.pool).await?;
        Ok(())
    }

    #[must_use]
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }
}

/// 事务是不是因为可串行化冲突或死锁失败的。
///
/// 这两种不是 bug 而是**预期结果**，正确的反应是重跑整个事务（不是重跑单条语句）。
/// 40001 = serialization_failure，40P01 = deadlock_detected。
#[must_use]
pub fn is_retryable(err: &sqlx::Error) -> bool {
    matches!(
        err.as_database_error().and_then(|e| e.code()).as_deref(),
        Some("40001" | "40P01")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pool_size_follows_core_count_within_sane_bounds() {
        let c = StoreConfig::default();
        assert!(
            (4..=32).contains(&c.max_connections),
            "池大小 {} 越界；200 连接的池是症状不是配置",
            c.max_connections
        );
    }

    #[test]
    fn serialization_failures_are_classified_as_retryable() {
        // sqlx::Error 没法在单测里凭空造出带 SQLSTATE 的数据库错误，
        // 这里只锁住「非数据库错误不可重试」这一半，另一半由集成测试覆盖。
        assert!(!is_retryable(&sqlx::Error::RowNotFound));
    }
}
