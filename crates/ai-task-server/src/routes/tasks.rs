//! 任务的增查与触发。

use ai_task_core::ValidatedDag;
use ai_task_proto::{
    CreateTask, FieldError, Page, PageQuery, RunEventBody, RunSummary, TaskDetail, TaskId,
    TaskSummary, TriggerKind, TriggerRun,
};
use ai_task_store::{NewRun, NewTask, PendingEvent};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::{Json, http::HeaderMap};

use crate::error::AppError;
use crate::routes::runs::to_summary;
use crate::state::AppState;

/// `POST /api/v1/tasks`
pub async fn create(
    State(state): State<AppState>,
    Json(body): Json<CreateTask>,
) -> Result<impl IntoResponse, AppError> {
    // DAG 在**入库时**校验，不是运行时。一个环、一个悬空引用、一个语法错误的
    // JSONPath，都应该在保存时就被拒，而不是等到凌晨两点触发时才让 run 挂掉。
    ValidatedDag::validate(body.spec.clone()).map_err(|errors| {
        AppError::Validation(
            errors
                .iter()
                .map(|e| FieldError {
                    field: "spec".into(),
                    code: "invalid_dag".into(),
                    message: e.to_string(),
                })
                .collect(),
        )
    })?;

    let rules_hash = rules_hash(&body.rules);
    let (task, _) = state
        .store
        .create_task(NewTask {
            workspace_id: state.workspace_id,
            name: body.name,
            description: body.description,
            spec: body.spec,
            rules: body.rules,
            rules_hash,
            enabled: body.enabled,
        })
        .await
        .map_err(map_conflict)?;

    Ok((StatusCode::CREATED, Json(task_summary(&task, None))))
}

/// `PUT /api/v1/tasks/{id}` —— 更新定义，**产生一个新版本**。
///
/// 老版本原样保留：历史 run 绑的是版本快照，改定义不能动它们，
/// 否则改完之后历史 run 无法解释、无法回放，漂移检测也失去了基线。
///
/// `If-Match` 带上 `GET` 返回的 ETag 就能挡住丢更新。不带的话按"我不在乎并发"
/// 处理——刻意允许，脚本化操作里逼着先 GET 一次很烦，而任务定义的丢更新
/// 后果有限（老版本还在，能看出被覆盖了什么）。
pub async fn update(
    State(state): State<AppState>,
    Path(id): Path<TaskId>,
    headers: HeaderMap,
    Json(body): Json<CreateTask>,
) -> Result<impl IntoResponse, AppError> {
    ValidatedDag::validate(body.spec.clone()).map_err(|errors| {
        AppError::Validation(
            errors
                .iter()
                .map(|e| FieldError {
                    field: "spec".into(),
                    code: "invalid_dag".into(),
                    message: e.to_string(),
                })
                .collect(),
        )
    })?;

    let expected = headers
        .get(axum::http::header::IF_MATCH)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.trim_matches(['"', 'W', '/']))
        .and_then(|v| v.parse::<i64>().ok());

    let rules_hash = rules_hash(&body.rules);
    let (task, version) = state
        .store
        .update_task(
            state.workspace_id,
            id,
            expected,
            NewTask {
                workspace_id: state.workspace_id,
                name: body.name,
                description: body.description,
                spec: body.spec,
                rules: body.rules,
                rules_hash,
                enabled: body.enabled,
            },
        )
        .await
        .map_err(|err| match err {
            // 版本不匹配是 412，不是 409：语义上是"你给的前置条件不成立"
            ai_task_store::StoreError::Conflict { what: "task", id } => {
                AppError::PreconditionFailed(format!("任务已被别人改过：{id}"))
            }
            other => map_conflict(other),
        })?;

    state
        .audit(
            "task.update",
            "task",
            id.to_string(),
            None,
            Some(serde_json::json!({
                "version_no": version.version_no,
                "name": task.name,
            })),
        )
        .await;

    let mut out = HeaderMap::new();
    if let Ok(etag) = format!("\"{}\"", task.version).parse() {
        out.insert(axum::http::header::ETAG, etag);
    }
    Ok((out, Json(task_summary(&task, None))))
}

/// `GET /api/v1/tasks`/// `GET /api/v1/tasks`
pub async fn list(
    State(state): State<AppState>,
    Query(page): Query<PageQuery>,
) -> Result<Json<Page<TaskSummary>>, AppError> {
    let tasks = state
        .store
        .list_tasks(state.workspace_id, i64::from(page.effective_limit()))
        .await?;
    Ok(Json(Page {
        items: tasks.iter().map(|t| task_summary(t, None)).collect(),
        next_cursor: None,
    }))
}

/// `GET /api/v1/tasks/{id}` —— 带上当前版本的完整编排定义。
pub async fn get(
    State(state): State<AppState>,
    Path(id): Path<TaskId>,
) -> Result<impl IntoResponse, AppError> {
    let task = state
        .store
        .get_task(state.workspace_id, id)
        .await
        .map_err(|e| map_not_found(e, "task", &id.to_string()))?;
    let version = state
        .store
        .get_task_version(task.current_version_id)
        .await?;

    // ETag 用乐观锁版本号。更新时带 If-Match 就能挡住丢更新。
    let mut headers = HeaderMap::new();
    if let Ok(etag) = format!("\"{}\"", task.version).parse() {
        headers.insert(axum::http::header::ETAG, etag);
    }
    Ok((
        headers,
        Json(TaskDetail {
            summary: task_summary(&task, None),
            spec: version.spec,
            rules: version.rules,
            version_no: version.version_no,
        }),
    ))
}

/// `POST /api/v1/tasks/{id}/runs` —— 触发一次执行。
///
/// 返回 **202 Accepted** 而不是 201：run 只是被接受了，还没跑完。
/// 客户端拿 `Location` 里的 URL 去订阅事件流看进展。
pub async fn trigger(
    State(state): State<AppState>,
    Path(id): Path<TaskId>,
    Json(body): Json<TriggerRun>,
) -> Result<impl IntoResponse, AppError> {
    let task = state
        .store
        .get_task(state.workspace_id, id)
        .await
        .map_err(|e| map_not_found(e, "task", &id.to_string()))?;

    if !task.enabled {
        return Err(AppError::Conflict(format!("任务 {} 已停用", task.name)));
    }

    // 基线必须是本 workspace 里真实存在的 run。给一个不存在的 id 就默默
    // 不比较的话，用户会以为"跑完了没报漂移 = 没漂移"。
    if let Some(baseline) = body.compare_to {
        state
            .store
            .get_run(state.workspace_id, baseline)
            .await
            .map_err(|_| {
                AppError::Validation(vec![ai_task_proto::FieldError {
                    field: "compare_to".into(),
                    code: "not_found".into(),
                    message: format!("基线 run {baseline} 不存在"),
                }])
            })?;
    }

    let run = state
        .store
        .create_run(
            NewRun {
                workspace_id: state.workspace_id,
                task_id: task.id,
                task_version_id: task.current_version_id,
                trigger: TriggerKind::Manual,
                dry_run: body.dry_run,
                inputs: body.inputs.clone(),
                compare_to: body.compare_to,
            },
            PendingEvent::run(RunEventBody::RunQueued {
                task_version_id: task.current_version_id,
                trigger: TriggerKind::Manual,
                inputs: body.inputs,
                dry_run: body.dry_run,
            }),
        )
        .await?;

    // 触发是"谁让它跑的"。定时触发由调度器负责记，这里记的是人手动点的。
    state
        .audit(
            "run.trigger",
            "run",
            run.id.to_string(),
            None,
            Some(serde_json::json!({
                "task_id": task.id.to_string(),
                "dry_run": body.dry_run,
                "compare_to": body.compare_to.map(|id| id.to_string()),
            })),
        )
        .await;

    state.supervisor.spawn(state.workspace_id, run.id);

    let mut headers = HeaderMap::new();
    if let Ok(location) = format!("/api/v1/runs/{}", run.id).parse() {
        headers.insert(axum::http::header::LOCATION, location);
    }
    Ok((StatusCode::ACCEPTED, headers, Json(to_summary(&run))))
}

fn task_summary(task: &ai_task_store::TaskRecord, last_run: Option<RunSummary>) -> TaskSummary {
    TaskSummary {
        id: task.id,
        name: task.name.clone(),
        description: task.description.clone(),
        current_version_id: task.current_version_id,
        enabled: task.enabled,
        created_at: task.created_at,
        updated_at: task.updated_at,
        version: task.version,
        last_run,
    }
}

/// 规则文本的指纹。进 run 的漂移指纹，用来回答「是哪版规则产生了这个行为」。
fn rules_hash(rules: &[String]) -> String {
    let mut sorted = rules.to_vec();
    // 排序后再哈希：规则顺序调换不改变语义，不该被算成一次漂移
    sorted.sort();
    let mut hasher = blake3::Hasher::new();
    for rule in &sorted {
        hasher.update(rule.as_bytes());
        hasher.update(b"\n");
    }
    hasher.finalize().to_hex()[..32].to_string()
}

fn map_conflict(err: ai_task_store::StoreError) -> AppError {
    match err {
        ai_task_store::StoreError::Conflict { what, id } => {
            AppError::Conflict(format!("{what} `{id}` 已存在"))
        }
        other => AppError::Store(other),
    }
}

fn map_not_found(err: ai_task_store::StoreError, what: &str, id: &str) -> AppError {
    match err {
        // 跨 workspace 也走这里：返回 403 等于确认了这个 ID 的存在
        ai_task_store::StoreError::NotFound { .. } => {
            AppError::NotFound(format!("{what} {id} 不存在"))
        }
        other => AppError::Store(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules_hash_is_order_independent_but_content_sensitive() {
        let a = rules_hash(&["禁止删除".into(), "只读".into()]);
        let b = rules_hash(&["只读".into(), "禁止删除".into()]);
        assert_eq!(a, b, "调换规则顺序不改变语义，不该算成漂移");

        let c = rules_hash(&["禁止删除".into()]);
        assert_ne!(a, c, "少一条规则必须体现在指纹里");
        assert_eq!(a.len(), 32);
    }

    #[test]
    fn empty_rules_still_produce_a_stable_hash() {
        assert_eq!(rules_hash(&[]), rules_hash(&[]));
        assert_ne!(rules_hash(&[]), rules_hash(&["x".into()]));
    }
}
