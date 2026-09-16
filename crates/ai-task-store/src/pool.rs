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

/// 配置类列表的硬上限。
///
/// 主机、用户、技能、定时这些都是人手配的，正常量级是几十。
/// 加上限不是为了翻页，是为了**不让一次失控的写入把列表接口拖垮**——
/// 没有 LIMIT 的查询在数据异常时会变成一次全表扫加一个巨大的 JSON 响应，
/// 而那正是最需要界面还能打开的时候。
pub const CONFIG_LIST_CAP: i64 = 500;

impl Store {
    /// 建池并探活。
    pub async fn connect(config: &StoreConfig) -> Result<Self, StoreError> {
        let options: PgConnectOptions = config.url.parse().map_err(StoreError::Connect)?;
        let connecting = PgPoolOptions::new()
            .max_connections(config.max_connections)
            .min_connections(config.min_connections)
            .acquire_timeout(config.acquire_timeout)
            // 长事务会挡住 VACUUM，进而导致膨胀。这里只是兜底，
            // 真正的约束是「事务里不做任何网络 IO」。
            .idle_timeout(Duration::from_secs(600))
            // 连接不要永久复用：长命连接会攒住旧的执行计划和临时表空间，
            // 而且换掉之后才能享受到数据库那边的配置变更。
            .max_lifetime(Duration::from_secs(30 * 60))
            .after_connect(|conn, _meta| {
                Box::pin(async move {
                    // 一条查询不该跑到天荒地老：慢查询在表现上和连接池耗尽
                    // 长得一样，设个上限才能在日志里把两者分开。
                    // 值取得比 acquire_timeout 宽：正常的重查询不该被误杀。
                    sqlx::query("SET statement_timeout = '30s'")
                        .execute(&mut *conn)
                        .await?;
                    // 事务里挂着不动会挡住 VACUUM。这是兜底，不是正常路径。
                    sqlx::query("SET idle_in_transaction_session_timeout = '60s'")
                        .execute(&mut *conn)
                        .await?;
                    // pg_stat_activity 里能一眼看出这些连接是谁的
                    sqlx::query("SET application_name = 'ai-task'")
                        .execute(&mut *conn)
                        .await?;
                    Ok(())
                })
            })
            .connect_with(options);

        // sqlx 0.9 的 PoolOptions 只有 acquire_timeout，没有 connect_timeout——
        // 所以这个配置项得自己兑现，否则库不可达时启动会一直挂到 acquire_timeout 才报错，
        // 而「连不上」和「池被占满」是两件需要分开诊断的事。
        let pool = tokio::time::timeout(config.connect_timeout, connecting)
            .await
            .map_err(|_| StoreError::Connect(sqlx::Error::PoolTimedOut))?
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
    /// 丢掉超过保留期的月度分区。返回丢掉的张数。
    ///
    /// 不做这件事的话分区数每月 +1 且无上限，而 SSE 的续传查询没有 `ts` 谓词，
    /// 每次都要 Merge Append 扫过所有分区。
    pub async fn drop_old_partitions(&self, retain_months: i32) -> Result<i32, StoreError> {
        let row = sqlx::query("SELECT ai_task_drop_old_partitions($1) AS dropped")
            .bind(retain_months)
            .fetch_one(&self.pool)
            .await?;
        Ok(row.try_get("dropped")?)
    }

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
    fn the_connect_timeout_is_shorter_than_the_acquire_timeout() {
        // 否则它永远不会先触发，等于没配：连不上和池占满会表现成同一种超时。
        let c = StoreConfig::default();
        assert!(
            c.connect_timeout < c.acquire_timeout,
            "connect_timeout {:?} 应当短于 acquire_timeout {:?}",
            c.connect_timeout,
            c.acquire_timeout
        );
    }
}
