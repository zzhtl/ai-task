//! 审计日志的读接口。

use ai_task_proto::Page;
use axum::Json;
use axum::extract::{Query, State};
use serde::{Deserialize, Serialize};

use crate::error::AppError;
use crate::state::AppState;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditQuery {
    /// 只看某个对象的历史，如 `rule` / `host` / `approval` / `run`。
    /// 两个都要给，只给一个是查询写错了。
    #[serde(default)]
    pub target_kind: Option<String>,
    #[serde(default)]
    pub target_id: Option<String>,
    #[serde(default)]
    pub limit: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct AuditItem {
    pub id: i64,
    /// `None` 表示系统自身的动作（调度器触发、超时自动拒绝）。
    pub actor: Option<String>,
    pub action: String,
    pub target_kind: String,
    pub target_id: String,
    pub before: Option<serde_json::Value>,
    pub after: Option<serde_json::Value>,
    /// 关联的 HTTP 请求 id，能和访问日志对上。
    pub request_id: Option<String>,
    pub ts: chrono::DateTime<chrono::Utc>,
}

/// `GET /api/v1/audit`
pub async fn list(
    State(state): State<AppState>,
    Query(query): Query<AuditQuery>,
) -> Result<Json<Page<AuditItem>>, AppError> {
    // 只给一半的过滤条件是写错了，不该悄悄退化成"查全部"
    let target = match (&query.target_kind, &query.target_id) {
        (Some(kind), Some(id)) => Some((kind.as_str(), id.as_str())),
        (None, None) => None,
        _ => {
            return Err(AppError::Validation(vec![ai_task_proto::FieldError {
                field: "target_kind".into(),
                code: "incomplete".into(),
                message: "target_kind 和 target_id 要么都给，要么都不给".into(),
            }]));
        }
    };

    let records = state
        .store
        .recent_audit(state.workspace_id, target, query.limit.unwrap_or(100))
        .await?;

    Ok(Json(Page {
        items: records
            .into_iter()
            .map(|r| AuditItem {
                id: r.id,
                actor: r.actor_id.map(|id| id.to_string()),
                action: r.action,
                target_kind: r.target_kind,
                target_id: r.target_id,
                before: r.before,
                after: r.after,
                request_id: r.request_id,
                ts: r.ts,
            })
            .collect(),
        next_cursor: None,
    }))
}
