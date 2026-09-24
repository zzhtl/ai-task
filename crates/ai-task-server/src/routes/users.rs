//! 用户与角色管理。全部要 admin。
//!
//! 每个动作都进审计：这些接口决定的是"谁能让这个系统去别的机器上执行命令"，
//! 事后必须能回答"这个人的权限是谁给的"。

use ai_task_proto::{FieldError, Page, UserId};
use ai_task_store::{NewUser, Role, UserUpdate};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use serde::{Deserialize, Serialize};

use crate::error::AppError;
use crate::extract::{Json, Path};
use crate::state::AppState;

#[derive(Debug, Serialize)]
pub struct UserSummary {
    pub id: String,
    pub email: String,
    pub display_name: String,
    pub role: String,
    /// `true` 表示已停用：登不进来，已有会话也已吊销。
    pub disabled: bool,
    pub active_sessions: i64,
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// 首次部署时创建的内置管理员：不能删、不能降级或停用，资料只能由本人改。
    /// 它是系统的兜底账号，别的管理员动不了它。
    pub system: bool,
}

/// `GET /api/v1/users`
pub async fn list(State(state): State<AppState>) -> Result<Json<Page<UserSummary>>, AppError> {
    let users = state.store.list_users(state.workspace_id).await?;
    let system = system_user_id(&users);
    Ok(Json(Page {
        items: users
            .into_iter()
            .map(|u| UserSummary {
                id: u.id.to_string(),
                email: u.email,
                display_name: u.display_name,
                role: u.role.as_str().to_owned(),
                disabled: u.disabled_at.is_some(),
                active_sessions: u.active_sessions,
                created_at: u.created_at,
                system: Some(u.id) == system,
            })
            .collect(),
        next_cursor: None,
    }))
}

/// 内置管理员 = 这个 workspace 里最早创建的用户，也就是 bootstrap 建出来的那个。
/// 列表本来就按 created_at 排，取第一个。
fn system_user_id(users: &[ai_task_store::auth::UserRecord]) -> Option<UserId> {
    users.first().map(|u| u.id)
}

async fn is_system_user(state: &AppState, id: UserId) -> Result<bool, AppError> {
    // 一条 ORDER BY ... LIMIT 1，不再为了找"最早那个"把整张表拉回来
    Ok(state.store.system_user_id(state.workspace_id).await? == Some(id))
}

/// 当前请求是谁发的。没开认证时是 `None`。
async fn actor_id(state: &AppState, headers: &HeaderMap) -> Result<Option<UserId>, AppError> {
    if !state.require_auth {
        return Ok(None);
    }
    Ok(crate::routes::auth::current(state, headers)
        .await?
        .map(|p| p.user_id))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateUser {
    pub email: String,
    pub display_name: String,
    /// 留空或不给表示口令不动。
    #[serde(default)]
    pub password: Option<String>,
}

/// `PUT /api/v1/users/{id}`
///
/// 改邮箱、显示名，可选换口令。**不吊销会话**：改口令是常规操作，泄漏时用
/// `revoke-sessions` 明确地踢。内置管理员的资料只能由本人改。
pub async fn update(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<uuid::Uuid>,
    Json(body): Json<UpdateUser>,
) -> Result<impl IntoResponse, AppError> {
    let user_id = UserId(id);
    if !body.email.contains('@') {
        return Err(invalid("email", "不像是一个邮箱地址"));
    }
    if body.display_name.trim().is_empty() {
        return Err(invalid("display_name", "显示名不能为空"));
    }
    let password = body.password.filter(|p| !p.is_empty());
    if let Some(p) = &password {
        let floor = state.min_password_len;
        if p.chars().count() < floor {
            return Err(invalid(
                "password",
                &format!("至少 {floor} 个字符：这个账号能让系统 SSH 到任意机器执行命令"),
            ));
        }
    }
    if is_system_user(&state, user_id).await? {
        let actor = actor_id(&state, &headers).await?;
        if state.require_auth && actor != Some(user_id) {
            return Err(AppError::Conflict("内置管理员的资料只能由本人修改".into()));
        }
    }
    let password_changed = password.is_some();

    let changed = state
        .store
        .update_user(
            state.workspace_id,
            user_id,
            UserUpdate {
                email: body.email.clone(),
                display_name: body.display_name.trim().to_owned(),
                password,
            },
        )
        .await
        .map_err(|err| match err {
            ai_task_store::StoreError::Conflict { id, .. } => {
                AppError::Conflict(format!("邮箱 {id} 已被占用"))
            }
            other => AppError::Store(other),
        })?;
    if !changed {
        return Err(AppError::NotFound(format!("用户 {id} 不存在")));
    }

    // 不记口令，只记改了没改
    state
        .audit(
            "user.update",
            "user",
            id.to_string(),
            None,
            Some(serde_json::json!({
                "email": body.email,
                "display_name": body.display_name.trim(),
                "password_changed": password_changed,
            })),
        )
        .await;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /api/v1/users/{id}`
///
/// 三道闸：不能删自己、不能删内置管理员、不能删最后一个管理员。
/// 他建过的任务和做过的审批留下，只是关联的用户变成空。
pub async fn delete(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<uuid::Uuid>,
) -> Result<impl IntoResponse, AppError> {
    let user_id = UserId(id);
    if actor_id(&state, &headers).await? == Some(user_id) {
        return Err(AppError::Conflict(
            "不能删除自己的账号。让另一个管理员来删".into(),
        ));
    }
    if is_system_user(&state, user_id).await? {
        return Err(AppError::Conflict(
            "内置管理员不能删除：它是首次部署时创建的兜底账号".into(),
        ));
    }
    guard_last_admin(&state, &headers, user_id, true).await?;

    // 先把 email 拿到手：删掉之后审计里只剩一个 id，没人知道那是谁
    let email = state
        .store
        .list_users(state.workspace_id)
        .await?
        .into_iter()
        .find(|u| u.id == user_id)
        .map(|u| u.email)
        .ok_or_else(|| AppError::NotFound(format!("用户 {id} 不存在")))?;

    if !state.store.delete_user(state.workspace_id, user_id).await? {
        return Err(AppError::NotFound(format!("用户 {id} 不存在")));
    }
    state
        .audit(
            "user.delete",
            "user",
            id.to_string(),
            Some(serde_json::json!({ "email": email })),
            None,
        )
        .await;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateUser {
    pub email: String,
    pub display_name: String,
    pub password: String,
    pub role: String,
}

/// `POST /api/v1/users`
pub async fn create(
    State(state): State<AppState>,
    Json(body): Json<CreateUser>,
) -> Result<impl IntoResponse, AppError> {
    let role = parse_role(&body.role)?;
    if !body.email.contains('@') {
        return Err(invalid("email", "不像是一个邮箱地址"));
    }
    let floor = state.min_password_len;
    if body.password.chars().count() < floor {
        return Err(invalid(
            "password",
            &format!("至少 {floor} 个字符：这个账号能让系统 SSH 到任意机器执行命令"),
        ));
    }

    let id = state
        .store
        .create_user(NewUser {
            workspace_id: state.workspace_id,
            email: body.email.clone(),
            display_name: body.display_name.clone(),
            password: body.password,
            role,
        })
        .await
        .map_err(|err| match err {
            ai_task_store::StoreError::Conflict { id, .. } => {
                AppError::Conflict(format!("邮箱 {id} 已被占用"))
            }
            other => AppError::Store(other),
        })?;

    state
        .audit(
            "user.create",
            "user",
            id.to_string(),
            None,
            Some(serde_json::json!({
                "email": body.email,
                "display_name": body.display_name,
                "role": role.as_str(),
            })),
        )
        .await;

    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({ "id": id.to_string() })),
    ))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoleChange {
    pub role: String,
}

/// `PUT /api/v1/users/{id}/role`
///
/// 改动**立刻生效**，不用吊销会话：每次请求都会重算角色。
pub async fn set_role(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<uuid::Uuid>,
    Json(body): Json<RoleChange>,
) -> Result<impl IntoResponse, AppError> {
    let role = parse_role(&body.role)?;
    let user_id = UserId(id);
    if role != Role::Admin && is_system_user(&state, user_id).await? {
        return Err(AppError::Conflict("内置管理员不能降级".into()));
    }
    guard_last_admin(&state, &headers, user_id, role != Role::Admin).await?;

    if !state
        .store
        .set_role(state.workspace_id, user_id, role)
        .await?
    {
        return Err(AppError::NotFound(format!("用户 {id} 不存在")));
    }

    state
        .audit(
            "user.set_role",
            "user",
            id.to_string(),
            None,
            Some(serde_json::json!({ "role": role.as_str() })),
        )
        .await;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DisableChange {
    pub disabled: bool,
}

/// `PUT /api/v1/users/{id}/disabled`
///
/// 停用会连带吊销该用户的所有会话。
pub async fn set_disabled(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<uuid::Uuid>,
    Json(body): Json<DisableChange>,
) -> Result<impl IntoResponse, AppError> {
    let user_id = UserId(id);
    if body.disabled && is_system_user(&state, user_id).await? {
        return Err(AppError::Conflict("内置管理员不能停用".into()));
    }
    guard_last_admin(&state, &headers, user_id, body.disabled).await?;

    if !state
        .store
        .set_user_disabled(state.workspace_id, user_id, body.disabled)
        .await?
    {
        return Err(AppError::NotFound(format!("用户 {id} 不存在")));
    }

    state
        .audit(
            "user.set_disabled",
            "user",
            id.to_string(),
            None,
            Some(serde_json::json!({ "disabled": body.disabled })),
        )
        .await;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/v1/users/{id}/revoke-sessions`
///
/// 把这个人踢下线。口令泄漏时用得上——改口令本身不会让已发出的 token 失效。
pub async fn revoke_sessions(
    State(state): State<AppState>,
    Path(id): Path<uuid::Uuid>,
) -> Result<impl IntoResponse, AppError> {
    // 先确认这个用户在本 workspace 里：不然拿别的租户的 id 就能踢人下线
    let user_id = UserId(id);
    let exists = state
        .store
        .list_users(state.workspace_id)
        .await?
        .into_iter()
        .any(|u| u.id == user_id);
    if !exists {
        return Err(AppError::NotFound(format!("用户 {id} 不存在")));
    }

    let revoked = state.store.revoke_sessions(user_id).await?;
    state
        .audit(
            "user.revoke_sessions",
            "user",
            id.to_string(),
            None,
            Some(serde_json::json!({ "revoked": revoked })),
        )
        .await;
    Ok(Json(serde_json::json!({ "revoked": revoked })))
}

/// 挡住"最后一个管理员被摘掉之后没人能管了"。
///
/// 那之后加人、改角色、看审计全都做不了，只能去改库救。这个操作没有撤销键。
///
/// **只在目标当前就是管理员时才需要判。**动一个非管理员不可能减少管理员数量
/// ——第一版漏了这个条件，结果把一个 viewer 升成 operator 也被拦成了"最后一个管理员"。
async fn guard_last_admin(
    state: &AppState,
    headers: &HeaderMap,
    target: UserId,
    losing_admin: bool,
) -> Result<(), AppError> {
    if !losing_admin || !state.require_auth {
        return Ok(());
    }

    let users = state.store.list_users(state.workspace_id).await?;
    let target_is_admin = users
        .iter()
        .any(|u| u.id == target && u.role == Role::Admin && u.disabled_at.is_none());
    if !target_is_admin {
        return Ok(());
    }

    if state.store.active_admin_count(state.workspace_id).await? > 1 {
        return Ok(());
    }

    let actor = crate::routes::auth::current(state, headers).await?;
    Err(AppError::Conflict(
        if actor.is_some_and(|a| a.user_id == target) {
            "你是最后一个管理员，不能把自己摘掉。先加一个管理员再来".into()
        } else {
            "这是最后一个管理员。摘掉之后加人、改角色、看审计都做不了，只能改库救回来".to_owned()
        },
    ))
}

fn parse_role(text: &str) -> Result<Role, AppError> {
    Role::parse(text).ok_or_else(|| invalid("role", "只能是 viewer / operator / admin"))
}

fn invalid(field: &str, message: &str) -> AppError {
    AppError::Validation(vec![FieldError {
        field: field.into(),
        code: "invalid".into(),
        message: message.into(),
    }])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_user_summary_never_carries_a_password_hash() {
        // 加一个 password/hash 字段会让这条失败。口令哈希离线爆破一次就够了。
        let json = serde_json::to_value(UserSummary {
            id: "u".into(),
            email: "a@b.c".into(),
            display_name: "zzh".into(),
            role: "admin".into(),
            disabled: false,
            active_sessions: 2,
            created_at: chrono::Utc::now(),
            system: false,
        })
        .expect("序列化");
        let keys: Vec<&str> = json
            .as_object()
            .expect("对象")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            [
                "id",
                "email",
                "display_name",
                "role",
                "disabled",
                "active_sessions",
                "created_at",
                "system"
            ],
            "用户的对外形状变了。加字段前先确认它不是凭据。"
        );
    }

    #[test]
    fn an_unknown_role_name_is_refused_rather_than_defaulted() {
        // 默认成 viewer 看起来"安全"，但会让一次拼写错误静默地不生效：
        // 你以为给了 operator，实际是 viewer
        assert!(parse_role("superuser").is_err());
        assert!(parse_role("Admin").is_err(), "大小写也要严格");
        assert!(parse_role("").is_err());
        assert_eq!(parse_role("operator").ok(), Some(Role::Operator));
    }
}
