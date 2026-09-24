//! 定时配置。
//!
//! **cron 表达式在保存时就编译一次。** 存进去一个跑不通的表达式，代价是凌晨
//! 两点那个任务不响——而且没有任何东西会报错，因为调度器只是永远算不出下一个
//! 触发点。保存时拒绝，是这个错误唯一有机会被人看见的时刻。

use ai_task_proto::{
    FieldError, MisfirePolicy, OverlapPolicy, Page, ScheduleFire, ScheduleId, SchedulePreview,
    SchedulePreviewQuery, TaskId,
};
use ai_task_runtime::{CronError, CronSchedule};
use ai_task_store::schedules::NewSchedule;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::{Deserialize, Serialize};

use crate::error::AppError;
use crate::extract::{Json, Path, Query};
use crate::state::AppState;

/// jitter 上限。比它更大的抖动会让「几点触发」失去意义。
const MAX_JITTER_S: u32 = 3600;

/// 预览默认看几次、最多看几次。
const DEFAULT_PREVIEW: u32 = 5;
const MAX_PREVIEW: u32 = 10;

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
                next_three: next_three(&due.cron, &due.timezone),
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

/// `GET /api/v1/schedules/preview` —— 编辑定时时的实时预览。
///
/// 纯计算，不碰库。和保存时的校验走同一个解析：这里算得出来，保存时就不会因为
/// 表达式被拒。
pub async fn preview(
    Query(query): Query<SchedulePreviewQuery>,
) -> Result<Json<SchedulePreview>, AppError> {
    let count = query.count.unwrap_or(DEFAULT_PREVIEW);
    if !(1..=MAX_PREVIEW).contains(&count) {
        return Err(invalid("count", &format!("一次预览 1 到 {MAX_PREVIEW} 次")));
    }
    let schedule = CronSchedule::parse(&query.cron, &query.timezone).map_err(cron_error)?;
    let fires = upcoming(&schedule, chrono::Utc::now(), count)
        .into_iter()
        .map(|at| ScheduleFire {
            at,
            local: schedule.format_local(at),
        })
        .collect();
    Ok(Json(SchedulePreview { fires }))
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
    let schedule = CronSchedule::parse(&body.cron, &body.timezone).map_err(cron_error)?;

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
fn next_three(cron: &str, timezone: &str) -> Vec<String> {
    let Ok(schedule) = CronSchedule::parse(cron, timezone) else {
        return Vec::new();
    };
    upcoming(&schedule, chrono::Utc::now(), 3)
        .into_iter()
        .map(|at| schedule.format_local(at))
        .collect()
}

/// `after` 之后的 `count` 个触发点。算不出更多时就少给几个。
fn upcoming(
    schedule: &CronSchedule,
    after: chrono::DateTime<chrono::Utc>,
    count: u32,
) -> Vec<chrono::DateTime<chrono::Utc>> {
    let mut out = Vec::with_capacity(count as usize);
    let mut cursor = after;
    for _ in 0..count {
        let Some(next) = schedule.next_after(cursor) else {
            break;
        };
        out.push(next);
        cursor = next;
    }
    out
}

/// 解析错误归到出错的那个字段上：时区写错就标在时区框底下，别都算在表达式头上。
fn cron_error(err: CronError) -> AppError {
    let field = match err {
        CronError::BadTimezone(_) => "timezone",
        CronError::BadExpression { .. } | CronError::NeverFires(_) => "cron",
    };
    invalid(field, &err.to_string())
}

fn invalid(field: &str, message: &str) -> AppError {
    AppError::Validation(vec![FieldError {
        field: field.into(),
        code: "invalid".into(),
        message: message.into(),
    }])
}
