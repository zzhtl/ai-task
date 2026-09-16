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

/// 审计查询的过滤条件。
///
/// 收成一个结构体是因为它们总是一起传，而且 `target` 的两个分量
/// 挨着传很容易写反。
#[derive(Debug, Default, Clone, Copy)]
pub struct AuditFilter<'a> {
    /// 只看某个类型的对象，如 `host` / `rule`。
    pub target_kind: Option<&'a str>,
    /// 再收窄到某一个具体对象。给了它就必须同时给 `target_kind`，调用方保证。
    pub target_id: Option<&'a str>,
    /// 只看这些动作。
    ///
    /// **和 `search` 取并集，不是交集。** 它们是同一次搜索的两种表达：
    /// 界面上的动作名是中文（「触发执行」），库里存的是 `run.trigger`，
    /// 中文只存在于前端的映射表里。前端把命中的动作码一起送过来，
    /// 这里要的是「文本匹配**或**动作命中」——写成 AND 的话搜中文永远是空的。
    /// `None` 表示不参与筛选。
    pub actions: Option<&'a [String]>,
    /// 文本搜索。和 `actions` 取并集。
    pub search: Option<&'a str>,
    /// 上一页最后一条的 `(ts, id)`。
    pub cursor: Option<(chrono::DateTime<chrono::Utc>, i64)>,
}

/// 转义 LIKE 的元字符。
///
/// 不转义的话，用户搜一个 `%` 就等于匹配全部——看起来像"搜什么都出来"，
/// 而不是"这个查询有问题"。
fn escape_like(raw: &str) -> String {
    raw.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
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
    /// 审计流水。
    ///
    /// **筛选在这里做，不在客户端做。** 之前是硬拉 300 条回浏览器再过滤——
    /// 一旦实例忙起来，"最近 300 条里恰好没有"和"根本没发生过"在界面上
    /// 长得一模一样，而审计恰恰是不能这样糊弄的东西。
    pub async fn list_audit(
        &self,
        workspace_id: WorkspaceId,
        filter: AuditFilter<'_>,
        limit: i64,
    ) -> Result<Vec<AuditRecord>, StoreError> {
        let AuditFilter {
            target_kind,
            target_id,
            actions,
            search,
            cursor,
        } = filter;
        // 文本搜索顺带覆盖操作人：界面上显示的是人名/邮箱，
        // 而库里存的是 actor_id，不 JOIN 的话"按人找"就搜不到。
        let pattern = search.map(|q| format!("%{}%", escape_like(q)));

        let rows = sqlx::query(
            "SELECT a.* FROM audit_log a
             LEFT JOIN users u ON u.id = a.actor_id
             WHERE a.workspace_id = $1
               AND ($2::text IS NULL OR a.target_kind = $2)
               AND ($3::text IS NULL OR a.target_id = $3)
               -- actions 和 search 取并集，不是交集。理由写在 AuditFilter 上：
               -- 中文动作名只存在于前端的映射表里，写成 AND 的话搜中文永远是空的。
               AND (
                    ($4::text[] IS NULL AND $5::text IS NULL)
                    OR ($4::text[] IS NOT NULL AND a.action = ANY($4))
                    OR ($5::text IS NOT NULL AND (
                         a.action ILIKE $5 ESCAPE '\\'
                      OR a.target_kind ILIKE $5 ESCAPE '\\'
                      OR a.target_id ILIKE $5 ESCAPE '\\'
                      OR u.email ILIKE $5 ESCAPE '\\'
                      OR u.display_name ILIKE $5 ESCAPE '\\'
                      OR a.before::text ILIKE $5 ESCAPE '\\'
                      OR a.after::text ILIKE $5 ESCAPE '\\'))
               )
               AND ($6::timestamptz IS NULL OR (a.ts, a.id) < ($6, $7))
             ORDER BY a.ts DESC, a.id DESC
             LIMIT $8",
        )
        .bind(uuid::Uuid::from(workspace_id))
        .bind(target_kind)
        .bind(target_id)
        .bind(actions)
        .bind(pattern)
        .bind(cursor.map(|(ts, _)| ts))
        .bind(cursor.map(|(_, id)| id))
        .bind(limit.clamp(1, 200))
        .fetch_all(self.pool())
        .await?;
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
