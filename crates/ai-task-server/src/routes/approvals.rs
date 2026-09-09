//! 人工审批的对外接口。
//!
//! 审批卡片渲染的是 `intent`——结构化的意图，不是一段自然语言。
//! 要让人在几秒内判断"这该不该做"，卡片上必须是具体的：
//! 哪台机器、跑什么命令、改哪些文件。

use ai_task_proto::{ApprovalId, Page};
use ai_task_store::DecisionOutcome;
use axum::extract::{Path, State};
use axum::{Json, response::IntoResponse};
use serde::{Deserialize, Serialize};

use crate::error::AppError;
use crate::state::AppState;

#[derive(Debug, Serialize)]
pub struct ApprovalCard {
    pub id: String,
    pub run_id: String,
    pub node_key: Option<String>,
    pub title: String,
    /// 结构化意图。前端按字段渲染，不做自然语言解析。
    pub intent: serde_json::Value,
    /// 触发本次审批的策略规则。`approval` 节点为 `None`。
    pub rule_id: Option<String>,
    pub requested_at: chrono::DateTime<chrono::Utc>,
    pub expires_at: chrono::DateTime<chrono::Utc>,
    /// 还剩多少秒。已过期为 0。前端不用自己算，也就不会因为时钟不同步而算错。
    pub expires_in_s: i64,
}

impl From<ai_task_store::Approval> for ApprovalCard {
    fn from(a: ai_task_store::Approval) -> Self {
        Self {
            id: a.id.to_string(),
            run_id: a.run_id.to_string(),
            node_key: a.node_key,
            title: a.title,
            intent: a.intent,
            rule_id: a.rule_id.map(|id| id.to_string()),
            requested_at: a.requested_at,
            expires_in_s: (a.expires_at - chrono::Utc::now()).num_seconds().max(0),
            expires_at: a.expires_at,
        }
    }
}

/// `GET /api/v1/approvals`
///
/// 只返回待决的。已决策的审批属于 run 的历史，从事件流里读。
pub async fn list(State(state): State<AppState>) -> Result<Json<Page<ApprovalCard>>, AppError> {
    // 先把过期的收掉，否则界面上会一直挂着一张点不动的卡片
    let _ = state.store.expire_approvals(chrono::Utc::now()).await;
    let pending = state.store.pending_approvals(state.workspace_id).await?;
    Ok(Json(Page {
        items: pending.into_iter().map(ApprovalCard::from).collect(),
        next_cursor: None,
    }))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Decision {
    pub approved: bool,
    /// 拒绝理由会作为 `tool_result` 回给模型，让它自己调整。
    /// 写清楚"为什么不行"比写"不行"有用得多。
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct DecisionResult {
    /// 实际生效的结论。**并发决策时它可能和你提交的不一样**——
    /// 谁先到算谁的，这个字段告诉你实际是什么。
    pub approved: bool,
    /// 你的这次提交是不是生效的那一次。
    pub was_first: bool,
}

/// `POST /api/v1/approvals/{id}/decide`
pub async fn decide(
    State(state): State<AppState>,
    Path(id): Path<uuid::Uuid>,
    Json(body): Json<Decision>,
) -> Result<impl IntoResponse, AppError> {
    let outcome = state
        .store
        .decide_approval(
            state.workspace_id,
            ApprovalId(id),
            body.approved,
            // M5 还没接认证，决策人先留空。事件日志里已经记下了这次决策，
            // 接上认证后这里换成会话里的 user id。
            None,
            body.reason.as_deref(),
        )
        .await?;

    match outcome {
        DecisionOutcome::Recorded { approved } => {
            // 审批是最需要留痕的一类动作：事后要能回答"这次危险操作是谁点的头"
            state
                .audit(
                    "approval.decide",
                    "approval",
                    id.to_string(),
                    None,
                    Some(serde_json::json!({
                        "approved": approved,
                        "reason": body.reason,
                    })),
                )
                .await;
            Ok(Json(DecisionResult {
                approved,
                was_first: true,
            }))
        }
        // 已经有结论了不是错误：两个人同时看到卡片是常态。
        // 但必须告诉后到的那个人实际生效的是什么。
        DecisionOutcome::AlreadyDecided { approved } => Ok(Json(DecisionResult {
            approved,
            was_first: false,
        })),
        DecisionOutcome::NotFound => Err(AppError::NotFound(format!("审批 {id} 不存在"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_card_reports_the_remaining_time_so_the_client_never_computes_it() {
        // 前端自己算剩余时间的话，客户端时钟偏几分钟就会把还能点的卡片
        // 显示成已过期（或者反过来）
        let approval = ai_task_store::Approval {
            id: ApprovalId::new(),
            run_id: ai_task_proto::RunId::new(),
            node_key: Some("gate".into()),
            title: "确认".into(),
            intent: serde_json::json!({}),
            rule_id: None,
            requested_at: chrono::Utc::now(),
            expires_at: chrono::Utc::now() + chrono::Duration::seconds(120),
            decided_at: None,
            decided_by: None,
            approved: None,
            reason: None,
        };
        let card = ApprovalCard::from(approval);
        assert!(
            (115..=120).contains(&card.expires_in_s),
            "{}",
            card.expires_in_s
        );
    }

    #[test]
    fn an_expired_card_reports_zero_not_a_negative_number() {
        let approval = ai_task_store::Approval {
            id: ApprovalId::new(),
            run_id: ai_task_proto::RunId::new(),
            node_key: None,
            title: "确认".into(),
            intent: serde_json::json!({}),
            rule_id: None,
            requested_at: chrono::Utc::now(),
            expires_at: chrono::Utc::now() - chrono::Duration::hours(1),
            decided_at: None,
            decided_by: None,
            approved: None,
            reason: None,
        };
        assert_eq!(ApprovalCard::from(approval).expires_in_s, 0);
    }
}
