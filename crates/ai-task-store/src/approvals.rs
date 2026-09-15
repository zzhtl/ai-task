//! 人工审批。
//!
//! 两个来源：`approval` 节点（编排里显式的门），以及策略判决 `effect: ask`
//! （工具调用被挂起等人点头）。两者共用一张表和同一套决策接口。
//!
//! **超时的默认动作是拒绝。**审批门的全部意义就在于「没人点头就不做」；
//! 超时放行等于把它变成一个会延迟 15 分钟的空操作。

use ai_task_proto::{ApprovalId, NodeKey, RuleId, RunId, UserId, WorkspaceId};
use chrono::{DateTime, Utc};
use sqlx::Row as _;

use crate::{Store, StoreError};

#[derive(Debug, Clone)]
pub struct NewApproval {
    pub run_id: RunId,
    /// `None` 表示这是策略挂起的工具调用，不属于某个 `approval` 节点。
    pub node_key: Option<NodeKey>,
    pub title: String,
    /// 结构化意图。审批卡片渲染的是它，不是一段自然语言。
    pub intent: serde_json::Value,
    /// 触发本次审批的策略规则。走 `approval` 节点时为 `None`。
    pub rule_id: Option<RuleId>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct Approval {
    pub id: ApprovalId,
    pub run_id: RunId,
    pub node_key: Option<String>,
    pub title: String,
    pub intent: serde_json::Value,
    pub rule_id: Option<RuleId>,
    pub requested_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub decided_at: Option<DateTime<Utc>>,
    pub decided_by: Option<UserId>,
    /// `None` 表示还没决策。
    pub approved: Option<bool>,
    pub reason: Option<String>,
}

impl Approval {
    /// 已经有结论了吗（含超时自动拒绝）。
    #[must_use]
    pub fn is_decided(&self) -> bool {
        self.decided_at.is_some()
    }
}

/// 一次决策的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecisionOutcome {
    /// 决策已记下。
    Recorded { approved: bool },
    /// 已经有人先决策过了。返回既有结论，不覆盖。
    ///
    /// 两个人同时点「批准」和「拒绝」是必然会发生的；谁先到算谁的，
    /// 后到的那个必须看到实际生效的是什么，而不是以为自己说了算。
    AlreadyDecided { approved: bool },
    /// 这条审批不存在，或者不属于这个 workspace。
    NotFound,
}

impl Store {
    /// 创建一条待决审批。
    pub async fn create_approval(&self, new: NewApproval) -> Result<ApprovalId, StoreError> {
        let id = ApprovalId::new();
        sqlx::query(
            "INSERT INTO approvals (id, run_id, node_key, title, intent, rule_id, expires_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(uuid::Uuid::from(id))
        .bind(uuid::Uuid::from(new.run_id))
        .bind(new.node_key.as_ref().map(NodeKey::as_str))
        .bind(&new.title)
        .bind(&new.intent)
        .bind(new.rule_id.map(uuid::Uuid::from))
        .bind(new.expires_at)
        .execute(self.pool())
        .await?;
        Ok(id)
    }

    pub async fn get_approval(
        &self,
        workspace_id: WorkspaceId,
        id: ApprovalId,
    ) -> Result<Option<Approval>, StoreError> {
        // 连表 runs 是为了 workspace 隔离：approvals 自己没有 workspace_id，
        // 不连的话拿到别的租户的 approval id 就能读到它的意图内容
        let row = sqlx::query(
            "SELECT a.* FROM approvals a
             JOIN runs r ON r.id = a.run_id
             WHERE a.id = $1 AND r.workspace_id = $2",
        )
        .bind(uuid::Uuid::from(id))
        .bind(uuid::Uuid::from(workspace_id))
        .fetch_optional(self.pool())
        .await?;
        row.as_ref().map(approval_from_row).transpose()
    }

    /// 本 workspace 里所有待决的审批，最早过期的排在前面。
    ///
    /// **过期的直接在这里滤掉，不靠"读之前先写一次"。**
    /// 之前每次 GET /approvals 都要先跑一遍全 workspace 的
    /// `UPDATE ... RETURNING` 把过期的收掉——一个每 5 秒被前端打一次的只读接口，
    /// 每次都在写库。真正的收尾（写决策事件、放掉挂起的工具调用）
    /// 由维护任务和审批等待循环负责，那才是需要副作用的地方。
    pub async fn pending_approvals(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<Vec<Approval>, StoreError> {
        let rows = sqlx::query(
            "SELECT a.* FROM approvals a
             JOIN runs r ON r.id = a.run_id
             WHERE r.workspace_id = $1 AND a.decided_at IS NULL AND a.expires_at > now()
             ORDER BY a.expires_at",
        )
        .bind(uuid::Uuid::from(workspace_id))
        .fetch_all(self.pool())
        .await?;
        rows.iter().map(approval_from_row).collect()
    }

    /// 人工决策。
    ///
    /// `WHERE decided_at IS NULL` 让并发的两次决策只有一次能写进去——
    /// 这个竞争在真实使用里很常见（两个人同时看到卡片），靠应用层
    /// 「先读再写」是挡不住的。
    pub async fn decide_approval(
        &self,
        workspace_id: WorkspaceId,
        id: ApprovalId,
        approved: bool,
        decided_by: Option<UserId>,
        reason: Option<&str>,
    ) -> Result<DecisionOutcome, StoreError> {
        let updated = sqlx::query(
            "UPDATE approvals a
             SET decided_at = now(), approved = $3, decided_by = $4, reason = $5
             FROM runs r
             WHERE a.id = $1 AND r.id = a.run_id AND r.workspace_id = $2
               AND a.decided_at IS NULL
             RETURNING a.approved",
        )
        .bind(uuid::Uuid::from(id))
        .bind(uuid::Uuid::from(workspace_id))
        .bind(approved)
        .bind(decided_by.map(uuid::Uuid::from))
        .bind(reason)
        .fetch_optional(self.pool())
        .await?;

        if updated.is_some() {
            return Ok(DecisionOutcome::Recorded { approved });
        }
        // 没更新到：要么本来就不存在，要么已经决策过了。两者要分开报。
        match self.get_approval(workspace_id, id).await? {
            Some(existing) => Ok(DecisionOutcome::AlreadyDecided {
                approved: existing.approved.unwrap_or(false),
            }),
            None => Ok(DecisionOutcome::NotFound),
        }
    }

    /// 把已过期且仍未决策的审批一律标为拒绝，返回被处理的条目。
    ///
    /// 由等待方自己调用即可：审批只在有人等的时候才有意义，
    /// 没人等的过期审批留在库里不影响任何东西。
    pub async fn expire_approvals(&self, now: DateTime<Utc>) -> Result<Vec<Approval>, StoreError> {
        let rows = sqlx::query(
            "UPDATE approvals
             SET decided_at = $1, approved = false,
                 reason = COALESCE(reason, '超时未决，按拒绝处理')
             WHERE decided_at IS NULL AND expires_at <= $1
             RETURNING *",
        )
        .bind(now)
        .fetch_all(self.pool())
        .await?;
        rows.iter().map(approval_from_row).collect()
    }

    /// 等待方轮询用：只取结论，不取意图内容。
    pub async fn approval_verdict(
        &self,
        id: ApprovalId,
    ) -> Result<Option<(bool, Option<String>, Option<UserId>)>, StoreError> {
        let row = sqlx::query(
            "SELECT approved, reason, decided_by FROM approvals
             WHERE id = $1 AND decided_at IS NOT NULL",
        )
        .bind(uuid::Uuid::from(id))
        .fetch_optional(self.pool())
        .await?;
        let Some(row) = row else { return Ok(None) };
        Ok(Some((
            row.try_get::<Option<bool>, _>("approved")?.unwrap_or(false),
            row.try_get("reason")?,
            row.try_get::<Option<uuid::Uuid>, _>("decided_by")?
                .map(UserId),
        )))
    }
}

impl Store {
    /// 用户的展示名。审批事件里要写清是谁点的头。
    pub async fn user_display_name(&self, id: UserId) -> Result<Option<String>, StoreError> {
        let row = sqlx::query("SELECT display_name FROM users WHERE id = $1")
            .bind(uuid::Uuid::from(id))
            .fetch_optional(self.pool())
            .await?;
        row.map(|row| row.try_get("display_name").map_err(StoreError::from))
            .transpose()
    }

    /// 一条审批的 run 与过期时间。等待方重连时用它恢复上下文。
    pub async fn approval_deadline(
        &self,
        id: ApprovalId,
    ) -> Result<Option<(RunId, DateTime<Utc>)>, StoreError> {
        let row = sqlx::query("SELECT run_id, expires_at FROM approvals WHERE id = $1")
            .bind(uuid::Uuid::from(id))
            .fetch_optional(self.pool())
            .await?;
        row.map(|row| Ok((RunId(row.try_get("run_id")?), row.try_get("expires_at")?)))
            .transpose()
    }

    /// run 属于哪个 workspace。
    pub async fn workspace_of_run(&self, run_id: RunId) -> Result<WorkspaceId, StoreError> {
        let row = sqlx::query("SELECT workspace_id FROM runs WHERE id = $1")
            .bind(uuid::Uuid::from(run_id))
            .fetch_one(self.pool())
            .await?;
        Ok(WorkspaceId(row.try_get("workspace_id")?))
    }
}

fn approval_from_row(row: &sqlx::postgres::PgRow) -> Result<Approval, StoreError> {
    Ok(Approval {
        id: ApprovalId(row.try_get("id")?),
        run_id: RunId(row.try_get("run_id")?),
        node_key: row.try_get("node_key")?,
        title: row.try_get("title")?,
        intent: row.try_get("intent")?,
        rule_id: row.try_get::<Option<uuid::Uuid>, _>("rule_id")?.map(RuleId),
        requested_at: row.try_get("requested_at")?,
        expires_at: row.try_get("expires_at")?,
        decided_at: row.try_get("decided_at")?,
        decided_by: row
            .try_get::<Option<uuid::Uuid>, _>("decided_by")?
            .map(UserId),
        approved: row.try_get("approved")?,
        reason: row.try_get("reason")?,
    })
}
