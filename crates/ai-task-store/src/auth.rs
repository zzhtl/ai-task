//! 用户、会话与角色。
//!
//! 三条不可动摇的规则：
//! 1. **口令只存 argon2id 哈希。**库被拖走也不该等于账号被拿走。
//! 2. **会话 token 只存 SHA-256。**存原文的话，能读库的人可以直接冒充任何人。
//! 3. **认证失败一律走同一条路径、同样的耗时。**"用户不存在"和"口令错了"
//!    在响应上必须无法区分，否则登录接口就成了一个用户名枚举器。

use ai_task_proto::{UserId, WorkspaceId};
use chrono::{DateTime, Duration, Utc};
use sqlx::Row as _;

use crate::{Store, StoreError};

/// 会话有效期。
pub const SESSION_TTL: Duration = Duration::days(7);

/// 角色。权限是包含关系：admin ⊃ operator ⊃ viewer。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Role {
    Viewer,
    Operator,
    Admin,
}

impl Role {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Viewer => "viewer",
            Self::Operator => "operator",
            Self::Admin => "admin",
        }
    }

    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "viewer" => Some(Self::Viewer),
            "operator" => Some(Self::Operator),
            "admin" => Some(Self::Admin),
            _ => None,
        }
    }

    /// 够不够 `needed` 这一档。
    ///
    /// 用序关系而不是逐个列举：新增一个中间档位时，所有检查点自动跟着对。
    #[must_use]
    pub fn allows(self, needed: Role) -> bool {
        self >= needed
    }
}

/// 用户的对外形状。**没有口令哈希，也不会有。**
#[derive(Debug, Clone)]
pub struct UserRecord {
    pub id: UserId,
    pub email: String,
    pub display_name: String,
    pub role: Role,
    pub disabled_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    /// 这个用户当前有几个活着的会话。
    pub active_sessions: i64,
}

/// 已认证的调用方。
#[derive(Debug, Clone)]
pub struct Principal {
    pub user_id: UserId,
    pub workspace_id: WorkspaceId,
    pub display_name: String,
    /// 该用户在这个 workspace 里的最高角色。
    pub role: Role,
}

#[derive(Debug, Clone)]
pub struct NewUser {
    pub workspace_id: WorkspaceId,
    pub email: String,
    pub display_name: String,
    pub password: String,
    pub role: Role,
}

impl Store {
    /// 建一个用户并绑定角色。
    pub async fn create_user(&self, new: NewUser) -> Result<UserId, StoreError> {
        let hash = hash_password(&new.password).map_err(|detail| StoreError::Corrupt {
            what: "password",
            detail,
        })?;
        let id = UserId::new();
        let mut tx = self.pool().begin().await?;
        sqlx::query(
            "INSERT INTO users (id, workspace_id, email, display_name, password_hash)
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(uuid::Uuid::from(id))
        .bind(uuid::Uuid::from(new.workspace_id))
        .bind(new.email.trim().to_lowercase())
        .bind(&new.display_name)
        .bind(&hash)
        .execute(&mut *tx)
        .await
        .map_err(|err| match err.as_database_error().and_then(|e| e.code()) {
            Some(code) if code == "23505" => StoreError::Conflict {
                what: "user",
                id: new.email.clone(),
            },
            _ => StoreError::from(err),
        })?;
        sqlx::query("INSERT INTO role_bindings (user_id, workspace_id, role) VALUES ($1, $2, $3)")
            .bind(uuid::Uuid::from(id))
            .bind(uuid::Uuid::from(new.workspace_id))
            .bind(new.role.as_str())
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(id)
    }

    /// 有没有任何用户。没有的话是首次部署，需要引导建管理员。
    pub async fn has_any_user(&self, workspace_id: WorkspaceId) -> Result<bool, StoreError> {
        let row = sqlx::query("SELECT 1 AS x FROM users WHERE workspace_id = $1 LIMIT 1")
            .bind(uuid::Uuid::from(workspace_id))
            .fetch_optional(self.pool())
            .await?;
        Ok(row.is_some())
    }

    /// 校验口令并建会话，返回明文 token（**只在这里出现这一次**）。
    ///
    /// 用户不存在时也会跑一次哈希校验。不这么做的话，"不存在"会明显更快返回，
    /// 于是这个接口就成了一个用户名枚举器。
    pub async fn login(
        &self,
        workspace_id: WorkspaceId,
        email: &str,
        password: &str,
    ) -> Result<Option<(String, Principal)>, StoreError> {
        let row = sqlx::query(
            "SELECT id, display_name, password_hash, disabled_at FROM users
             WHERE workspace_id = $1 AND email = $2",
        )
        .bind(uuid::Uuid::from(workspace_id))
        .bind(email.trim().to_lowercase())
        .fetch_optional(self.pool())
        .await?;

        // 没有这个用户时拿一个固定的哈希去比，让两条路径的耗时一致。
        // 取成 owned 而不是借着 row：借用会跨过下面的 await，编译不过。
        let stored: String = row
            .as_ref()
            .and_then(|r| r.try_get::<Option<String>, _>("password_hash").ok())
            .flatten()
            .unwrap_or_else(|| dummy_hash().to_owned());
        let ok = verify_password(password, &stored);

        let Some(row) = row else { return Ok(None) };
        if !ok {
            return Ok(None);
        }
        if row
            .try_get::<Option<DateTime<Utc>>, _>("disabled_at")?
            .is_some()
        {
            return Ok(None);
        }

        let user_id = UserId(row.try_get("id")?);
        let role = self
            .highest_role(user_id, workspace_id)
            .await?
            .unwrap_or(Role::Viewer);

        let token = new_token();
        sqlx::query("INSERT INTO sessions (token_hash, user_id, expires_at) VALUES ($1, $2, $3)")
            .bind(token_hash(&token).to_vec())
            .bind(uuid::Uuid::from(user_id))
            .bind(Utc::now() + SESSION_TTL)
            .execute(self.pool())
            .await?;

        Ok(Some((
            token,
            Principal {
                user_id,
                workspace_id,
                display_name: row.try_get("display_name")?,
                role,
            },
        )))
    }

    /// 用 token 换回调用方身份。过期或不存在都返回 `None`。
    pub async fn principal_for(&self, token: &str) -> Result<Option<Principal>, StoreError> {
        let row = sqlx::query(
            "SELECT u.id, u.workspace_id, u.display_name
             FROM sessions s
             JOIN users u ON u.id = s.user_id
             WHERE s.token_hash = $1 AND s.expires_at > now() AND u.disabled_at IS NULL",
        )
        .bind(token_hash(token).to_vec())
        .fetch_optional(self.pool())
        .await?;

        let Some(row) = row else { return Ok(None) };
        let user_id = UserId(row.try_get("id")?);
        let workspace_id = WorkspaceId(row.try_get("workspace_id")?);
        Ok(Some(Principal {
            user_id,
            workspace_id,
            display_name: row.try_get("display_name")?,
            role: self
                .highest_role(user_id, workspace_id)
                .await?
                .unwrap_or(Role::Viewer),
        }))
    }

    /// 列出 workspace 里的所有用户。
    pub async fn list_users(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<Vec<UserRecord>, StoreError> {
        // 角色和会话数各连一次：用户数量是几十的量级，不值得为它做分页
        let rows = sqlx::query(
            "SELECT u.id, u.email, u.display_name, u.disabled_at, u.created_at,
                    (SELECT count(*) FROM sessions s
                      WHERE s.user_id = u.id AND s.expires_at > now()) AS active_sessions,
                    -- **不能用 max(role)**：那是字符串序，viewer > operator > admin，
                    -- 和角色高低正好相反。一个同时绑了 admin 和 viewer 的人
                    -- 会在列表里显示成 viewer。
                    (SELECT rb.role FROM role_bindings rb
                      WHERE rb.user_id = u.id AND rb.workspace_id = u.workspace_id
                      ORDER BY CASE rb.role
                                 WHEN 'admin' THEN 3
                                 WHEN 'operator' THEN 2
                                 ELSE 1
                               END DESC
                      LIMIT 1) AS role
             FROM users u
             WHERE u.workspace_id = $1
             ORDER BY u.created_at",
        )
        .bind(uuid::Uuid::from(workspace_id))
        .fetch_all(self.pool())
        .await?;

        rows.iter()
            .map(|row| {
                Ok(UserRecord {
                    id: UserId(row.try_get("id")?),
                    email: row.try_get("email")?,
                    display_name: row.try_get("display_name")?,
                    // 一个绑定都没有时兜底成权限最小的那档
                    role: row
                        .try_get::<Option<String>, _>("role")?
                        .and_then(|r| Role::parse(&r))
                        .unwrap_or(Role::Viewer),
                    disabled_at: row.try_get("disabled_at")?,
                    created_at: row.try_get("created_at")?,
                    active_sessions: row.try_get("active_sessions")?,
                })
            })
            .collect()
    }

    /// 改角色。旧绑定整个换掉，不是叠加。
    ///
    /// **不需要吊销会话**：`principal_for` 每次请求都重算角色，降级立刻生效。
    pub async fn set_role(
        &self,
        workspace_id: WorkspaceId,
        user_id: UserId,
        role: Role,
    ) -> Result<bool, StoreError> {
        let mut tx = self.pool().begin().await?;
        let exists = sqlx::query("SELECT 1 AS x FROM users WHERE id = $1 AND workspace_id = $2")
            .bind(uuid::Uuid::from(user_id))
            .bind(uuid::Uuid::from(workspace_id))
            .fetch_optional(&mut *tx)
            .await?
            .is_some();
        if !exists {
            return Ok(false);
        }
        sqlx::query("DELETE FROM role_bindings WHERE user_id = $1 AND workspace_id = $2")
            .bind(uuid::Uuid::from(user_id))
            .bind(uuid::Uuid::from(workspace_id))
            .execute(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO role_bindings (user_id, workspace_id, role) VALUES ($1, $2, $3)")
            .bind(uuid::Uuid::from(user_id))
            .bind(uuid::Uuid::from(workspace_id))
            .bind(role.as_str())
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(true)
    }

    /// 停用或恢复一个用户。停用时连带吊销他所有的会话。
    ///
    /// `principal_for` 本来就会挡住 disabled 的用户，删会话是第二道：
    /// 万一哪天那个条件被改掉了，已经发出去的 token 也不该还能用。
    pub async fn set_user_disabled(
        &self,
        workspace_id: WorkspaceId,
        user_id: UserId,
        disabled: bool,
    ) -> Result<bool, StoreError> {
        let mut tx = self.pool().begin().await?;
        let updated = sqlx::query(
            "UPDATE users SET disabled_at = CASE WHEN $3 THEN now() ELSE NULL END
             WHERE id = $1 AND workspace_id = $2
             RETURNING id",
        )
        .bind(uuid::Uuid::from(user_id))
        .bind(uuid::Uuid::from(workspace_id))
        .bind(disabled)
        .fetch_optional(&mut *tx)
        .await?;
        if updated.is_none() {
            return Ok(false);
        }
        if disabled {
            sqlx::query("DELETE FROM sessions WHERE user_id = $1")
                .bind(uuid::Uuid::from(user_id))
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(true)
    }

    /// 吊销一个用户的所有会话，返回吊销掉的条数。
    pub async fn revoke_sessions(&self, user_id: UserId) -> Result<u64, StoreError> {
        let done = sqlx::query("DELETE FROM sessions WHERE user_id = $1")
            .bind(uuid::Uuid::from(user_id))
            .execute(self.pool())
            .await?;
        Ok(done.rows_affected())
    }

    /// 还剩几个启用状态的管理员。
    ///
    /// 用来挡住"把自己降级/停用之后没人能管了"——那只能去改库救。
    pub async fn active_admin_count(&self, workspace_id: WorkspaceId) -> Result<i64, StoreError> {
        let row = sqlx::query(
            "SELECT count(*) AS n FROM role_bindings rb
             JOIN users u ON u.id = rb.user_id
             WHERE rb.workspace_id = $1 AND rb.role = 'admin' AND u.disabled_at IS NULL",
        )
        .bind(uuid::Uuid::from(workspace_id))
        .fetch_one(self.pool())
        .await?;
        row.try_get("n").map_err(StoreError::from)
    }

    /// 清掉过期会话。返回删掉的条数。
    pub async fn purge_expired_sessions(&self) -> Result<u64, StoreError> {
        let done = sqlx::query("DELETE FROM sessions WHERE expires_at <= now()")
            .execute(self.pool())
            .await?;
        Ok(done.rows_affected())
    }

    /// 注销：删掉这一个会话。
    pub async fn logout(&self, token: &str) -> Result<(), StoreError> {
        sqlx::query("DELETE FROM sessions WHERE token_hash = $1")
            .bind(token_hash(token).to_vec())
            .execute(self.pool())
            .await?;
        Ok(())
    }

    async fn highest_role(
        &self,
        user_id: UserId,
        workspace_id: WorkspaceId,
    ) -> Result<Option<Role>, StoreError> {
        let rows =
            sqlx::query("SELECT role FROM role_bindings WHERE user_id = $1 AND workspace_id = $2")
                .bind(uuid::Uuid::from(user_id))
                .bind(uuid::Uuid::from(workspace_id))
                .fetch_all(self.pool())
                .await?;
        Ok(rows
            .iter()
            .filter_map(|r| r.try_get::<String, _>("role").ok())
            .filter_map(|r| Role::parse(&r))
            .max())
    }
}

/// 一个真实但对不上任何口令的 argon2id 哈希。
///
/// 用户不存在时拿它去比，好让两条路径耗时一致。
///
/// 运行时算出来而不是写成字面量：手写的 PHC 串一旦格式不合法，`verify` 会
/// 立刻返回 false 而**不做任何计算**——于是"用户不存在"又变回一条快路径，
/// 登录接口重新成为用户名枚举器。这个失败是静默的。
fn dummy_hash() -> &'static str {
    static DUMMY: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    DUMMY.get_or_init(|| hash_password("这个口令不属于任何人").unwrap_or_else(|_| String::new()))
}

fn hash_password(password: &str) -> Result<String, String> {
    use argon2::password_hash::PasswordHasher as _;
    // 盐由实现自己生成（getrandom feature，默认开）
    argon2::Argon2::default()
        .hash_password(password.as_bytes())
        .map(|h| h.to_string())
        .map_err(|e| e.to_string())
}

fn verify_password(password: &str, stored: &str) -> bool {
    use argon2::password_hash::PasswordVerifier as _;
    use argon2::password_hash::phc::PasswordHash;
    PasswordHash::new(stored).is_ok_and(|parsed| {
        argon2::Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok()
    })
}

/// 256 位随机 token。
fn new_token() -> String {
    use rand::Rng as _;
    let mut bytes = [0u8; 32];
    rand::rng().fill(&mut bytes);
    use std::fmt::Write as _;
    bytes.iter().fold(String::with_capacity(64), |mut acc, b| {
        let _ = write!(acc, "{b:02x}");
        acc
    })
}

fn token_hash(token: &str) -> [u8; 32] {
    use sha2::{Digest as _, Sha256};
    Sha256::digest(token.as_bytes()).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_are_ordered_so_new_tiers_do_not_need_new_checks() {
        assert!(Role::Admin.allows(Role::Viewer));
        assert!(Role::Admin.allows(Role::Operator));
        assert!(Role::Operator.allows(Role::Viewer));
        assert!(!Role::Viewer.allows(Role::Operator));
        assert!(!Role::Operator.allows(Role::Admin));
        assert!(Role::Viewer.allows(Role::Viewer));
    }

    #[test]
    fn role_seniority_is_the_opposite_of_alphabetical_order() {
        // 'admin' < 'operator' < 'viewer' 是字符串序，而权限高低正好反过来。
        // 任何拿 max(role) / ORDER BY role 取"最高角色"的 SQL 都是错的。
        let mut alphabetical = ["viewer", "operator", "admin"];
        alphabetical.sort_unstable();
        assert_eq!(alphabetical, ["admin", "operator", "viewer"]);

        let mut by_power = [Role::Viewer, Role::Operator, Role::Admin];
        by_power.sort_unstable();
        assert_eq!(by_power, [Role::Viewer, Role::Operator, Role::Admin]);
        assert_eq!(by_power.iter().max(), Some(&Role::Admin));
        assert_ne!(
            by_power.iter().max().map(|r| r.as_str()),
            alphabetical.last().copied(),
            "字符串序的最大值是 viewer，权限的最大值是 admin"
        );
    }

    #[test]
    fn role_names_round_trip_through_the_database_representation() {
        // CHECK 约束里写死了这三个字符串，对不上就写不进去
        for role in [Role::Viewer, Role::Operator, Role::Admin] {
            assert_eq!(Role::parse(role.as_str()), Some(role));
        }
        assert_eq!(Role::parse("superuser"), None);
    }

    #[test]
    fn a_password_hash_is_salted_so_two_users_with_one_password_differ() {
        let a = hash_password("同一个口令").expect("哈希");
        let b = hash_password("同一个口令").expect("哈希");
        assert_ne!(a, b, "没加盐的话，撞库能一次命中所有用同一口令的人");
        assert!(a.starts_with("$argon2id$"), "{a}");
        assert!(verify_password("同一个口令", &a));
        assert!(!verify_password("别的口令", &a));
    }

    #[test]
    fn the_dummy_hash_is_a_real_one_so_the_timing_path_really_runs() {
        // 占位哈希格式不合法的话，verify 会立刻返回 false 而不做任何计算，
        // "用户不存在"就又变回一条快路径，登录接口重新成为用户名枚举器。
        let dummy = dummy_hash();
        assert!(dummy.starts_with("$argon2id$"), "{dummy}");
        assert!(!verify_password("随便什么", dummy));

        // 两条路径的耗时要在同一个量级
        let real = hash_password("真实口令").expect("哈希");
        let t0 = std::time::Instant::now();
        let _ = verify_password("错的", &real);
        let with_user = t0.elapsed();
        let t1 = std::time::Instant::now();
        let _ = verify_password("错的", dummy);
        let without_user = t1.elapsed();
        let ratio = with_user.as_secs_f64() / without_user.as_secs_f64().max(1e-9);
        assert!(
            (0.2..5.0).contains(&ratio),
            "两条路径耗时相差 {ratio:.1} 倍，登录接口会泄漏用户是否存在"
        );
    }

    #[test]
    fn a_session_token_is_never_stored_in_the_clear() {
        let token = new_token();
        assert_eq!(token.len(), 64);
        let stored = token_hash(&token);
        assert_ne!(stored.as_slice(), token.as_bytes());
        // 同一个 token 每次算出同样的哈希，否则查不回来
        assert_eq!(stored, token_hash(&token));
        assert_ne!(stored, token_hash(&new_token()));
    }
}
