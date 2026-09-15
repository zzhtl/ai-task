//! 任务的增查与触发。

use ai_task_core::ValidatedDag;
use ai_task_proto::{
    CreateTask, FieldError, Page, PageQuery, RunEventBody, RunSummary, TaskDetail, TaskId,
    TaskSummary, TriggerKind, TriggerRun,
};
use ai_task_store::idempotency::{IdempotentCreate, IdempotentRun};
use ai_task_store::{NewRun, NewTask, PendingEvent};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Json, http::HeaderMap};

use crate::error::AppError;
use crate::idempotency::IdempotentJson;
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

/// `DELETE /api/v1/tasks/{id}`
///
/// **级联删掉这个任务的全部执行历史。**那些 run 里有成本记录和完整事件流，
/// 是审计材料；这个操作没有撤销键，所以响应里报出实际删掉了多少。
pub async fn delete(
    State(state): State<AppState>,
    Path(id): Path<TaskId>,
) -> Result<impl IntoResponse, AppError> {
    let task = state
        .store
        .get_task(state.workspace_id, id)
        .await
        .map_err(|e| map_not_found(e, "task", &id.to_string()))?;
    let runs = state.store.count_runs_of(state.workspace_id, id).await?;

    if !state.store.delete_task(state.workspace_id, id).await? {
        return Err(AppError::NotFound(format!("任务 {id} 不存在")));
    }

    // before 里留下名字和被牵连的 run 数——删除之后，审计是唯一还能回答
    // "这里原来有什么"的地方
    state
        .audit(
            "task.delete",
            "task",
            id.to_string(),
            Some(serde_json::json!({ "name": task.name, "runs_deleted": runs })),
            None,
        )
        .await;

    Ok(Json(serde_json::json!({ "deleted_runs": runs })))
}

/// `GET /api/v1/tasks`
pub async fn list(
    State(state): State<AppState>,
    Query(page): Query<PageQuery>,
) -> Result<Json<Page<TaskSummary>>, AppError> {
    let tasks = state
        .store
        .list_tasks(state.workspace_id, i64::from(page.effective_limit()))
        .await?;

    // `last_run` 以前恒为 None——DTO 里有这个字段、列表页也要显示"上次执行"，
    // 于是前端只好自己再拉 200 条 run 回去逐行 find。
    // 一条 LATERAL 把它补上，不是 N+1。
    let ids: Vec<_> = tasks.iter().map(|t| t.id).collect();
    let mut last = state.store.last_runs_for(state.workspace_id, &ids).await?;

    Ok(Json(Page {
        items: tasks
            .iter()
            .map(|t| task_summary(t, last.remove(&t.id).map(|r| to_summary(&r))))
            .collect(),
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
    request: IdempotentJson<TriggerRun>,
) -> Result<Response, AppError> {
    let IdempotentJson { body, key, hash } = request;
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

    let new_run = NewRun {
        workspace_id: state.workspace_id,
        task_id: task.id,
        task_version_id: task.current_version_id,
        trigger: TriggerKind::Manual,
        dry_run: body.dry_run,
        inputs: body.inputs.clone(),
        compare_to: body.compare_to,
    };
    let queued = PendingEvent::run(RunEventBody::RunQueued {
        task_version_id: task.current_version_id,
        trigger: TriggerKind::Manual,
        inputs: body.inputs.clone(),
        dry_run: body.dry_run,
    });

    // 幂等键和建 run 在**同一个事务**里提交（ADR 0002）。
    // 校验放在认领之前：422 不该变成一条可重放的存储响应。
    let outcome = state
        .store
        .create_run_idempotent(
            IdempotentCreate {
                workspace_id: state.workspace_id,
                key: key.as_deref(),
                request_hash: &hash,
                status: i32::from(StatusCode::ACCEPTED.as_u16()),
            },
            new_run,
            queued,
            |run| serde_json::to_value(to_summary(run)).unwrap_or(serde_json::Value::Null),
        )
        .await?;

    let run = match outcome {
        IdempotentRun::Created(run) => *run,
        IdempotentRun::Replayed { status, body } => {
            return Ok(crate::idempotency::replay(status, body));
        }
        IdempotentRun::Conflict => {
            return Err(AppError::Conflict(
                "同一个 Idempotency-Key 配了不同的请求体".into(),
            ));
        }
        IdempotentRun::InFlight => {
            return Err(AppError::Conflict(
                "同一个 Idempotency-Key 的上一次请求还在处理中".into(),
            ));
        }
    };
    let summary = to_summary(&run);

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

    // **提交之后**才能 spawn：事务里起后台任务的话，回滚了任务还在跑。
    // 这中间崩溃会留下一个 queued 的 run，由启动时的 unfinished_runs 收尾。
    state.supervisor.spawn(state.workspace_id, run.id);

    let mut headers = HeaderMap::new();
    if let Ok(location) = format!("/api/v1/runs/{}", run.id).parse() {
        headers.insert(axum::http::header::LOCATION, location);
    }
    Ok((StatusCode::ACCEPTED, headers, Json(summary)).into_response())
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
