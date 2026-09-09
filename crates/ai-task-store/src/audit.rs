//! 审计日志。
//!
//! 记的是**人做了什么**，不是系统跑了什么。系统的行为在 `run_events` 里，
//! 那条流已经足够完整；这里补的是它答不了的问题：谁改了这条策略、
//! 谁批准了那次操作、谁把这台生产机加进来的。
//!
//! 写失败不能让业务失败。审计缺一条是可惜，但因为审计写不进去而拒绝一次
//! 合法的操作，会让人第一时间想把审计关掉。

use ai_task_proto::{UserId, WorkspaceId};
use chrono::{DateTime, Utc};
use sqlx::Row as _;

use crate::{Store, StoreError};

#[derive(Debug, Clone)]
pub struct AuditEntry {
    pub workspace_id: WorkspaceId,
    /// `None` 表示系统自身的动作（调度器触发、超时自动拒绝）。
    pub actor_id: Option<UserId>,
    /// 动作名，如 `rule.create` / `approval.decide` / `host.create`。
    /// 点分两段：前面是对象类型，后面是动词。
    pub action: &'static str,
    pub target_kind: &'static str,
    pub target_id: String,
    /// 改动前后。创建时 `before` 为 `None`，删除时 `after` 为 `None`。
    pub before: Option<serde_json::Value>,
    pub after: Option<serde_json::Value>,
    /// 关联到 HTTP 请求。排查时能把审计条目和访问日志对上。
    pub request_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AuditRecord {
    pub id: i64,
    pub actor_id: Option<UserId>,
    pub action: String,
    pub target_kind: String,
    pub target_id: String,
    pub before: Option<serde_json::Value>,
    pub after: Option<serde_json::Value>,
    pub request_id: Option<String>,
    pub ts: DateTime<Utc>,
}

impl Store {
    /// 记一条审计。
    ///
    /// 返回 `Result` 只是为了让调用方能记日志；**不要**因为它失败就中止业务。
    pub async fn audit(&self, entry: AuditEntry) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO audit_log
               (workspace_id, actor_id, action, target_kind, target_id, before, after, request_id)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(uuid::Uuid::from(entry.workspace_id))
        .bind(entry.actor_id.map(uuid::Uuid::from))
        .bind(entry.action)
        .bind(entry.target_kind)
        .bind(&entry.target_id)
        .bind(&entry.before)
        .bind(&entry.after)
        .bind(&entry.request_id)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// 最近的审计条目。
    pub async fn recent_audit(
        &self,
        workspace_id: WorkspaceId,
        target: Option<(&str, &str)>,
        limit: i64,
    ) -> Result<Vec<AuditRecord>, StoreError> {
        let rows = match target {
            Some((kind, id)) => {
                sqlx::query(
                    "SELECT * FROM audit_log
                     WHERE workspace_id = $1 AND target_kind = $2 AND target_id = $3
                     ORDER BY ts DESC LIMIT $4",
                )
                .bind(uuid::Uuid::from(workspace_id))
                .bind(kind)
                .bind(id)
                .bind(limit.clamp(1, 500))
                .fetch_all(self.pool())
                .await?
            }
            None => {
                sqlx::query(
                    "SELECT * FROM audit_log WHERE workspace_id = $1 ORDER BY ts DESC LIMIT $2",
                )
                .bind(uuid::Uuid::from(workspace_id))
                .bind(limit.clamp(1, 500))
                .fetch_all(self.pool())
                .await?
            }
        };
        rows.iter().map(audit_from_row).collect()
    }
}

fn audit_from_row(row: &sqlx::postgres::PgRow) -> Result<AuditRecord, StoreError> {
    Ok(AuditRecord {
        id: row.try_get("id")?,
        actor_id: row
            .try_get::<Option<uuid::Uuid>, _>("actor_id")?
            .map(UserId),
        action: row.try_get("action")?,
        target_kind: row.try_get("target_kind")?,
        target_id: row.try_get("target_id")?,
        before: row.try_get("before")?,
        after: row.try_get("after")?,
        request_id: row.try_get("request_id")?,
        ts: row.try_get("ts")?,
    })
}
