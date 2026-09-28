//! 规则与 skills 的配置接口。

use std::collections::HashMap;

use ai_task_core::PolicyRule;
use ai_task_proto::{
    EvaluatePolicy, FieldError, Page, PolicyDecidedBy, PolicyEffect, PolicyEvaluation, RuleId,
    SkillDetail, SkillId, TaskId, UpdateSkill,
};
use ai_task_store::{NewRule, NewSkill, RuleKind, Skill, SkillUpdate, StoreError};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use serde::{Deserialize, Serialize};

use crate::error::AppError;
use crate::extract::{Json, Path};
use crate::routes::policy::{judge, normalize_tool};
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
/// `GET /api/v1/rules`
///
/// 以前这里返回的是手拼的 `{"items": [...]}`，是全站唯一一个不走 `Page`
/// 的列表接口——客户端得为它单独写一套解析。改成标准形状。
pub async fn list_rules(State(state): State<AppState>) -> Result<Json<Page<RuleView>>, AppError> {
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
    // store 侧封顶 500 条。规则是人手写的护栏，到不了这个量级；
    // 真到了的话是配置本身出了问题，不是需要翻页。
    Ok(Json(Page {
        items,
        next_cursor: None,
    }))
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
        } => (
            name,
            RuleKind::Policy,
            compiled_policy_spec(&rule)?,
            global,
            priority,
        ),
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

/// 硬策略的 spec：序列化，并**立刻试编译一次**。
///
/// 坏正则必须在保存时就被拒——等到凌晨两点工具调用时才发现策略集装不起来，
/// 那时整个 run 都会被拒。
fn compiled_policy_spec(rule: &PolicyRuleBody) -> Result<serde_json::Value, AppError> {
    let spec = serde_json::to_value(rule)
        .map_err(|err| validation("match", &format!("策略无法序列化：{err}")))?;
    let probe: PolicyRule = serde_json::from_value(with_probe_identity(spec.clone()))
        .map_err(|err| validation("match", &format!("策略结构不合法：{err}")))?;
    ai_task_core::PolicySet::compile(vec![probe], ai_task_core::DefaultPolicy::AllowAll)
        .map_err(|err| validation("match", &err.to_string()))?;
    Ok(spec)
}

/// `PUT /api/v1/rules/{id}` 的报文。
///
/// **没有 `name`**：任务是按名字挂规则的，改名等于把它从所有任务上悄悄摘下来。
/// 种类也必须和原来一致——软规则和硬策略的 spec 结构不同，换种类等于删了重建。
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum UpdateRule {
    Prompt {
        text: String,
        #[serde(default)]
        global: bool,
        #[serde(default)]
        priority: i32,
    },
    Policy {
        #[serde(flatten)]
        rule: PolicyRuleBody,
        #[serde(default)]
        global: bool,
        #[serde(default)]
        priority: i32,
    },
}

fn kind_label(kind: &str) -> &'static str {
    if kind == "policy" {
        "硬策略"
    } else {
        "软规则"
    }
}

/// `PUT /api/v1/rules/{id}`
pub async fn update_rule(
    State(state): State<AppState>,
    Path(id): Path<uuid::Uuid>,
    Json(body): Json<UpdateRule>,
) -> Result<impl IntoResponse, AppError> {
    let rule_id = ai_task_proto::RuleId(id);
    let Some(existing) = state.store.get_rule(state.workspace_id, rule_id).await? else {
        return Err(AppError::NotFound(format!("规则 {id} 不存在")));
    };

    let (kind, spec, global, priority) = match body {
        UpdateRule::Prompt {
            text,
            global,
            priority,
        } => {
            if text.trim().is_empty() {
                return Err(validation("text", "软规则的文本不能为空"));
            }
            (
                "prompt",
                serde_json::json!({ "text": text }),
                global,
                priority,
            )
        }
        UpdateRule::Policy {
            rule,
            global,
            priority,
        } => ("policy", compiled_policy_spec(&rule)?, global, priority),
    };
    if existing.kind != kind {
        return Err(validation(
            "kind",
            &format!(
                "这是一条{}，不能改成{}。要换种类就删了重建",
                kind_label(&existing.kind),
                kind_label(kind)
            ),
        ));
    }

    state
        .store
        .update_rule(state.workspace_id, rule_id, &spec, global, priority)
        .await?;

    // 策略是护栏。"这条 deny 是什么时候被改成 allow 的"必须查得到，所以前后都记
    state
        .audit(
            "rule.update",
            "rule",
            id.to_string(),
            Some(serde_json::json!({
                "spec": existing.spec,
                "scope": existing.scope,
                "priority": existing.priority,
            })),
            Some(serde_json::json!({
                "name": existing.name,
                "spec": spec,
                "scope": if global { "global" } else { "task" },
                "priority": priority,
            })),
        )
        .await;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /api/v1/rules/{id}`
///
/// 还有任务挂着它时拒绝（409）并列出任务名。删掉之后审批记录里的 `rule_id`
/// 会置空，历史 run 的 `rules_hash` 也解释不了了——确认框里要把这两件事说出来。
pub async fn delete_rule(
    State(state): State<AppState>,
    Path(id): Path<uuid::Uuid>,
) -> Result<impl IntoResponse, AppError> {
    let rule_id = ai_task_proto::RuleId(id);
    let Some(existing) = state.store.get_rule(state.workspace_id, rule_id).await? else {
        return Err(AppError::NotFound(format!("规则 {id} 不存在")));
    };
    let mounted = state
        .store
        .tasks_using_rule(state.workspace_id, &existing.name)
        .await?;
    if !mounted.is_empty() {
        return Err(AppError::Conflict(format!(
            "还有任务挂着这条规则：{}。先在任务里取消挂载再删",
            mounted.join("、")
        )));
    }
    if !state.store.delete_rule(state.workspace_id, rule_id).await? {
        return Err(AppError::NotFound(format!("规则 {id} 不存在")));
    }
    state
        .audit(
            "rule.delete",
            "rule",
            id.to_string(),
            Some(serde_json::json!({
                "name": existing.name,
                "kind": existing.kind,
                "spec": existing.spec,
            })),
            None,
        )
        .await;
    Ok(StatusCode::NO_CONTENT)
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
    let errors = skill_errors(
        Some(&body.name),
        &body.version,
        &body.description,
        &body.files,
    );
    if !errors.is_empty() {
        return Err(AppError::Validation(errors));
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

/// 一个技能最多带几个附件。
const MAX_SKILL_FILES: usize = 100;

/// 技能的共用校验，建和改都走这里，一次把问题报全。
///
/// 名字和附件路径都要拼进 `.claude/skills/<name>/` 下面。落盘时还有一道兜底，
/// 但到那时已经是凌晨两点的一次 run 失败了——坏路径要在保存时就挡住。
fn skill_errors(
    name: Option<&str>,
    version: &str,
    description: &str,
    files: &std::collections::BTreeMap<String, String>,
) -> Vec<FieldError> {
    let mut errors = Vec::new();
    if let Some(name) = name
        && (name.is_empty()
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'))
    {
        errors.push(field_error(
            "name",
            "技能名只能包含 A-Z a-z 0-9 - _：它会成为工作目录下的一级目录名",
        ));
    }
    let version = version.trim();
    if version.is_empty() || version.chars().count() > 64 {
        errors.push(field_error("version", "版本号不能为空，最多 64 个字符"));
    }
    if description.trim().is_empty() {
        errors.push(field_error(
            "description",
            "描述不能为空：渐进式披露时模型只看得到它，没有描述就等于这个技能不会被用上",
        ));
    }
    if files.len() > MAX_SKILL_FILES {
        errors.push(field_error(
            "files",
            &format!("附件最多 {MAX_SKILL_FILES} 个"),
        ));
    }
    if let Some(bad) = files.keys().find(|path| !is_safe_skill_path(path)) {
        errors.push(field_error(
            "files",
            &format!(
                "附件路径 `{bad}` 不合法：要写成相对路径（如 ref/notes.md），不能有 `..`、空段或反斜杠，也不能叫 SKILL.md"
            ),
        ));
    }
    errors
}

/// 附件路径只能落在技能自己的目录里，而且不能盖掉生成的 `SKILL.md`。
fn is_safe_skill_path(path: &str) -> bool {
    !path.is_empty()
        && path.chars().count() <= 200
        && path != "SKILL.md"
        && !path.contains(['\\', '\0'])
        && path
            .split('/')
            .all(|segment| !segment.is_empty() && segment != "." && segment != "..")
}

fn skill_detail(skill: Skill, versions: i64, used_by: Vec<String>) -> SkillDetail {
    SkillDetail {
        id: skill.id,
        name: skill.name,
        version: skill.version,
        description: skill.description,
        body: skill.body,
        files: skill.files.into_iter().collect(),
        content_hash: skill.content_hash,
        created_at: skill.created_at,
        versions: u32::try_from(versions).unwrap_or(u32::MAX),
        used_by,
    }
}

/// ETag 是这一版的 id：每次修改都插新版本行，id 跟着变。
fn skill_etag(id: SkillId) -> HeaderMap {
    let mut headers = HeaderMap::new();
    if let Ok(value) = format!("\"{id}\"").parse() {
        headers.insert(axum::http::header::ETAG, value);
    }
    headers
}

/// `GET /api/v1/skills/{name}` —— 最新版全文，外加谁在用它。
pub async fn get_skill(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let Some((skill, versions)) = state.store.latest_skill(state.workspace_id, &name).await? else {
        return Err(AppError::NotFound(format!("技能 `{name}` 不存在")));
    };
    let used_by = state
        .store
        .tasks_using_skill(state.workspace_id, &name)
        .await?;
    Ok((
        skill_etag(skill.id),
        Json(skill_detail(skill, versions, used_by)),
    ))
}

/// `PUT /api/v1/skills/{name}` —— 改一个技能。内容变了才插新版本。
///
/// `If-Match` 可以不带，和任务一致；**带了就必须是 ETag**——
/// 解析不出来时按不匹配处理，不能悄悄当成没带，否则等于把并发保护关掉了。
pub async fn update_skill(
    State(state): State<AppState>,
    Path(name): Path<String>,
    headers: HeaderMap,
    Json(body): Json<UpdateSkill>,
) -> Result<impl IntoResponse, AppError> {
    let errors = skill_errors(None, &body.version, &body.description, &body.files);
    if !errors.is_empty() {
        return Err(AppError::Validation(errors));
    }
    let expected = match headers.get(axum::http::header::IF_MATCH) {
        None => None,
        Some(value) if value.as_bytes() == b"*" => None,
        Some(value) => Some(
            value
                .to_str()
                .ok()
                .map(|v| v.trim().trim_start_matches("W/").trim_matches('"'))
                .and_then(|v| v.parse::<uuid::Uuid>().ok())
                .map(SkillId)
                .ok_or_else(|| {
                    AppError::PreconditionFailed("If-Match 不是这个技能的 ETag".into())
                })?,
        ),
    };

    let version = body.version.trim().to_owned();
    let outcome = state
        .store
        .update_skill(
            NewSkill {
                workspace_id: state.workspace_id,
                name: name.clone(),
                version: version.clone(),
                description: body.description,
                body: body.body,
                files: body.files.into_iter().collect(),
            },
            expected,
        )
        .await
        .map_err(|err| match err {
            StoreError::NotFound { .. } => AppError::NotFound(format!("技能 `{name}` 不存在")),
            StoreError::Conflict { what: "skill", .. } => AppError::PreconditionFailed(format!(
                "技能 `{name}` 在你打开之后被改过，重新加载再改"
            )),
            StoreError::Conflict {
                what: "skill version",
                ..
            } => AppError::Validation(vec![field_error(
                "version",
                &format!("版本 {version} 已经用过，换一个版本号"),
            )]),
            other => AppError::Store(other),
        })?;

    if let SkillUpdate::Created { previous, current } = &outcome {
        state
            .audit(
                "skill.update",
                "skill",
                name.clone(),
                Some(serde_json::json!({
                    "id": previous.id,
                    "version": previous.version,
                    "content_hash": previous.content_hash,
                })),
                Some(serde_json::json!({
                    "name": name,
                    "id": current.id,
                    "version": current.version,
                    "content_hash": current.content_hash,
                })),
            )
            .await;
    }

    let Some((skill, versions)) = state.store.latest_skill(state.workspace_id, &name).await? else {
        return Err(AppError::NotFound(format!("技能 `{name}` 不存在")));
    };
    let used_by = state
        .store
        .tasks_using_skill(state.workspace_id, &name)
        .await?;
    Ok((
        skill_etag(skill.id),
        Json(skill_detail(skill, versions, used_by)),
    ))
}

/// `DELETE /api/v1/skills/{name}` —— 删掉这个技能的全部版本。
///
/// 还有任务的当前版本在用它时拒绝（409）并列出任务名：删掉之后那些任务下一次跑
/// 就少了这份说明，而且不会报错——这比拒绝删除糟糕得多。
pub async fn delete_skill(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let Some((latest, versions)) = state.store.latest_skill(state.workspace_id, &name).await?
    else {
        return Err(AppError::NotFound(format!("技能 `{name}` 不存在")));
    };
    let used_by = state
        .store
        .tasks_using_skill(state.workspace_id, &name)
        .await?;
    if !used_by.is_empty() {
        return Err(AppError::Conflict(format!(
            "还有任务在用这个技能：{}。先在任务里去掉它再删",
            used_by.join("、")
        )));
    }
    state.store.delete_skill(state.workspace_id, &name).await?;
    state
        .audit(
            "skill.delete",
            "skill",
            name.clone(),
            Some(serde_json::json!({
                "name": name,
                "versions": versions,
                "version": latest.version,
                "content_hash": latest.content_hash,
            })),
            None,
        )
        .await;
    Ok(StatusCode::NO_CONTENT)
}

/// 试算时一次最多带几个主机 tag。真实主机上的 tag 也就几个，这是防手滑的上限。
const MAX_EVAL_TAGS: usize = 50;

/// `POST /api/v1/rules/evaluate` —— 拿一次假想的工具调用试算硬策略。
///
/// 和真正的工具调用走同一个 [`judge`]，规则也按同一个口径加载：已启用的全局规则，
/// 加上任务挂载的（任务级优先级 +1000）。**不写事件、不建审批、不记审计**——
/// 它只回答"要是现在这样调，会怎样"。
pub async fn evaluate(
    State(state): State<AppState>,
    Json(body): Json<EvaluatePolicy>,
) -> Result<Json<PolicyEvaluation>, AppError> {
    let mut errors = Vec::new();
    let tool = body.tool.trim();
    if tool.is_empty() {
        errors.push(field_error("tool", "工具名不能为空"));
    } else if tool.chars().count() > 200 {
        errors.push(field_error("tool", "工具名不能超过 200 个字符"));
    }
    if !body.input.is_object() {
        errors.push(field_error(
            "input",
            "参数必须是 JSON 对象，和模型发出的 tool_input 同形",
        ));
    }
    if body.host_tags.len() > MAX_EVAL_TAGS
        || body.host_tags.iter().any(|t| t.chars().count() > 100)
    {
        errors.push(field_error(
            "host_tags",
            &format!("最多 {MAX_EVAL_TAGS} 个 tag，每个不超过 100 个字符"),
        ));
    }
    // 别的 workspace 的任务和不存在的任务一样处理：都报在字段上
    let task = match body.task_id {
        Some(id) => match state.store.get_task(state.workspace_id, id).await {
            Ok(task) => Some(task),
            Err(ai_task_store::StoreError::NotFound { .. }) => {
                errors.push(field_error("task_id", "任务不存在"));
                None
            }
            Err(other) => return Err(other.into()),
        },
        None => None,
    };
    if !errors.is_empty() {
        return Err(AppError::Validation(errors));
    }

    let attached = match &task {
        Some(task) => {
            state
                .store
                .get_task_version(task.current_version_id)
                .await?
                .rules
        }
        None => Vec::new(),
    };
    let rules = state
        .store
        .policy_rules_for(state.workspace_id, &attached)
        .await?;
    let considered = u32::try_from(rules.len()).unwrap_or(u32::MAX);
    let names: HashMap<RuleId, String> = rules.iter().map(|r| (r.id, r.name.clone())).collect();
    // 不指定任务时用一个不属于任何任务的 id：只对特定任务生效的规则就不会命中
    let task_id = task.map_or_else(TaskId::new, |t| t.id);

    let evaluation = match judge(
        rules,
        tool,
        &body.input,
        &body.host_tags,
        task_id,
        body.dry_run,
    ) {
        Ok(judgement) => PolicyEvaluation {
            effect: judgement.effect,
            policy_effect: judgement.policy_effect,
            decided_by: if judgement.rule_id.is_some() {
                PolicyDecidedBy::Rule
            } else {
                PolicyDecidedBy::Default
            },
            rule_name: judgement.rule_id.and_then(|id| names.get(&id).cloned()),
            rule_id: judgement.rule_id,
            reason: judgement.reason,
            tool: judgement.tool,
            considered,
        },
        // 真实调用时这里是 500，hook 按失败关门——效果就是所有工具调用都被拒。
        // 试算如实说出来，而不是报一个让人以为是试算自己坏了的 500。
        Err(err) => PolicyEvaluation {
            effect: PolicyEffect::Deny,
            policy_effect: PolicyEffect::Deny,
            decided_by: PolicyDecidedBy::InvalidRules,
            rule_id: None,
            rule_name: None,
            reason: format!("策略集装不起来，真实调用时所有工具调用都会被拒：{err}"),
            tool: normalize_tool(tool).to_owned(),
            considered,
        },
    };
    Ok(Json(evaluation))
}

fn field_error(field: &str, message: &str) -> FieldError {
    FieldError {
        field: field.into(),
        code: "invalid".into(),
        message: message.into(),
    }
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
    AppError::Validation(vec![field_error(field, message)])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skill_files_must_stay_inside_the_skill_directory() {
        for ok in ["a.md", "ref/notes.md", "scripts/check/run.sh", "skill.md"] {
            assert!(is_safe_skill_path(ok), "{ok} 应当合法");
        }
        for bad in [
            "",
            "/etc/passwd",
            "../outside",
            "ref/../../x",
            "ref//a.md",
            "ref/",
            "./a.md",
            "SKILL.md",
            "ref\\a.md",
            "a\0b",
        ] {
            assert!(!is_safe_skill_path(bad), "{bad:?} 应当被挡住");
        }
    }

    #[test]
    fn skill_errors_are_reported_all_at_once() {
        let files = std::collections::BTreeMap::from([("../x".to_owned(), String::new())]);
        let fields: Vec<String> = skill_errors(Some("bad name"), " ", "", &files)
            .into_iter()
            .map(|e| e.field)
            .collect();
        assert_eq!(fields, ["name", "version", "description", "files"]);
        assert!(skill_errors(Some("ok-name"), "2", "什么时候用", &Default::default()).is_empty());
    }
}
