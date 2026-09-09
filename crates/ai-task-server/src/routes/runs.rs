//! Run 的查询与取消。

use ai_task_proto::{Page, RunId, RunSummary};
use ai_task_store::RunRecord;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::{Json, response::IntoResponse};
use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::error::AppError;
use crate::state::AppState;

/// `GET /api/v1/runs` 的查询参数。
///
/// `deny_unknown_fields`：拼错的参数必须报错。静默忽略会把一个笔误变成
/// 「返回全部」——最典型的例子是 `taskid` 拼错后看起来"查到了很多结果"。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListQuery {
    #[serde(default)]
    pub task_id: Option<ai_task_proto::TaskId>,
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
}

/// `GET /api/v1/runs`
pub async fn list(
    State(state): State<AppState>,
    Query(query): Query<ListQuery>,
) -> Result<Json<Page<RunSummary>>, AppError> {
    let limit = i64::from(query.limit.unwrap_or(50).clamp(1, 200));
    let cursor = query.cursor.as_deref().map(decode_cursor).transpose()?;

    let runs = state
        .store
        .list_runs(state.workspace_id, query.task_id, cursor, limit)
        .await?;

    // 取满一页才给游标；不满说明到底了，给 next_cursor 会让客户端白跑一次
    let next_cursor = (i64::try_from(runs.len()).unwrap_or(0) == limit)
        .then(|| runs.last().map(|r| encode_cursor(r.created_at, r.id)))
        .flatten();

    Ok(Json(Page {
        items: runs.iter().map(to_summary).collect(),
        next_cursor,
    }))
}

/// `GET /api/v1/runs/{id}`
pub async fn get(
    State(state): State<AppState>,
    Path(id): Path<RunId>,
) -> Result<Json<RunSummary>, AppError> {
    let run = state
        .store
        .get_run(state.workspace_id, id)
        .await
        .map_err(|e| map_not_found(e, id))?;
    Ok(Json(to_summary(&run)))
}

/// `POST /api/v1/runs/{id}/cancel`
/// `DELETE /api/v1/runs/{id}` —— 删掉一次执行记录连同它的事件流和资源采样。
///
/// 不做软删除：run 里的成本和策略判决是审计材料，"删了但还在"比真删更糟——
/// 它会让每一条按 run 聚合的查询都得记得加 `WHERE NOT deleted`，漏一个地方
/// 就是数字对不上。要留痕的是**这次删除本身**，那条记录在 audit_log 里。
pub async fn delete(
    State(state): State<AppState>,
    Path(id): Path<RunId>,
) -> Result<impl IntoResponse, AppError> {
    // 先取快照：删完就没得取了，而审计要记下删掉的是什么
    let run = state
        .store
        .get_run(state.workspace_id, id)
        .await
        .map_err(|e| map_not_found(e, id))?;

    match state
        .store
        .delete_run(state.workspace_id, id)
        .await
        .map_err(AppError::from)?
    {
        // 刚才 get_run 还拿得到，现在没了：有人在同时删同一条。
        // 结果和自己删掉是一样的，别报错。
        //
        // （单纯重复调用不会走到这里——上面的 get_run 会先返回 404。
        //   DELETE 的幂等在这两条路径上都成立：不会 500，也不会有副作用。）
        None => Ok(StatusCode::NO_CONTENT),
        Some(false) => Err(AppError::Conflict(format!(
            "run 还在 {:?}，执行器仍在往它的事件流里写。先取消，跑完了再删",
            run.status
        ))),
        Some(true) => {
            state
                .audit(
                    "run.delete",
                    "run",
                    id.to_string(),
                    Some(serde_json::json!({
                        "task_id": run.task_id.to_string(),
                        "status": run.status,
                        "cost_usd": run.cost,
                    })),
                    None,
                )
                .await;
            Ok(StatusCode::NO_CONTENT)
        }
    }
}

pub async fn cancel(
    State(state): State<AppState>,
    Path(id): Path<RunId>,
) -> Result<impl IntoResponse, AppError> {
    let run = state
        .store
        .get_run(state.workspace_id, id)
        .await
        .map_err(|e| map_not_found(e, id))?;

    if run.status.is_terminal() {
        return Err(AppError::Conflict(format!(
            "run 已经是终态 {:?}，无法取消",
            run.status
        )));
    }

    if state.supervisor.cancel(id) {
        state
            .audit("run.cancel", "run", id.to_string(), None, None)
            .await;
        // 202：取消已受理，但 run 要等执行器收尾才会真正落终态
        Ok(StatusCode::ACCEPTED)
    } else {
        // M1 只能取消本副本上的 run。跨副本取消要走数据库标记位，是 M2 的事。
        Err(AppError::Conflict(
            "这个 run 不在当前副本上执行，暂时无法取消".into(),
        ))
    }
}

pub fn to_summary(run: &RunRecord) -> RunSummary {
    RunSummary {
        id: run.id,
        task_id: run.task_id,
        task_version_id: run.task_version_id,
        status: run.status,
        trigger: run.trigger,
        dry_run: run.dry_run,
        created_at: run.created_at,
        started_at: run.started_at,
        finished_at: run.finished_at,
        cost_usd: run.cost,
        // 只在终态且非成功时给：running 中的 error 列可能是上一次尝试留下的
        error: (run.status.is_terminal() && !run.status.is_success())
            .then(|| run.error.clone())
            .flatten(),
        max_seq: run.max_seq,
    }
}

/// 游标编码 `(created_at, id)`。
///
/// 对客户端不透明——里面是什么、怎么编码，随时可以变。
fn encode_cursor(created_at: DateTime<Utc>, id: RunId) -> String {
    format!("{}|{id}", created_at.timestamp_micros())
}

fn decode_cursor(cursor: &str) -> Result<(DateTime<Utc>, RunId), AppError> {
    let bad = || AppError::BadRequest("cursor 不合法；它只应当来自上一页的 next_cursor".into());
    let (micros, id) = cursor.split_once('|').ok_or_else(bad)?;
    let created_at =
        DateTime::from_timestamp_micros(micros.parse().map_err(|_| bad())?).ok_or_else(bad)?;
    Ok((created_at, RunId(id.parse().map_err(|_| bad())?)))
}

fn map_not_found(err: ai_task_store::StoreError, id: RunId) -> AppError {
    match err {
        ai_task_store::StoreError::NotFound { .. } => {
            AppError::NotFound(format!("run {id} 不存在"))
        }
        other => AppError::Store(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_round_trips_without_losing_precision() {
        // 微秒精度必须保住：同一毫秒内的并发插入靠它 + id 去重
        let ts = DateTime::from_timestamp_micros(1_788_857_536_123_456).expect("时间");
        let id = RunId::new();
        let (back_ts, back_id) = decode_cursor(&encode_cursor(ts, id)).expect("解析");
        assert_eq!(back_ts, ts);
        assert_eq!(back_id, id);
    }

    #[test]
    fn a_tampered_cursor_is_a_bad_request_not_a_panic() {
        for bad in ["", "abc", "123", "abc|def", "123|not-a-uuid", "|"] {
            assert!(
                matches!(decode_cursor(bad), Err(AppError::BadRequest(_))),
                "游标 {bad:?} 应当被拒"
            );
        }
    }
}
