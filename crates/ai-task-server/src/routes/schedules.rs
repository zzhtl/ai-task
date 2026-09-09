//! 定时配置。
//!
//! **cron 表达式在保存时就编译一次。** 存进去一个跑不通的表达式，代价是凌晨
//! 两点那个任务不响——而且没有任何东西会报错，因为调度器只是永远算不出下一个
//! 触发点。保存时拒绝，是这个错误唯一有机会被人看见的时刻。

use ai_task_proto::{FieldError, MisfirePolicy, OverlapPolicy, Page, ScheduleId, TaskId};
use ai_task_runtime::CronSchedule;
use ai_task_store::schedules::NewSchedule;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::{Json, response::IntoResponse};
use serde::{Deserialize, Serialize};

use crate::error::AppError;
use crate::state::AppState;

/// jitter 上限。比它更大的抖动会让「几点触发」失去意义。
const MAX_JITTER_S: u32 = 3600;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateSchedule {
    pub task_id: TaskId,
    /// 5 段（分 时 日 月 周）或 6 段（秒 分 时 日 月 周），支持 `L` 和 `#`。
    pub cron: String,
    /// IANA 时区名，如 `Asia/Shanghai`。夏令时按本地时间语义处理。
    #[serde(default = "default_tz")]
    pub timezone: String,
    /// 服务停过一段之后怎么补。默认只补一次。
    #[serde(default)]
    pub misfire: MisfirePolicy,
    /// 上一次还没跑完时怎么办。默认跳过。
    #[serde(default)]
    pub overlap: OverlapPolicy,
    /// 随机延迟秒数，避免整点惊群。**不影响记录在案的触发点**。
    #[serde(default)]
    pub jitter_s: u32,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_tz() -> String {
    "Asia/Shanghai".into()
}
fn default_true() -> bool {
    true
}

#[derive(Debug, Serialize)]
pub struct ScheduleSummary {
    pub id: String,
    pub task_id: String,
    pub cron: String,
    pub timezone: String,
    pub misfire: MisfirePolicy,
    pub overlap: OverlapPolicy,
    pub jitter_s: u32,
    pub enabled: bool,
    /// 下一次触发时刻。停用时仍然保留，恢复后从这里接着走。
    /// `None` 表示这条配置已经算不出未来的触发点了。
    pub next_fire_at: Option<chrono::DateTime<chrono::Utc>>,
    pub last_fired_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 接下来三次触发，按该时区的本地时间给。**用来让人一眼看出表达式写对没有**
    /// ——`0 0 * * *` 和 `0 0 * * 0` 光看字符串是分不出来的。
    pub next_three: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListQuery {
    #[serde(default)]
    pub task_id: Option<TaskId>,
}

/// `GET /api/v1/schedules`
pub async fn list(
    State(state): State<AppState>,
    Query(query): Query<ListQuery>,
) -> Result<Json<Page<ScheduleSummary>>, AppError> {
    let rows = state
        .store
        .list_schedules(state.workspace_id, query.task_id)
        .await?;
    Ok(Json(Page {
        items: rows
            .into_iter()
            .map(|(id, due, enabled)| ScheduleSummary {
                id: id.to_string(),
                task_id: due.task_id.to_string(),
                next_three: preview(&due.cron, &due.timezone),
                cron: due.cron,
                timezone: due.timezone,
                misfire: due.misfire,
                overlap: due.overlap,
                jitter_s: due.jitter_s,
                enabled,
                next_fire_at: due.next_fire_at,
                last_fired_at: due.last_fired_at,
            })
            .collect(),
        next_cursor: None,
    }))
}

/// `POST /api/v1/schedules`
pub async fn create(
    State(state): State<AppState>,
    Json(body): Json<CreateSchedule>,
) -> Result<impl IntoResponse, AppError> {
    let new = validate(&state, &body).await?;
    let id = state.store.create_schedule(new).await?;

    state
        .audit(
            "schedule.create",
            "schedule",
            id.to_string(),
            None,
            Some(serde_json::json!({
                "task_id": body.task_id.to_string(),
                "cron": body.cron,
                "timezone": body.timezone,
            })),
        )
        .await;

    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({ "id": id.to_string() })),
    ))
}

/// `PUT /api/v1/schedules/{id}`
pub async fn update(
    State(state): State<AppState>,
    Path(id): Path<uuid::Uuid>,
    Json(body): Json<CreateSchedule>,
) -> Result<impl IntoResponse, AppError> {
    let new = validate(&state, &body).await?;
    if !state
        .store
        .update_schedule(state.workspace_id, ScheduleId(id), &new)
        .await?
    {
        return Err(AppError::NotFound(format!("定时配置 {id} 不存在")));
    }
    state
        .audit(
            "schedule.update",
            "schedule",
            id.to_string(),
            None,
            Some(serde_json::json!({ "cron": body.cron, "enabled": body.enabled })),
        )
        .await;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnabledChange {
    pub enabled: bool,
}

/// `PUT /api/v1/schedules/{id}/enabled`
pub async fn set_enabled(
    State(state): State<AppState>,
    Path(id): Path<uuid::Uuid>,
    Json(body): Json<EnabledChange>,
) -> Result<impl IntoResponse, AppError> {
    if !state
        .store
        .set_schedule_enabled(state.workspace_id, ScheduleId(id), body.enabled)
        .await?
    {
        return Err(AppError::NotFound(format!("定时配置 {id} 不存在")));
    }
    state
        .audit(
            "schedule.set_enabled",
            "schedule",
            id.to_string(),
            None,
            Some(serde_json::json!({ "enabled": body.enabled })),
        )
        .await;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /api/v1/schedules/{id}`
pub async fn delete(
    State(state): State<AppState>,
    Path(id): Path<uuid::Uuid>,
) -> Result<impl IntoResponse, AppError> {
    if !state
        .store
        .delete_schedule(state.workspace_id, ScheduleId(id))
        .await?
    {
        return Err(AppError::NotFound(format!("定时配置 {id} 不存在")));
    }
    state
        .audit("schedule.delete", "schedule", id.to_string(), None, None)
        .await;
    Ok(StatusCode::NO_CONTENT)
}

/// 校验并算出第一个触发点。
async fn validate(state: &AppState, body: &CreateSchedule) -> Result<NewSchedule, AppError> {
    // 任务必须在本 workspace 里存在，否则这条定时配置永远跑不出东西
    state
        .store
        .get_task(state.workspace_id, body.task_id)
        .await
        .map_err(|_| invalid("task_id", &format!("任务 {} 不存在", body.task_id)))?;

    if body.jitter_s > MAX_JITTER_S {
        return Err(invalid(
            "jitter_s",
            &format!("最多 {MAX_JITTER_S} 秒：再大的话「几点触发」就没有意义了"),
        ));
    }

    // 在这里编译一次。存进去一个跑不通的表达式，代价是那个任务永远不响，
    // 而且不会有任何报错。
    let schedule = CronSchedule::parse(&body.cron, &body.timezone)
        .map_err(|err| invalid("cron", &err.to_string()))?;

    let now = chrono::Utc::now();
    let next_fire_at = schedule.next_after(now).ok_or_else(|| {
        invalid(
            "cron",
            "这个表达式算不出任何未来的触发点（比如 2 月 30 日）",
        )
    })?;

    Ok(NewSchedule {
        workspace_id: state.workspace_id,
        task_id: body.task_id,
        cron: body.cron.clone(),
        timezone: body.timezone.clone(),
        misfire: body.misfire,
        overlap: body.overlap,
        jitter_s: body.jitter_s,
        enabled: body.enabled,
        next_fire_at,
        // jitter 只推迟「去抢」的时刻，**绝不能污染 next_fire_at**：
        // 那个值会进 runs.fire_at，而幂等唯一约束依赖它。
        next_claim_at: next_fire_at,
    })
}

/// 接下来三次触发，按该时区的本地时间。
///
/// 让人一眼看出表达式写对没有——`0 0 * * *`（每天零点）和 `0 0 * * 0`
/// （每周日零点）光看字符串是分不出来的，看时间就一目了然。
fn preview(cron: &str, timezone: &str) -> Vec<String> {
    let Ok(schedule) = CronSchedule::parse(cron, timezone) else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(3);
    let mut cursor = chrono::Utc::now();
    for _ in 0..3 {
        let Some(next) = schedule.next_after(cursor) else {
            break;
        };
        out.push(schedule.format_local(next));
        cursor = next;
    }
    out
}

fn invalid(field: &str, message: &str) -> AppError {
    AppError::Validation(vec![FieldError {
        field: field.into(),
        code: "invalid".into(),
        message: message.into(),
    }])
}
