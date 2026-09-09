//! 漂移指纹。
//!
//! 一个每天跑的 AI 任务，今天对，明天可能因为模型升级 / prompt 改动 /
//! 环境变化悄悄变了，**而且不报错**。这是定时 AI 任务能否进生产的分水岭。
//!
//! 做法：每个 run 记两个哈希。
//! - `fingerprint` = 这次执行的**输入条件**（任务版本 + 模型 + effort + 规则 + skills）
//! - `output_digest` = 这次执行的**结构化输出的稳定子集**
//!
//! 同 fingerprint 但 output_digest 变了 → 输入没变、输出变了 → 行为漂移。

use ai_task_proto::{DagSpec, NodeConfig};

/// 计算一次执行的输入指纹。
///
/// 只包含**会影响模型行为**的东西。刻意排除：
/// - 节点的 `host` / `limits`：换台机器跑不该算作不同的行为条件
/// - `retry` / `timeout`：它们影响的是失败处理，不是模型的输出
/// - `name` / 描述文本：纯展示
///
/// 排除得太少会让指纹每次都变（漂移检测退化成噪音），排除得太多会漏报。
#[must_use]
pub fn fingerprint(spec: &DagSpec, rules_hash: &str, skills: &[String]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"ai-task-fingerprint-v1\n");
    hasher.update(rules_hash.as_bytes());
    hasher.update(b"\n");

    // skills 排序后再进哈希：勾选顺序不同不该算作不同的条件
    let mut skills: Vec<&str> = skills.iter().map(String::as_str).collect();
    skills.sort_unstable();
    for skill in skills {
        hasher.update(skill.as_bytes());
        hasher.update(b"\0");
    }
    hasher.update(b"\n");

    // 节点按 key 排序：DAG 里节点的书写顺序不影响执行
    let mut nodes: Vec<_> = spec.nodes.iter().collect();
    nodes.sort_by(|a, b| a.key.as_str().cmp(b.key.as_str()));
    for node in nodes {
        hasher.update(node.key.as_str().as_bytes());
        hasher.update(b"\0");
        match &node.config {
            NodeConfig::Ai(ai) => {
                hasher.update(b"ai\0");
                hasher.update(ai.prompt.as_bytes());
                hasher.update(b"\0");
                hasher.update(ai.model.as_deref().unwrap_or("default").as_bytes());
                hasher.update(b"\0");
                hasher.update(format!("{:?}", ai.effort).as_bytes());
                hasher.update(b"\0");
                let mut tools = ai.tools.clone();
                tools.sort_unstable();
                hasher.update(tools.join(",").as_bytes());
            }
            NodeConfig::Shell(shell) => {
                hasher.update(b"shell\0");
                hasher.update(shell.command.as_bytes());
            }
            other => {
                // assert / approval / map：类型本身参与指纹，配置细节不参与。
                // 它们不驱动模型，改它们不会让"同样的输入得到不同的输出"。
                hasher.update(other.kind().as_bytes());
            }
        }
        hasher.update(b"\n");
    }
    hasher.finalize().to_hex().to_string()
}

/// 计算输出摘要。
///
/// **只取结构化输出的稳定子集**：递归地把对象的键排序、把数值和布尔值原样保留，
/// 而把自由文本、时间戳、id 一类的东西剔除。否则每次 run 的摘要都不一样，
/// 漂移检测就永远在告警。
#[must_use]
pub fn output_digest(output: Option<&serde_json::Value>) -> Option<String> {
    let output = output?;
    let stable = stable_subset(output)?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"ai-task-digest-v1\n");
    hasher.update(canonical(&stable).as_bytes());
    Some(hasher.finalize().to_hex().to_string())
}

/// 输出里"应该稳定"的那部分。
///
/// 判断标准：这个值变了，是不是意味着**结论**变了。
/// 数字、布尔、null 留下；数组、对象递归；字符串按 [`is_identifier_like`] 判断。
///
/// **想可靠地盯住某个结论，就用 `output_schema` 把它声明成数字或枚举。**
/// 下面这套启发式是没有 schema 时的兜底，它只能做到"多数时候对"。
fn stable_subset(value: &serde_json::Value) -> Option<serde_json::Value> {
    use serde_json::Value;
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) => Some(value.clone()),
        Value::String(s) if is_identifier_like(s) => Some(value.clone()),
        Value::String(_) => None,
        Value::Array(items) => Some(Value::Array(
            items.iter().filter_map(stable_subset).collect(),
        )),
        Value::Object(map) => {
            let kept: serde_json::Map<String, Value> = map
                .iter()
                .filter_map(|(k, v)| stable_subset(v).map(|v| (k.clone(), v)))
                .collect();
            (!kept.is_empty()).then_some(Value::Object(kept))
        }
    }
}

/// 这个字符串是"标识"还是"叙述"。
///
/// 标识（`healthy`、`web-1.prod`、`ERR_502`）变了就是结论变了，要进摘要。
/// 叙述（"发现了三个问题，建议尽快处理"）每次措辞都不同、结论却可能一样，
/// 进了摘要就等于每次都报漂移，然后这个功能会被人关掉。
///
/// **不能只按长度判**：一句中文叙述常常只有十几个字符，比一个域名还短。
/// 真正的分界是标点和空格——它们只出现在句子里。
fn is_identifier_like(text: &str) -> bool {
    const MAX_LEN: usize = 64;
    if text.is_empty() || text.chars().count() > MAX_LEN {
        return false;
    }
    !text.chars().any(|c| {
        c.is_whitespace()
            // 中英文的句读。域名里的 `.` 和路径里的 `/` 不在此列。
            || matches!(c, '。' | '，' | '、' | '；' | '！' | '？' | '：'
                         | ',' | ';' | '!' | '?')
    })
}

/// 键排序后的紧凑 JSON。`serde_json::Map` 默认保序，同样的内容不同的书写顺序
/// 会得到不同的字节串——那会让摘要对"字段顺序"敏感。
fn canonical(value: &serde_json::Value) -> String {
    use serde_json::Value;
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let body: Vec<String> = keys
                .iter()
                .map(|k| format!("{}:{}", serde_json::json!(k), canonical(&map[*k])))
                .collect();
            format!("{{{}}}", body.join(","))
        }
        Value::Array(items) => {
            let body: Vec<String> = items.iter().map(canonical).collect();
            format!("[{}]", body.join(","))
        }
        other => other.to_string(),
    }
}

/// 两次执行的结构化差异。
#[derive(Debug, Clone, PartialEq)]
pub struct Drift {
    /// 输入条件一样吗。不一样的话输出变了是**预期内**的。
    pub same_fingerprint: bool,
    /// 输出摘要一样吗。
    pub same_digest: bool,
    /// 逐字段的差异，路径 → (基线, 本次)。
    pub changes: Vec<FieldChange>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FieldChange {
    /// 点分路径，如 `errors.count`。
    pub path: String,
    pub before: Option<serde_json::Value>,
    pub after: Option<serde_json::Value>,
}

impl Drift {
    /// 该不该告警：输入条件没变，输出却变了。
    ///
    /// 输入条件变了而输出跟着变，那是**改动生效了**，不是漂移。
    #[must_use]
    pub fn is_alarming(&self) -> bool {
        self.same_fingerprint && !self.same_digest
    }
}

/// 比较基线与本次。
#[must_use]
pub fn compare(
    baseline_fingerprint: Option<&str>,
    baseline_output: Option<&serde_json::Value>,
    current_fingerprint: Option<&str>,
    current_output: Option<&serde_json::Value>,
) -> Drift {
    let mut changes = Vec::new();
    diff_into(
        "",
        baseline_output.and_then(stable_subset).as_ref(),
        current_output.and_then(stable_subset).as_ref(),
        &mut changes,
    );
    changes.sort_by(|a, b| a.path.cmp(&b.path));
    Drift {
        same_fingerprint: baseline_fingerprint.is_some()
            && baseline_fingerprint == current_fingerprint,
        same_digest: output_digest(baseline_output) == output_digest(current_output),
        changes,
    }
}

fn diff_into(
    path: &str,
    before: Option<&serde_json::Value>,
    after: Option<&serde_json::Value>,
    out: &mut Vec<FieldChange>,
) {
    use serde_json::Value;
    // 上限：一个爆改的输出不该产出一万条 diff 把界面拖死
    const MAX_CHANGES: usize = 200;
    if out.len() >= MAX_CHANGES {
        return;
    }
    match (before, after) {
        (Some(Value::Object(a)), Some(Value::Object(b))) => {
            let mut keys: Vec<&String> = a.keys().chain(b.keys()).collect();
            keys.sort();
            keys.dedup();
            for key in keys {
                let child = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                diff_into(&child, a.get(key), b.get(key), out);
            }
        }
        (Some(Value::Array(a)), Some(Value::Array(b))) if a.len() == b.len() => {
            for (i, (x, y)) in a.iter().zip(b).enumerate() {
                diff_into(&format!("{path}[{i}]"), Some(x), Some(y), out);
            }
        }
        (x, y) if x == y => {}
        (x, y) => out.push(FieldChange {
            path: if path.is_empty() {
                "$".into()
            } else {
                path.into()
            },
            before: x.cloned(),
            after: y.cloned(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn spec_with(prompt: &str, model: Option<&str>) -> DagSpec {
        use ai_task_proto::{AiNode, ExecutorKind, NodeKey, NodeSpec, OnFailure, RetryPolicy};
        DagSpec {
            nodes: vec![NodeSpec {
                key: NodeKey::parse("n").expect("key"),
                name: None,
                config: NodeConfig::Ai(AiNode {
                    prompt: prompt.into(),
                    executor: ExecutorKind::ClaudeCode,
                    model: model.map(str::to_owned),
                    effort: None,
                    skills: vec![],
                    tools: vec![],
                    max_turns: None,
                    budget_usd: None,
                }),
                inputs: Default::default(),
                output_schema: None,
                retry: RetryPolicy::default(),
                on_failure: OnFailure::default(),
                timeout_s: None,
                host: None,
                limits: None,
            }],
            edges: vec![],
            input_schema: None,
            budget_usd: None,
            timeout_s: None,
        }
    }

    #[test]
    fn changing_the_prompt_changes_the_fingerprint() {
        let a = fingerprint(&spec_with("巡检", None), "r1", &[]);
        let b = fingerprint(&spec_with("巡检并汇总", None), "r1", &[]);
        assert_ne!(a, b);
    }

    #[test]
    fn changing_the_model_changes_the_fingerprint() {
        let a = fingerprint(&spec_with("巡检", Some("claude-sonnet-5")), "r1", &[]);
        let b = fingerprint(&spec_with("巡检", Some("claude-opus-5")), "r1", &[]);
        assert_ne!(
            a, b,
            "换模型必须换指纹，否则模型升级导致的漂移会被当成行为异常"
        );
    }

    #[test]
    fn changing_the_rules_changes_the_fingerprint() {
        let spec = spec_with("巡检", None);
        assert_ne!(fingerprint(&spec, "r1", &[]), fingerprint(&spec, "r2", &[]));
    }

    #[test]
    fn skill_selection_order_does_not_change_the_fingerprint() {
        // 勾选顺序不同不该算作不同的执行条件
        let spec = spec_with("巡检", None);
        assert_eq!(
            fingerprint(&spec, "r", &["b".into(), "a".into()]),
            fingerprint(&spec, "r", &["a".into(), "b".into()])
        );
        assert_ne!(
            fingerprint(&spec, "r", &["a".into()]),
            fingerprint(&spec, "r", &["a".into(), "b".into()])
        );
    }

    #[test]
    fn changing_where_it_runs_does_not_change_the_fingerprint() {
        // 换台机器跑不该算作"行为条件变了"——否则把任务挪一台机器
        // 就会丢掉所有历史基线
        let mut a = spec_with("巡检", None);
        let mut b = spec_with("巡检", None);
        a.nodes[0].host = Some(ai_task_proto::HostSelector::Local);
        b.nodes[0].host = Some(ai_task_proto::HostSelector::Host {
            host_id: ai_task_proto::HostId::new(),
        });
        assert_eq!(fingerprint(&a, "r", &[]), fingerprint(&b, "r", &[]));
    }

    #[test]
    fn free_text_does_not_enter_the_digest() {
        // AI 每次措辞都不同，但结论可能完全一样。把叙述算进摘要，
        // 漂移检测就永远在告警，然后被人关掉。
        let a = json!({"count": 3, "summary": "发现了三个问题，建议尽快处理相关配置项。"});
        let b = json!({"count": 3, "summary": "共计三处异常，建议及时修复配置。"});
        assert_eq!(output_digest(Some(&a)), output_digest(Some(&b)));
    }

    #[test]
    fn a_changed_number_does_change_the_digest() {
        let a =
            json!({"count": 3, "summary": "叙述性的一大段文字，长度超过三十二个字符所以会被剔除"});
        let b =
            json!({"count": 4, "summary": "叙述性的一大段文字，长度超过三十二个字符所以会被剔除"});
        assert_ne!(output_digest(Some(&a)), output_digest(Some(&b)));
    }

    #[test]
    fn identifier_like_strings_are_kept_because_they_carry_the_conclusion() {
        for (a, b) in [
            ("healthy", "degraded"),
            ("web-1.prod.internal", "web-2.prod.internal"),
            ("/var/log/app.log", "/var/log/other.log"),
            ("ERR_502", "ERR_503"),
        ] {
            assert_ne!(
                output_digest(Some(&json!({ "v": a }))),
                output_digest(Some(&json!({ "v": b }))),
                "{a} 和 {b} 应当被当作有判别意义的值"
            );
        }
    }

    #[test]
    fn a_short_chinese_sentence_is_narrative_not_an_identifier() {
        // 一句中文叙述常常只有十几个字符，比一个域名还短。
        // 只按长度判会把它当成枚举值，于是每次 run 都报漂移。
        assert!(!is_identifier_like("发现了三个问题，建议尽快处理"));
        assert!(!is_identifier_like("共计三处异常。"));
        assert!(!is_identifier_like("disk is full"));
        assert!(is_identifier_like("healthy"));
        assert!(is_identifier_like("已完成"));
        assert!(!is_identifier_like(""), "空串不是标识");
        assert!(!is_identifier_like(&"x".repeat(65)), "太长的一律当叙述");
    }

    #[test]
    fn field_order_does_not_change_the_digest() {
        let a = json!({"a": 1, "b": 2});
        let b = json!({"b": 2, "a": 1});
        assert_eq!(output_digest(Some(&a)), output_digest(Some(&b)));
    }

    #[test]
    fn drift_only_alarms_when_the_inputs_were_identical() {
        let base = json!({"errors": 2});
        let now = json!({"errors": 5});

        // 输入没变、输出变了 → 漂移
        let drift = compare(Some("fp1"), Some(&base), Some("fp1"), Some(&now));
        assert!(drift.is_alarming());
        assert_eq!(drift.changes.len(), 1);
        assert_eq!(drift.changes[0].path, "errors");
        assert_eq!(drift.changes[0].before, Some(json!(2)));
        assert_eq!(drift.changes[0].after, Some(json!(5)));

        // 改了 prompt 之后输出跟着变，那是改动生效了，不是漂移
        let expected = compare(Some("fp1"), Some(&base), Some("fp2"), Some(&now));
        assert!(!expected.is_alarming());
        assert!(!expected.same_fingerprint);
        assert_eq!(expected.changes.len(), 1, "但差异照样要列出来");
    }

    #[test]
    fn a_nested_change_reports_its_full_path() {
        let a = json!({"hosts": {"web-1": {"cpu": 10}}});
        let b = json!({"hosts": {"web-1": {"cpu": 90}}});
        let drift = compare(Some("fp"), Some(&a), Some("fp"), Some(&b));
        assert_eq!(drift.changes[0].path, "hosts.web-1.cpu");
    }

    #[test]
    fn an_added_field_is_reported_as_appearing_from_nothing() {
        let a = json!({"errors": 1});
        let b = json!({"errors": 1, "warnings": 3});
        let drift = compare(Some("fp"), Some(&a), Some("fp"), Some(&b));
        assert_eq!(drift.changes.len(), 1);
        assert_eq!(drift.changes[0].path, "warnings");
        assert_eq!(drift.changes[0].before, None);
        assert_eq!(drift.changes[0].after, Some(json!(3)));
    }

    #[test]
    fn a_missing_baseline_never_alarms() {
        // 第一次跑没有基线，不该报"漂移"
        let now = json!({"errors": 5});
        assert!(!compare(None, None, Some("fp"), Some(&now)).is_alarming());
    }

    #[test]
    fn the_diff_is_bounded() {
        // 输出爆改时不能产出一万条 diff 把界面拖死
        let build = |offset: i64| -> serde_json::Value {
            serde_json::Value::Object(
                (0..1000)
                    .map(|i: i64| (i.to_string(), json!(i + offset)))
                    .collect(),
            )
        };
        let a = build(0);
        let b = build(1);
        let drift = compare(Some("fp"), Some(&a), Some("fp"), Some(&b));
        assert!(drift.changes.len() <= 200, "{}", drift.changes.len());
    }
}
