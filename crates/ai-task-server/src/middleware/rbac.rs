//! 按角色放行。
//!
//! 判定只看**方法**，不看路径：读是 viewer，写是 operator，用户与凭据管理是 admin。
//! 按路径逐条配的话，新加一个接口忘了配就等于默认放行——这类遗漏没有任何症状，
//! 直到有人用它做了不该做的事。
//!
//! `AI_TASK_REQUIRE_AUTH=false` 可以整体关掉（默认关，见 config.rs 的说明）。
//! 关掉时所有请求以 admin 身份通过——**这只适用于回环上的单人部署**。

use ai_task_proto::UserId;
use ai_task_store::Role;
use axum::extract::{Request, State};
use axum::http::Method;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::error::AppError;
use crate::state::AppState;

/// 这些路径在未登录时也必须可达，否则没人能登录进来。
const PUBLIC: &[&str] = &[
    "/api/v1/healthz",
    "/api/v1/readyz",
    "/api/v1/auth/login",
    "/api/v1/auth/logout",
    "/api/v1/auth/me",
    "/api/v1/auth/bootstrap",
];

/// 需要 admin 的路径前缀：能读到别人身份、能加机器的地方。
const ADMIN_ONLY: &[&str] = &[
    "/api/v1/audit",
    "/api/v1/hosts",
    "/api/v1/rules",
    "/api/v1/users",
];

tokio::task_local! {
    /// 这次请求是谁发的。审计写入时读它，免得把 principal 塞进每个 handler 的签名。
    static CURRENT_USER: Option<UserId>;
}

/// 当前请求的用户。没开认证、或不在请求上下文里时为 `None`。
#[must_use]
pub fn current_user() -> Option<UserId> {
    CURRENT_USER.try_with(|u| *u).ok().flatten()
}

pub async fn layer(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let path = request.uri().path();

    // 静态资源和非 /api 路径不在这一层管：前端要能加载出登录页
    if !path.starts_with("/api/v1") || PUBLIC.contains(&path) {
        return next.run(request).await;
    }
    if !state.require_auth {
        return next.run(request).await;
    }

    let needed = required_role(request.method(), path);
    let principal = match crate::routes::auth::current(&state, request.headers()).await {
        Ok(Some(principal)) => principal,
        Ok(None) => return AppError::Unauthorized("需要登录").into_response(),
        // 查不动数据库时**关门**：认不出身份就不该放行
        Err(err) => return err.into_response(),
    };

    if !principal.role.allows(needed) {
        // 走 AppError 而不是手拼 JSON：手拼的那版少了 request_id 和 details，
        // 于是全站唯一一个客户端解析不出 `ApiError` 的响应恰好出现在权限失败上，
        // 而那正是最需要把 request_id 报给运维的时候。见 ADR 0002。
        return AppError::Forbidden(format!(
            "需要 {} 及以上，你是 {}",
            needed.as_str(),
            principal.role.as_str()
        ))
        .into_response();
    }

    CURRENT_USER
        .scope(Some(principal.user_id), next.run(request))
        .await
}

/// 这个请求需要哪一档。
///
/// **按方法判，不按路径枚举。**新接口默认落进正确的档位，
/// 而不是因为没人记得配而默认放行。
fn required_role(method: &Method, path: &str) -> Role {
    if ADMIN_ONLY.iter().any(|p| path.starts_with(p)) {
        return Role::Admin;
    }
    match *method {
        Method::GET | Method::HEAD | Method::OPTIONS => Role::Viewer,
        _ => Role::Operator,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reading_needs_viewer_and_writing_needs_operator() {
        assert_eq!(required_role(&Method::GET, "/api/v1/runs"), Role::Viewer);
        assert_eq!(
            required_role(&Method::POST, "/api/v1/tasks/x/runs"),
            Role::Operator
        );
        assert_eq!(
            required_role(&Method::PUT, "/api/v1/tasks/x"),
            Role::Operator
        );
        assert_eq!(
            required_role(&Method::DELETE, "/api/v1/anything"),
            Role::Operator
        );
    }

    #[test]
    fn an_unrecognised_write_route_defaults_to_operator_not_to_open() {
        // 明天加的接口今天就有正确的默认值
        assert_eq!(
            required_role(&Method::PATCH, "/api/v1/something-invented-tomorrow"),
            Role::Operator
        );
    }

    #[test]
    fn machines_credentials_and_the_audit_trail_are_admin_only() {
        // 主机里存着 SSH 私钥、审计里是"谁做了什么"、策略是护栏本身，
        // 这三样连读都不该对 operator 开放
        for path in [
            "/api/v1/hosts",
            "/api/v1/rules",
            "/api/v1/audit",
            "/api/v1/audit?limit=10",
            "/api/v1/users",
        ] {
            assert_eq!(required_role(&Method::GET, path), Role::Admin, "{path}");
        }
    }

    #[test]
    fn the_login_route_is_reachable_without_being_logged_in() {
        // 少了它就没人能登录进来
        assert!(PUBLIC.contains(&"/api/v1/auth/login"));
        assert!(PUBLIC.contains(&"/api/v1/auth/me"));
        assert!(PUBLIC.contains(&"/api/v1/healthz"));
        // 但业务接口一个都不在里面
        assert!(!PUBLIC.iter().any(|p| p.starts_with("/api/v1/tasks")));
        assert!(!PUBLIC.iter().any(|p| p.starts_with("/api/v1/runs")));
    }
}
