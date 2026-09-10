//! 路由树与中间件栈。

pub mod approvals;
pub mod audit;
pub mod auth;
pub mod drift;
pub mod health;
pub mod hosts;
pub mod policy;
pub mod remote;
pub mod rules;
pub mod runs;
pub mod schedules;
pub mod sse;
pub mod tasks;
pub mod users;

use axum::Router;
use axum::routing::{get, post, put};
use tower::{Layer as _, ServiceBuilder};
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::compression::CompressionLayer;
use tower_http::normalize_path::{NormalizePath, NormalizePathLayer};
use tower_http::trace::TraceLayer;

use crate::error::AppError;
use crate::middleware::trace::RequestSpan;
use crate::state::AppState;
use crate::{assets, middleware};

/// 单个请求体的上限。M1 起 DAG spec 会走这里，1 MiB 对一份编排定义绰绰有余。
const BODY_LIMIT: usize = 1024 * 1024;

/// 对外服务的完整栈。
///
/// `NormalizePathLayer` **必须包在 Router 外面**，不能用 `Router::layer` 挂：
/// 后者在路由**之后**执行，等路径匹配失败了再去规整就晚了，`/healthz/` 照样 404。
/// 这条在 tower-http 的文档里写着，但很容易在照抄中间件栈时踩到。
#[must_use]
pub fn build_service(state: AppState) -> NormalizePath<Router> {
    NormalizePathLayer::trim_trailing_slash().layer(build_router(state))
}

fn build_router(state: AppState) -> Router {
    let api = Router::new()
        .route("/healthz", get(health::healthz))
        .route("/readyz", get(health::readyz))
        .route("/tasks", get(tasks::list).post(tasks::create))
        .route(
            "/tasks/{id}",
            get(tasks::get).put(tasks::update).delete(tasks::delete),
        )
        .route("/tasks/{id}/runs", post(tasks::trigger))
        .route("/runs", get(runs::list))
        .route("/runs/{id}", get(runs::get).delete(runs::delete))
        .route("/runs/{id}/cancel", post(runs::cancel))
        .route("/runs/{id}/events", get(sse::stream))
        .route("/runs/{id}/metrics", get(hosts::metrics))
        .route("/runs/{id}/drift", get(drift::compare))
        .route("/hosts", get(hosts::list).post(hosts::create))
        .route("/hosts/{id}", put(hosts::update).delete(hosts::delete))
        .route("/schedules", get(schedules::list).post(schedules::create))
        .route(
            "/schedules/{id}",
            axum::routing::put(schedules::update).delete(schedules::delete),
        )
        .route(
            "/schedules/{id}/enabled",
            axum::routing::put(schedules::set_enabled),
        )
        .route("/approvals", get(approvals::list))
        .route("/audit", get(audit::list))
        .route("/auth/login", post(auth::login))
        .route("/auth/logout", post(auth::logout))
        .route("/auth/me", get(auth::me))
        .route("/auth/bootstrap", post(auth::bootstrap))
        .route("/users", get(users::list).post(users::create))
        .route("/users/{id}", put(users::update).delete(users::delete))
        .route("/users/{id}/role", axum::routing::put(users::set_role))
        .route(
            "/users/{id}/disabled",
            axum::routing::put(users::set_disabled),
        )
        .route("/users/{id}/revoke-sessions", post(users::revoke_sessions))
        .route("/approvals/{id}/decide", post(approvals::decide))
        .route("/rules", get(rules::list_rules).post(rules::create_rule))
        .route(
            "/rules/{id}",
            put(rules::update_rule).delete(rules::delete_rule),
        )
        .route("/rules/{id}/enabled", put(rules::set_rule_enabled))
        .route("/skills", get(rules::list_skills).post(rules::create_skill));

    // 内部接口：只有 run 工作目录里的 hook 配置知道令牌。
    // 刻意不挂在 /api/v1 下，免得被当成公开接口对待。
    let internal = Router::new()
        .route("/policy/decide", post(policy::decide))
        .route("/policy/await", post(policy::await_approval))
        .route("/remote/call", post(remote::call));

    Router::new()
        // /api/v1 下的未知路径必须是 JSON 404。没有这一条的话它会落到下面的
        // SPA 回退上，客户端拿到 200 + 一段 HTML——一个拼错的接口路径看起来
        // 像"调通了"，是最难查的一类问题。
        .nest("/api/v1", api.fallback(api_not_found))
        .nest("/internal", internal)
        .fallback(assets::serve)
        // RBAC 挂在 with_state 之前、路由之后：它要读 state，也要看得见
        // 最终匹配到的方法。内部接口不走这里——它们用的是 run 级令牌，
        // 调用方是 hook 子进程，没有会话可言。
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            middleware::rbac::layer,
        ))
        .with_state(state)
        .layer(
            // 顺序自外向内。每一层的位置都有理由：
            ServiceBuilder::new()
                // 1. request id 必须在 trace 之前：span 要带上它
                .layer(axum::middleware::from_fn(middleware::request_id::layer))
                .layer(TraceLayer::new_for_http().make_span_with(RequestSpan))
                // 2. 压缩在业务层外侧，handler 不用关心
                .layer(CompressionLayer::new().gzip(true).no_br().no_deflate())
                .layer(axum::extract::DefaultBodyLimit::max(BODY_LIMIT))
                // 3. CatchPanic 在最内侧：一个 handler panic 不能带走整个连接，
                //    但它外面的 trace/request-id 仍要能记录这次失败
                .layer(CatchPanicLayer::new()),
        )
}

/// `/api/v1` 下没有匹配到任何路由。
///
/// 取 `OriginalUri` 而不是 `Uri`：`nest()` 会把前缀剥掉再交给内层，
/// 用 `Uri` 报出来的是 `/healthzz` 而不是用户真正请求的 `/api/v1/healthzz`。
async fn api_not_found(axum::extract::OriginalUri(uri): axum::extract::OriginalUri) -> AppError {
    AppError::NotFound(format!("接口 {} 不存在", uri.path()))
}

// CORS 刻意不加。前后端由同一个二进制 serve，本来同源；开发期走 Vite 代理。
// 这个服务能 SSH 到任意机器执行命令，放开跨域是不必要的攻击面。

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt as _;

    /// 不需要数据库就能测的路由用这个：readyz 之外的一切。
    ///
    /// 刻意复用生产的 `NormalizePathLayer` 包法，否则测出来的路径行为
    /// 和真正跑起来的不是一回事。
    fn router_without_state() -> NormalizePath<Router> {
        let router = Router::new()
            .nest(
                "/api/v1",
                Router::new()
                    .route("/healthz", get(health::healthz))
                    .fallback(api_not_found),
            )
            .fallback(assets::serve)
            .layer(
                ServiceBuilder::new()
                    .layer(NormalizePathLayer::trim_trailing_slash())
                    .layer(axum::middleware::from_fn(middleware::request_id::layer))
                    .layer(CatchPanicLayer::new()),
            );
        NormalizePathLayer::trim_trailing_slash().layer(router)
    }

    #[tokio::test]
    async fn healthz_is_200_and_does_not_touch_the_database() {
        let response = router_without_state()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/healthz")
                    .body(Body::empty())
                    .expect("请求"),
            )
            .await
            .expect("响应");
        assert_eq!(response.status(), StatusCode::OK);

        let body = to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("body");
        let health: ai_task_proto::Health = serde_json::from_slice(&body).expect("解析");
        assert_eq!(health.status, ai_task_proto::HealthStatus::Ok);
        assert!(!health.version.is_empty());
    }

    #[tokio::test]
    async fn every_response_carries_a_request_id() {
        let response = router_without_state()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/healthz")
                    .body(Body::empty())
                    .expect("请求"),
            )
            .await
            .expect("响应");
        let id = response
            .headers()
            .get(middleware::request_id::HEADER)
            .and_then(|v| v.to_str().ok())
            .expect("必须回写 x-request-id");
        assert!(
            uuid::Uuid::parse_str(id).is_ok(),
            "自动生成的应当是 UUID：{id}"
        );
    }

    #[tokio::test]
    async fn an_incoming_request_id_is_propagated() {
        let response = router_without_state()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/healthz")
                    .header(middleware::request_id::HEADER, "trace-abc-1")
                    .body(Body::empty())
                    .expect("请求"),
            )
            .await
            .expect("响应");
        assert_eq!(
            response
                .headers()
                .get(middleware::request_id::HEADER)
                .map(|v| v.as_bytes()),
            Some(b"trace-abc-1".as_slice())
        );
    }

    #[tokio::test]
    async fn a_forged_request_id_is_replaced_not_echoed() {
        // 含控制字符的头值 hyper 自己就拒了，这里挡的是它放行、但我们不想
        // 原样写进日志的那些：超长值、含空格或尖括号的值。
        for forged in ["x".repeat(200), "a b".into(), "<script>".into()] {
            let response = router_without_state()
                .oneshot(
                    Request::builder()
                        .uri("/api/v1/healthz")
                        .header(middleware::request_id::HEADER, &forged)
                        .body(Body::empty())
                        .expect("请求"),
                )
                .await
                .expect("响应");
            let id = response
                .headers()
                .get(middleware::request_id::HEADER)
                .and_then(|v| v.to_str().ok())
                .expect("应当有 id");
            assert!(
                uuid::Uuid::parse_str(id).is_ok(),
                "非法 id {forged:?} 应被替换，实际回显了 {id:?}"
            );
        }
    }

    #[tokio::test]
    async fn trailing_slash_reaches_the_same_handler() {
        let response = router_without_state()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/healthz/")
                    .body(Body::empty())
                    .expect("请求"),
            )
            .await
            .expect("响应");
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn an_unknown_api_path_is_a_json_404_not_the_spa_shell() {
        let response = router_without_state()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/healthzz")
                    .body(Body::empty())
                    .expect("请求"),
            )
            .await
            .expect("响应");
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        let body = to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("body");
        let err: ai_task_proto::ApiError =
            serde_json::from_slice(&body).expect("接口 404 必须是结构化 JSON，而不是 SPA 的 HTML");
        assert_eq!(err.code, "not_found");
        assert!(
            err.message.contains("/api/v1/healthzz"),
            "报出来的必须是用户请求的完整路径，不能是 nest 剥掉前缀后的：{}",
            err.message
        );
        assert!(!err.request_id.is_empty(), "错误体里必须带 request_id");
    }

    #[tokio::test]
    async fn frontend_routes_fall_through_to_the_spa() {
        let response = router_without_state()
            .oneshot(
                Request::builder()
                    .uri("/runs/abc")
                    .body(Body::empty())
                    .expect("请求"),
            )
            .await
            .expect("响应");
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 1024 * 1024)
            .await
            .expect("body");
        assert!(String::from_utf8_lossy(&body).contains("<!doctype html>"));
    }
}
