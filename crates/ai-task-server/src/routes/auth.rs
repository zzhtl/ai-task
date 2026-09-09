//! 登录、注销、当前身份。
//!
//! 会话 token 走 **HttpOnly cookie**，不是 `localStorage`：后者对页面上的
//! 任意脚本可读，一次 XSS 就等于长期会话被拿走。cookie 加 `SameSite=Strict`
//! 挡掉跨站携带。

use ai_task_store::{NewUser, Role};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::{Json, response::IntoResponse};
use serde::{Deserialize, Serialize};

use crate::error::AppError;
use crate::state::AppState;

/// 会话 cookie 名。
pub const COOKIE: &str = "ai_task_session";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Credentials {
    pub email: String,
    pub password: String,
}

#[derive(Debug, Serialize)]
pub struct Identity {
    pub email: Option<String>,
    pub display_name: String,
    pub role: String,
}

/// `POST /api/v1/auth/login`
pub async fn login(
    State(state): State<AppState>,
    Json(body): Json<Credentials>,
) -> Result<impl IntoResponse, AppError> {
    let Some((token, principal)) = state
        .store
        .login(state.workspace_id, &body.email, &body.password)
        .await?
    else {
        // **不区分"用户不存在"和"口令错了"。**区分开等于提供一个用户名枚举器。
        return Err(AppError::Unauthorized("邮箱或口令不正确"));
    };

    state
        .audit(
            "auth.login",
            "user",
            principal.user_id.to_string(),
            None,
            None,
        )
        .await;

    let mut headers = HeaderMap::new();
    if let Ok(cookie) =
        session_cookie(&token, ai_task_store::auth::SESSION_TTL.num_seconds()).parse()
    {
        headers.insert(header::SET_COOKIE, cookie);
    }
    Ok((
        headers,
        Json(Identity {
            email: Some(body.email),
            display_name: principal.display_name,
            role: principal.role.as_str().to_owned(),
        }),
    ))
}

/// `POST /api/v1/auth/logout`
pub async fn logout(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, AppError> {
    if let Some(token) = token_from(&headers) {
        state.store.logout(&token).await?;
    }
    let mut out = HeaderMap::new();
    // Max-Age=0 让浏览器立刻丢掉它
    if let Ok(cookie) = session_cookie("", 0).parse() {
        out.insert(header::SET_COOKIE, cookie);
    }
    Ok((out, StatusCode::NO_CONTENT))
}

/// `GET /api/v1/auth/me`
///
/// 前端启动时问一次：没登录就跳登录页，登录了就按角色决定哪些按钮可点。
pub async fn me(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Identity>, AppError> {
    let principal = current(&state, &headers)
        .await?
        .ok_or(AppError::Unauthorized("尚未登录"))?;
    Ok(Json(Identity {
        email: Some(principal.email),
        display_name: principal.display_name,
        role: principal.role.as_str().to_owned(),
    }))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bootstrap {
    pub email: String,
    pub password: String,
    pub display_name: String,
}

/// `POST /api/v1/auth/bootstrap` —— 首次部署时建第一个管理员。
///
/// **只在一个用户都没有时可用。**已经有用户之后必须由管理员在界面上加人，
/// 否则这个接口就是一个任何人都能用的提权入口。
pub async fn bootstrap(
    State(state): State<AppState>,
    Json(body): Json<Bootstrap>,
) -> Result<impl IntoResponse, AppError> {
    if state.store.has_any_user(state.workspace_id).await? {
        return Err(AppError::Conflict(
            "已经有用户了。加人请让管理员在界面上操作".into(),
        ));
    }
    if body.password.chars().count() < state.min_password_len {
        return Err(AppError::Validation(vec![ai_task_proto::FieldError {
            field: "password".into(),
            code: "too_short".into(),
            message: format!(
                "管理员口令至少 {} 个字符：这个账号能 SSH 到任意机器执行命令",
                state.min_password_len
            ),
        }]));
    }

    let id = state
        .store
        .create_user(NewUser {
            workspace_id: state.workspace_id,
            email: body.email.clone(),
            display_name: body.display_name,
            password: body.password,
            role: Role::Admin,
        })
        .await?;

    state
        .audit(
            "auth.bootstrap",
            "user",
            id.to_string(),
            None,
            Some(serde_json::json!({ "email": body.email, "role": "admin" })),
        )
        .await;

    Ok((StatusCode::CREATED, Json(serde_json::json!({ "id": id }))))
}

/// 当前请求的身份。`None` 表示没登录。
pub async fn current(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<Option<ai_task_store::Principal>, AppError> {
    let Some(token) = token_from(headers) else {
        return Ok(None);
    };
    Ok(state.store.principal_for(&token).await?)
}

fn token_from(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())?
        .split(';')
        .filter_map(|part| part.trim().split_once('='))
        .find(|(name, _)| *name == COOKIE)
        .map(|(_, value)| value.to_owned())
}

fn session_cookie(token: &str, max_age: i64) -> String {
    // 刻意不带 Secure：这个服务默认只监听回环，开发时是 http。
    // 对外暴露时必须在反代上终止 TLS 并补 Secure ——README 里写了这一条。
    format!("{COOKIE}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age={max_age}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_session_cookie_is_not_reachable_from_page_scripts() {
        let cookie = session_cookie("abc", 3600);
        assert!(cookie.contains("HttpOnly"), "少了它，一次 XSS 就能拿走会话");
        assert!(cookie.contains("SameSite=Strict"), "{cookie}");
        assert!(cookie.contains("Path=/"), "{cookie}");
    }

    #[test]
    fn logging_out_expires_the_cookie_immediately() {
        assert!(session_cookie("", 0).contains("Max-Age=0"));
    }

    #[test]
    fn the_token_is_picked_out_of_a_crowded_cookie_header() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            "theme=dark; ai_task_session=deadbeef; other=1"
                .parse()
                .expect("头"),
        );
        assert_eq!(token_from(&headers).as_deref(), Some("deadbeef"));

        // 名字只是前缀的 cookie 不能被当成会话
        let mut decoy = HeaderMap::new();
        decoy.insert(
            header::COOKIE,
            "ai_task_session_backup=nope".parse().expect("头"),
        );
        assert_eq!(token_from(&decoy), None);
    }

    #[test]
    fn no_cookie_means_not_logged_in_rather_than_an_error() {
        assert_eq!(token_from(&HeaderMap::new()), None);
    }
}
