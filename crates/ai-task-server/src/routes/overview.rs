//! 首页聚合。
//!
//! 首页是全站唯一一个每隔几秒就打一次的页面。之前它拉四个接口、其中一个要回
//! **200 条完整 run**，只为在浏览器里算出五个数字——而且那几个"24 小时"的数字
//! 在实例忙起来之后就是错的（第 201 条之后的不参与统计）。
//! 这里一次算完，响应体是常数级的。

use ai_task_proto::{
    FieldError, Overview, OverviewDaily, OverviewDailyQuery, OverviewQuery, OverviewTasks,
    OverviewTasksQuery,
};
use axum::extract::State;

use crate::error::AppError;
use crate::extract::{Json, Query};
use crate::state::AppState;

/// 列表长度上限。首页只是"现在怎么样"，要翻记录去 /runs。
const LIVE_LIMIT: i64 = 20;
const RECENT_LIMIT: i64 = 10;
const UPCOMING_LIMIT: i64 = 6;

/// `GET /api/v1/overview`
pub async fn get(
    State(state): State<AppState>,
    Query(query): Query<OverviewQuery>,
) -> Result<Json<Overview>, AppError> {
    let window_hours = query.effective_window_hours();
    let ws = state.workspace_id;

    // 四条查询并发跑：它们互不依赖，而且各自命中不同的索引。
    // 串行的话首页的延迟就是四次往返之和。
    let (stats, live, recent, upcoming) = tokio::try_join!(
        state.store.overview_stats(ws, window_hours),
        state.store.overview_live(ws, LIVE_LIMIT),
        state.store.overview_recent(ws, RECENT_LIMIT),
        state.store.overview_upcoming(ws, UPCOMING_LIMIT),
    )?;

    Ok(Json(Overview {
        stats,
        live,
        recent,
        upcoming,
    }))
}

/// `GET /api/v1/overview/daily` —— 最近几天每天的执行结果，给首页的趋势图和迷你图。
pub async fn daily(
    State(state): State<AppState>,
    Query(query): Query<OverviewDailyQuery>,
) -> Result<Json<OverviewDaily>, AppError> {
    let (days, tz) = daily_params(&query)?;
    // 「今天」按请求里的时区算：东八区的凌晨一点，UTC 还是昨天
    let last_day = chrono::Utc::now().with_timezone(&tz).date_naive();
    let first_day = last_day - chrono::Days::new(u64::from(days - 1));
    let buckets = state
        .store
        .overview_daily(state.workspace_id, tz.name(), first_day, last_day)
        .await?;
    Ok(Json(OverviewDaily {
        timezone: tz.name().to_string(),
        days: buckets,
    }))
}

/// `GET /api/v1/overview/tasks` —— 窗口内失败最多的任务。
pub async fn tasks(
    State(state): State<AppState>,
    Query(query): Query<OverviewTasksQuery>,
) -> Result<Json<OverviewTasks>, AppError> {
    let (window_hours, limit) = tasks_params(&query)?;
    let items = state
        .store
        .overview_failing_tasks(state.workspace_id, window_hours, i64::from(limit))
        .await?;
    Ok(Json(OverviewTasks {
        window_hours,
        items,
    }))
}

/// 校验并补上默认值。有几个参数不对就一次报几个，别让人改一个试一次。
fn daily_params(query: &OverviewDailyQuery) -> Result<(u32, chrono_tz::Tz), AppError> {
    let mut errors = Vec::new();
    let days = query.days.unwrap_or(OverviewDailyQuery::DEFAULT_DAYS);
    if !(1..=OverviewDailyQuery::MAX_DAYS).contains(&days) {
        errors.push(invalid(
            "days",
            format!("看 1 到 {} 天", OverviewDailyQuery::MAX_DAYS),
        ));
    }
    let name = query
        .timezone
        .as_deref()
        .unwrap_or(OverviewDailyQuery::DEFAULT_TIMEZONE);
    let tz = match name.parse::<chrono_tz::Tz>() {
        Ok(tz) => Some(tz),
        Err(_) => {
            errors.push(invalid(
                "timezone",
                format!("`{name}` 不是合法的 IANA 时区名（形如 Asia/Shanghai、UTC）"),
            ));
            None
        }
    };
    match tz {
        Some(tz) if errors.is_empty() => Ok((days, tz)),
        _ => Err(AppError::Validation(errors)),
    }
}

fn tasks_params(query: &OverviewTasksQuery) -> Result<(u32, u32), AppError> {
    let mut errors = Vec::new();
    let window = query
        .window_hours
        .unwrap_or(OverviewTasksQuery::DEFAULT_WINDOW_HOURS);
    if !(1..=OverviewTasksQuery::MAX_WINDOW_HOURS).contains(&window) {
        errors.push(invalid(
            "window_hours",
            format!("看 1 到 {} 小时", OverviewTasksQuery::MAX_WINDOW_HOURS),
        ));
    }
    let limit = query.limit.unwrap_or(OverviewTasksQuery::DEFAULT_LIMIT);
    if !(1..=OverviewTasksQuery::MAX_LIMIT).contains(&limit) {
        errors.push(invalid(
            "limit",
            format!("一次最多 {} 个任务", OverviewTasksQuery::MAX_LIMIT),
        ));
    }
    if errors.is_empty() {
        Ok((window, limit))
    } else {
        Err(AppError::Validation(errors))
    }
}

fn invalid(field: &str, message: String) -> FieldError {
    FieldError {
        field: field.into(),
        code: "invalid".into(),
        message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields(err: AppError) -> Vec<String> {
        match err {
            AppError::Validation(errors) => errors.into_iter().map(|e| e.field).collect(),
            other => panic!("应该是 422，实际 {other:?}"),
        }
    }

    #[test]
    fn daily_defaults_to_two_weeks_in_shanghai() {
        let (days, tz) = daily_params(&OverviewDailyQuery::default()).expect("默认值合法");
        assert_eq!(days, 14);
        assert_eq!(tz.name(), "Asia/Shanghai");
    }

    #[test]
    fn daily_reports_every_bad_parameter_at_once() {
        let err = daily_params(&OverviewDailyQuery {
            days: Some(0),
            timezone: Some("Mars/Olympus".into()),
        })
        .expect_err("两个都不对");
        assert_eq!(fields(err), vec!["days", "timezone"]);

        let err = daily_params(&OverviewDailyQuery {
            days: Some(91),
            timezone: None,
        })
        .expect_err("超过 90 天");
        assert_eq!(fields(err), vec!["days"]);
    }

    #[test]
    fn tasks_bounds_are_enforced_together() {
        assert_eq!(
            tasks_params(&OverviewTasksQuery::default()).expect("默认值合法"),
            (168, 5)
        );
        let err = tasks_params(&OverviewTasksQuery {
            window_hours: Some(721),
            limit: Some(0),
        })
        .expect_err("两个都越界");
        assert_eq!(fields(err), vec!["window_hours", "limit"]);
    }
}
