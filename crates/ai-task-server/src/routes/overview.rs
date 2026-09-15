//! 首页聚合。
//!
//! 首页是全站唯一一个每隔几秒就打一次的页面。之前它拉四个接口、其中一个要回
//! **200 条完整 run**，只为在浏览器里算出五个数字——而且那几个"24 小时"的数字
//! 在实例忙起来之后就是错的（第 201 条之后的不参与统计）。
//! 这里一次算完，响应体是常数级的。

use ai_task_proto::{Overview, OverviewQuery};
use axum::Json;
use axum::extract::{Query, State};

use crate::error::AppError;
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
