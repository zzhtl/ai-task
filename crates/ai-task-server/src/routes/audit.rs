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
    /// 文本搜索：动作、对象 id、操作人、以及变更前后的内容。
    #[serde(default)]
    pub q: Option<String>,
    /// 只看这些动作，逗号分隔。
    ///
    /// 界面上的动作名是中文（"创建主机"），而库里存的是 `host.create`——
    /// 中文标签只存在于前端的映射表里，服务端搜不到。所以由前端把匹配上的
    /// 动作码翻译出来放进这个参数，而不是把那张表复制一份到后端。
    #[serde(default)]
    pub action: Option<String>,
    /// 上一页的 `next_cursor`。
    #[serde(default)]
    pub cursor: Option<String>,
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
    // 只给 target_id 不给 target_kind 是写错了：一个 id 不说清是什么东西的 id，
    // 没法判断它该匹配哪一行，悄悄退化成"查全部"更糟。
    // 反过来只给 target_kind 是合法的——「只看主机相关的动作」是个正当的筛选。
    if query.target_id.is_some() && query.target_kind.is_none() {
        return Err(AppError::Validation(vec![ai_task_proto::FieldError {
            field: "target_kind".into(),
            code: "required".into(),
            message: "给了 target_id 就必须同时给 target_kind".into(),
        }]));
    }
    let target = (query.target_kind.as_deref(), query.target_id.as_deref());

    let actions: Option<Vec<String>> = query.action.as_deref().map(|raw| {
        raw.split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect()
    });
    let search = query.q.as_deref().map(str::trim).filter(|q| !q.is_empty());
    let cursor = query.cursor.as_deref().map(decode_cursor).transpose()?;

    let limit = query.limit.unwrap_or(50).clamp(1, 200);
    // 多取一条来判断还有没有下一页，比单独 count 便宜
    let mut records = state
        .store
        .list_audit(
            state.workspace_id,
            ai_task_store::audit::AuditFilter {
                target_kind: target.0,
                target_id: target.1,
                actions: actions.as_deref(),
                search,
                cursor,
            },
            limit + 1,
        )
        .await?;

    let has_more = records.len() as i64 > limit;
    records.truncate(limit as usize);
    let next_cursor = has_more
        .then(|| records.last().map(|r| encode_cursor(r.ts, r.id)))
        .flatten();

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
        next_cursor,
    }))
}

/// 游标编码 `(ts, id)`。`id` 是 bigserial，同一毫秒内也能稳定排序。
/// 对客户端不透明——里面是什么随时可以变。
fn encode_cursor(ts: chrono::DateTime<chrono::Utc>, id: i64) -> String {
    format!("{}|{id}", ts.timestamp_micros())
}

fn decode_cursor(cursor: &str) -> Result<(chrono::DateTime<chrono::Utc>, i64), AppError> {
    let bad = || AppError::BadRequest("cursor 不合法；它只应当来自上一页的 next_cursor".into());
    let (micros, id) = cursor.split_once('|').ok_or_else(bad)?;
    let ts = chrono::DateTime::from_timestamp_micros(micros.parse().map_err(|_| bad())?)
        .ok_or_else(bad)?;
    Ok((ts, id.parse().map_err(|_| bad())?))
}
