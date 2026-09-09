//! 规则与 skills。
//!
//! 规则分两种，存在同一张表里但用途完全不同：
//! - `prompt`（软规则）：注入 system prompt，只影响模型倾向
//! - `policy`（硬策略）：在工具调用边界强制，模型影响不到它
//!
//! 见 `ai_task_core::policy` 里对这个分层的说明。

use ai_task_core::PolicyRule;
use ai_task_proto::{RuleId, SkillId, WorkspaceId};
use sqlx::Row;

use crate::{Store, StoreError};

/// 一条软规则（注入 prompt 的那种）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptRule {
    pub id: RuleId,
    pub name: String,
    pub text: String,
    pub priority: i32,
    /// 全局规则对本 workspace 的所有任务生效。
    pub global: bool,
}

/// 一个技能包。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    pub id: SkillId,
    pub name: String,
    pub version: String,
    /// frontmatter 里的一句话描述。渐进式披露时只有它进上下文。
    pub description: String,
    pub body: String,
    /// 附带文件：相对路径 → 内容。
    pub files: Vec<(String, String)>,
    pub content_hash: String,
}

/// 新建规则的入参。
#[derive(Debug, Clone)]
pub struct NewRule {
    pub workspace_id: WorkspaceId,
    pub name: String,
    /// `prompt` 规则给文本，`policy` 规则给 [`PolicyRule`] 的 JSON。
    pub spec: serde_json::Value,
    pub kind: RuleKind,
    pub global: bool,
    pub priority: i32,
    pub enabled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleKind {
    Prompt,
    Policy,
}

impl RuleKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Prompt => "prompt",
            Self::Policy => "policy",
        }
    }
}

#[derive(Debug, Clone)]
pub struct NewSkill {
    pub workspace_id: WorkspaceId,
    pub name: String,
    pub version: String,
    pub description: String,
    pub body: String,
    pub files: Vec<(String, String)>,
}

impl Store {
    pub async fn create_rule(&self, new: NewRule) -> Result<RuleId, StoreError> {
        let id = RuleId::new();
        sqlx::query(
            "INSERT INTO rules (id, workspace_id, name, kind, scope, spec, priority, enabled)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8)",
        )
        .bind(uuid::Uuid::from(id))
        .bind(uuid::Uuid::from(new.workspace_id))
        .bind(&new.name)
        .bind(new.kind.as_str())
        .bind(if new.global { "global" } else { "task" })
        .bind(&new.spec)
        .bind(new.priority)
        .bind(new.enabled)
        .execute(self.pool())
        .await
        .map_err(|err| duplicate(err, "rule name", &new.name))?;
        Ok(id)
    }

    /// 取一次 run 要用的全部硬策略。
    ///
    /// 全局规则始终生效；任务级规则按名字挑。任务级的优先级统一加 1000，
    /// 保证它能覆盖同名的全局规则——「针对这个任务的特例」就该压过通用规则。
    pub async fn policy_rules_for(
        &self,
        workspace_id: WorkspaceId,
        task_rule_names: &[String],
    ) -> Result<Vec<PolicyRule>, StoreError> {
        let rows = sqlx::query(
            "SELECT id, name, spec, priority, scope
             FROM rules
             WHERE workspace_id = $1 AND kind = 'policy' AND enabled
               AND (scope = 'global' OR name = ANY($2))
             ORDER BY priority DESC, name",
        )
        .bind(uuid::Uuid::from(workspace_id))
        .bind(task_rule_names)
        .fetch_all(self.pool())
        .await?;

        rows.into_iter()
            .map(|row| {
                let spec: serde_json::Value = row.try_get("spec")?;
                let id = RuleId(row.try_get("id")?);
                let name: String = row.try_get("name")?;
                let priority: i32 = row.try_get("priority")?;
                let scope: String = row.try_get("scope")?;

                // spec 里存的是规则主体，id / name / priority 以表列为准——
                // 这样改优先级不用重写 JSON。
                let mut rule: PolicyRule =
                    serde_json::from_value(with_identity(spec, id, &name, priority, &scope))
                        .map_err(|err| StoreError::Corrupt {
                            what: "rules.spec",
                            detail: format!("{name}: {err}"),
                        })?;
                rule.id = id;
                rule.name = name;
                rule.priority = if scope == "task" {
                    priority.saturating_add(1_000)
                } else {
                    priority
                };
                Ok(rule)
            })
            .collect()
    }

    /// 取一次 run 要注入 prompt 的软规则，按优先级排好。
    pub async fn prompt_rules_for(
        &self,
        workspace_id: WorkspaceId,
        task_rule_names: &[String],
    ) -> Result<Vec<PromptRule>, StoreError> {
        let rows = sqlx::query(
            "SELECT id, name, spec, priority, scope
             FROM rules
             WHERE workspace_id = $1 AND kind = 'prompt' AND enabled
               AND (scope = 'global' OR name = ANY($2))
             ORDER BY priority DESC, name",
        )
        .bind(uuid::Uuid::from(workspace_id))
        .bind(task_rule_names)
        .fetch_all(self.pool())
        .await?;

        rows.into_iter()
            .map(|row| {
                let spec: serde_json::Value = row.try_get("spec")?;
                let scope: String = row.try_get("scope")?;
                Ok(PromptRule {
                    id: RuleId(row.try_get("id")?),
                    name: row.try_get("name")?,
                    // 软规则的 spec 就是一段文本；容忍它被存成 {"text": "..."}
                    text: spec
                        .as_str()
                        .map(str::to_owned)
                        .or_else(|| spec.get("text")?.as_str().map(str::to_owned))
                        .unwrap_or_default(),
                    priority: row.try_get("priority")?,
                    global: scope == "global",
                })
            })
            .collect()
    }

    pub async fn create_skill(&self, new: NewSkill) -> Result<SkillId, StoreError> {
        let id = SkillId::new();
        let files = serde_json::Value::Object(
            new.files
                .iter()
                .map(|(path, content)| (path.clone(), serde_json::Value::String(content.clone())))
                .collect(),
        );
        let content_hash = skill_hash(&new.body, &new.files);

        sqlx::query(
            "INSERT INTO skills (id, workspace_id, name, version, description, body, files, content_hash)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8)",
        )
        .bind(uuid::Uuid::from(id))
        .bind(uuid::Uuid::from(new.workspace_id))
        .bind(&new.name)
        .bind(&new.version)
        .bind(&new.description)
        .bind(&new.body)
        .bind(&files)
        .bind(&content_hash)
        .execute(self.pool())
        .await
        .map_err(|err| duplicate(err, "skill", &format!("{}@{}", new.name, new.version)))?;
        Ok(id)
    }

    /// 按名字取 skill，取每个名字下版本号最大的那个。
    pub async fn skills_by_name(
        &self,
        workspace_id: WorkspaceId,
        names: &[String],
    ) -> Result<Vec<Skill>, StoreError> {
        if names.is_empty() {
            return Ok(Vec::new());
        }
        let rows = sqlx::query(
            "SELECT DISTINCT ON (name) id, name, version, description, body, files, content_hash
             FROM skills
             WHERE workspace_id = $1 AND name = ANY($2)
             ORDER BY name, version DESC",
        )
        .bind(uuid::Uuid::from(workspace_id))
        .bind(names)
        .fetch_all(self.pool())
        .await?;

        rows.into_iter().map(skill_from_row).collect()
    }

    pub async fn list_skills(&self, workspace_id: WorkspaceId) -> Result<Vec<Skill>, StoreError> {
        let rows = sqlx::query(
            "SELECT DISTINCT ON (name) id, name, version, description, body, files, content_hash
             FROM skills WHERE workspace_id = $1 ORDER BY name, version DESC",
        )
        .bind(uuid::Uuid::from(workspace_id))
        .fetch_all(self.pool())
        .await?;
        rows.into_iter().map(skill_from_row).collect()
    }
}

fn skill_from_row(row: sqlx::postgres::PgRow) -> Result<Skill, StoreError> {
    let files: serde_json::Value = row.try_get("files")?;
    Ok(Skill {
        id: SkillId(row.try_get("id")?),
        name: row.try_get("name")?,
        version: row.try_get("version")?,
        description: row.try_get("description")?,
        body: row.try_get("body")?,
        files: files
            .as_object()
            .map(|map| {
                map.iter()
                    .map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_string()))
                    .collect()
            })
            .unwrap_or_default(),
        content_hash: row.try_get("content_hash")?,
    })
}

/// 把表列里的身份信息塞回 spec，好让 serde 能一次反序列化出完整的 PolicyRule。
fn with_identity(
    mut spec: serde_json::Value,
    id: RuleId,
    name: &str,
    priority: i32,
    scope: &str,
) -> serde_json::Value {
    let _ = scope;
    if let Some(obj) = spec.as_object_mut() {
        obj.insert("id".into(), serde_json::json!(id));
        obj.insert("name".into(), serde_json::json!(name));
        obj.insert("priority".into(), serde_json::json!(priority));
    }
    spec
}

/// skill 的内容指纹。同内容不同版本号也能识别为同一份。
#[must_use]
pub fn skill_hash(body: &str, files: &[(String, String)]) -> String {
    let mut sorted: Vec<_> = files.iter().collect();
    sorted.sort();
    let mut hasher = blake3::Hasher::new();
    hasher.update(body.as_bytes());
    for (path, content) in sorted {
        hasher.update(path.as_bytes());
        hasher.update(b"\0");
        hasher.update(content.as_bytes());
        hasher.update(b"\0");
    }
    hasher.finalize().to_hex()[..32].to_string()
}

fn duplicate(err: sqlx::Error, what: &'static str, id: &str) -> StoreError {
    if err.as_database_error().and_then(|e| e.code()).as_deref() == Some("23505") {
        StoreError::Conflict {
            what,
            id: id.to_string(),
        }
    } else {
        StoreError::Query(err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skill_hash_ignores_file_order_but_not_content() {
        let a = skill_hash(
            "body",
            &[("a.md".into(), "1".into()), ("b.md".into(), "2".into())],
        );
        let b = skill_hash(
            "body",
            &[("b.md".into(), "2".into()), ("a.md".into(), "1".into())],
        );
        assert_eq!(a, b, "文件顺序不改变内容");

        let c = skill_hash(
            "body",
            &[
                ("a.md".into(), "changed".into()),
                ("b.md".into(), "2".into()),
            ],
        );
        assert_ne!(a, c);
        assert_ne!(a, skill_hash("other body", &[]));
    }

    #[test]
    fn identity_columns_win_over_whatever_is_in_the_spec_json() {
        // 改优先级只用改表列，不必重写 JSON
        let spec = serde_json::json!({
            "id": "00000000-0000-0000-0000-000000000000",
            "name": "旧名字",
            "priority": -1,
            "effect": "deny",
            "reason": "x",
            "match": {"tool": "Bash"}
        });
        let id = RuleId::new();
        let merged = with_identity(spec, id, "新名字", 42, "global");
        assert_eq!(merged["name"], "新名字");
        assert_eq!(merged["priority"], 42);
        assert_eq!(merged["id"], serde_json::json!(id));
    }
}
