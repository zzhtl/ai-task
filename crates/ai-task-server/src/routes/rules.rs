//! 规则与 skills 的配置接口。

use ai_task_core::PolicyRule;
use ai_task_proto::{FieldError, Page};
use ai_task_store::{NewRule, NewSkill, RuleKind};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::{Json, response::IntoResponse};
use serde::{Deserialize, Serialize};

use crate::error::AppError;
use crate::state::AppState;

/// `POST /api/v1/rules`
///
/// 两种规则共用这一个接口，但语义完全不同：
/// `prompt` 只影响模型倾向，`policy` 在工具调用边界强制。
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CreateRule {
    /// 软规则：一段注入 system prompt 的文本。
    Prompt {
        name: String,
        text: String,
        #[serde(default)]
        global: bool,
        #[serde(default)]
        priority: i32,
    },
    /// 硬策略：声明式匹配器。
    Policy {
        name: String,
        #[serde(flatten)]
        rule: PolicyRuleBody,
        #[serde(default)]
        global: bool,
        #[serde(default)]
        priority: i32,
    },
}

/// 硬策略的主体（`id` / `name` / `priority` 由表列承载，不在 body 里）。
#[derive(Debug, Deserialize, Serialize)]
pub struct PolicyRuleBody {
    pub effect: ai_task_proto::PolicyEffect,
    pub reason: String,
    #[serde(default)]
    pub scope: ai_task_core::policy::RuleScope,
    #[serde(rename = "match")]
    pub matcher: ai_task_core::policy::ToolMatcher,
}

#[derive(Debug, Serialize)]
pub struct RuleCreated {
    pub id: String,
}

#[derive(Debug, Serialize)]
pub struct RuleView {
    pub id: String,
    pub name: String,
    /// `prompt` 软规则 / `policy` 硬策略。两者语义完全不同，界面上要分开显示。
    pub kind: String,
    /// `global` 对所有任务生效 / `task` 只对显式挂上它的任务生效。
    pub scope: String,
    pub spec: serde_json::Value,
    pub priority: i32,
    pub enabled: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// `GET /api/v1/rules` —— 列出全部规则。
///
/// 建完看不见的规则等于没有：既不知道有哪些能挂到任务上，也不知道某条为什么
/// 没生效。不分页，规则是人手工维护的东西。
pub async fn list_rules(State(state): State<AppState>) -> Result<impl IntoResponse, AppError> {
    let items: Vec<RuleView> = state
        .store
        .list_rules(state.workspace_id)
        .await?
        .into_iter()
        .map(|r| RuleView {
            id: r.id.to_string(),
            name: r.name,
            kind: r.kind,
            scope: r.scope,
            spec: r.spec,
            priority: r.priority,
            enabled: r.enabled,
            created_at: r.created_at,
        })
        .collect();
    Ok(Json(serde_json::json!({ "items": items })))
}

#[derive(Debug, Deserialize)]
pub struct SetEnabled {
    pub enabled: bool,
}

/// `PUT /api/v1/rules/{id}/enabled` —— 停用 / 启用一条规则。
///
/// 只有停用，没有删除：规则文本进过 `runs.rules_hash`，删掉之后历史 run 就
/// 解释不了了，而"当时是哪版规则产生了这个行为"是漂移排查的第一个问题。
pub async fn set_rule_enabled(
    State(state): State<AppState>,
    Path(id): Path<uuid::Uuid>,
    Json(body): Json<SetEnabled>,
) -> Result<impl IntoResponse, AppError> {
    if !state
        .store
        .set_rule_enabled(state.workspace_id, ai_task_proto::RuleId(id), body.enabled)
        .await?
    {
        return Err(AppError::NotFound(format!("规则 {id} 不存在")));
    }
    state
        .audit(
            "rule.enabled",
            "rule",
            id.to_string(),
            None,
            Some(serde_json::json!({ "enabled": body.enabled })),
        )
        .await;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

pub async fn create_rule(
    State(state): State<AppState>,
    Json(body): Json<CreateRule>,
) -> Result<impl IntoResponse, AppError> {
    let (name, kind, spec, global, priority) = match body {
        CreateRule::Prompt {
            name,
            text,
            global,
            priority,
        } => {
            if text.trim().is_empty() {
                return Err(validation("text", "软规则的文本不能为空"));
            }
            (
                name,
                RuleKind::Prompt,
                serde_json::json!({ "text": text }),
                global,
                priority,
            )
        }
        CreateRule::Policy {
            name,
            rule,
            global,
            priority,
        } => {
            let spec = serde_json::to_value(&rule)
                .map_err(|err| validation("match", &format!("策略无法序列化：{err}")))?;

            // 立刻试编译一次。坏正则必须在保存时就被拒——等到凌晨两点
            // 工具调用时才发现策略集装不起来，那时整个 run 都会被拒。
            let probe: PolicyRule = serde_json::from_value(with_probe_identity(spec.clone()))
                .map_err(|err| validation("match", &format!("策略结构不合法：{err}")))?;
            ai_task_core::PolicySet::compile(vec![probe], ai_task_core::DefaultPolicy::AllowAll)
                .map_err(|err| validation("match", &err.to_string()))?;

            (name, RuleKind::Policy, spec, global, priority)
        }
    };

    let id = state
        .store
        .create_rule(NewRule {
            workspace_id: state.workspace_id,
            name: name.clone(),
            spec,
            kind,
            global,
            priority,
            enabled: true,
        })
        .await
        .map_err(|err| match err {
            ai_task_store::StoreError::Conflict { .. } => {
                AppError::Conflict(format!("规则 `{name}` 已存在"))
            }
            other => AppError::Store(other),
        })?;

    // 策略是护栏。"这条 deny 是什么时候被谁改掉的"必须查得到。
    state
        .audit(
            "rule.create",
            "rule",
            id.to_string(),
            None,
            Some(serde_json::json!({ "name": name, "kind": format!("{kind:?}") })),
        )
        .await;

    Ok((
        StatusCode::CREATED,
        Json(RuleCreated { id: id.to_string() }),
    ))
}

/// `POST /api/v1/skills`
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateSkill {
    pub name: String,
    #[serde(default = "default_version")]
    pub version: String,
    /// frontmatter 里的一句话描述。渐进式披露时只有它进上下文，
    /// 所以要写清"什么时候该用这个技能"。
    pub description: String,
    pub body: String,
    #[serde(default)]
    pub files: std::collections::BTreeMap<String, String>,
}

fn default_version() -> String {
    "1".into()
}

pub async fn create_skill(
    State(state): State<AppState>,
    Json(body): Json<CreateSkill>,
) -> Result<impl IntoResponse, AppError> {
    // 技能名要拼进 `.claude/skills/<name>/` 这个路径
    if !body
        .name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        || body.name.is_empty()
    {
        return Err(validation(
            "name",
            "技能名只能包含 A-Z a-z 0-9 - _：它会成为工作目录下的一级目录名",
        ));
    }
    if body.description.trim().is_empty() {
        return Err(validation(
            "description",
            "描述不能为空：渐进式披露时模型只看得到它，没有描述就等于这个技能不会被用上",
        ));
    }

    let id = state
        .store
        .create_skill(NewSkill {
            workspace_id: state.workspace_id,
            name: body.name.clone(),
            version: body.version,
            description: body.description,
            body: body.body,
            files: body.files.into_iter().collect(),
        })
        .await
        .map_err(|err| match err {
            ai_task_store::StoreError::Conflict { id, .. } => {
                AppError::Conflict(format!("技能 `{id}` 已存在"))
            }
            other => AppError::Store(other),
        })?;

    state
        .audit(
            "skill.create",
            "skill",
            id.to_string(),
            None,
            Some(serde_json::json!({ "name": body.name })),
        )
        .await;

    Ok((
        StatusCode::CREATED,
        Json(RuleCreated { id: id.to_string() }),
    ))
}

#[derive(Debug, Serialize)]
pub struct SkillSummary {
    pub name: String,
    pub version: String,
    pub description: String,
    pub content_hash: String,
}

/// `GET /api/v1/skills`
pub async fn list_skills(
    State(state): State<AppState>,
) -> Result<Json<Page<SkillSummary>>, AppError> {
    let skills = state.store.list_skills(state.workspace_id).await?;
    Ok(Json(Page {
        items: skills
            .into_iter()
            .map(|s| SkillSummary {
                name: s.name,
                version: s.version,
                description: s.description,
                content_hash: s.content_hash,
            })
            .collect(),
        next_cursor: None,
    }))
}

/// 试编译时用的占位身份。真正的 id / name / priority 由表列承载。
fn with_probe_identity(mut spec: serde_json::Value) -> serde_json::Value {
    if let Some(obj) = spec.as_object_mut() {
        obj.insert("id".into(), serde_json::json!(ai_task_proto::RuleId::new()));
        obj.insert("name".into(), serde_json::json!("probe"));
        obj.insert("priority".into(), serde_json::json!(0));
    }
    spec
}

fn validation(field: &str, message: &str) -> AppError {
    AppError::Validation(vec![FieldError {
        field: field.into(),
        code: "invalid".into(),
        message: message.into(),
    }])
}
