//! 硬策略引擎。
//!
//! 这是「策略在执行边界强制，不在 prompt 里祈求」的落点。规则分两层，混为一谈
//! 是这类系统最容易犯的致命错误：
//!
//! - **软规则**注入 system prompt，只影响模型的倾向。工具结果是不可信输入，
//!   它能影响模型对 prompt 的遵守程度。
//! - **硬策略**（本模块）在每次工具调用前拦截，返回 allow / deny / ask。
//!   它跑在模型之外，**上下文里的任何内容都影响不到它**。
//!
//! 表达式刻意只做声明式匹配（工具名 + 参数上的 regex/glob/包含），不上通用
//! 脚本语言：规则要可审计、可静态分析，而且规则本身写不出 RCE。
//!
//! 本模块零 IO，全部逻辑可单测。

use ai_task_proto::{PolicyEffect, RuleId, TaskId};
use serde::{Deserialize, Serialize};

/// 已知的只读工具。
///
/// **不在这张表里的一律按写类对待**（fail closed）。新增一个能改文件的工具时
/// 忘了更新策略，后果是它被默认放行——所以宁可把未知工具当危险的。
const READ_ONLY_TOOLS: &[&str] = &[
    "Read",
    "Glob",
    "Grep",
    "WebFetch",
    "WebSearch",
    "TodoWrite",
    // 模型交还结果的通道，不碰外界任何东西。
    //
    // **影子执行下必须放行。**它被当成写类工具的话，dry_run 的 run 永远
    // 产不出结构化输出——而影子执行的全部意义就是拿输出和基线比。
    // 实测过一次：模型反复重试这个工具，烧掉 $0.058 后 run 以"输出不符合契约"失败。
    "StructuredOutput",
    "remote_read",
    "remote_glob",
    "remote_grep",
];

/// 单条规则的正则编译上限，防止一个巨大的模式吃光内存。
///
/// 匹配本身不需要防 ReDoS：`regex` crate 保证线性时间。
const REGEX_SIZE_LIMIT: usize = 64 * 1024;

/// 一条硬策略规则（持久化形态）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyRule {
    pub id: RuleId,
    pub name: String,
    /// 数值大的先判，首个命中生效。
    #[serde(default)]
    pub priority: i32,
    pub effect: PolicyEffect,
    /// 拒绝原因。会作为 `tool_result` 回给模型，让它自己调整而不是直接失败，
    /// 所以要写成人和模型都能理解的一句话。
    pub reason: String,
    #[serde(default)]
    pub scope: RuleScope,
    #[serde(rename = "match")]
    pub matcher: ToolMatcher,
}

/// 规则的生效范围。空集合表示「不限」。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleScope {
    /// 只对带这些 tag 的主机生效。
    #[serde(default)]
    pub host_tags: Vec<String>,
    /// 只对这些任务生效。
    #[serde(default)]
    pub task_ids: Vec<TaskId>,
}

/// 工具调用的匹配条件。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolMatcher {
    /// 工具名。`None` 表示任意工具。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    /// 从 `tool_input` 里取哪个字段来匹配。
    /// `None` 时匹配整个 input 序列化后的 JSON 文本。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arg: Option<String>,
    /// 任一命中即算匹配。空列表表示「只要工具名对上就匹配」。
    #[serde(default)]
    pub any_of: Vec<Pattern>,
}

/// 参数上的匹配模式。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Pattern {
    /// 正则。`regex` crate 语法，线性时间匹配。
    Regex(String),
    /// shell 风格通配（`*` 任意多个字符、`?` 单个字符）。内部翻译成正则。
    Glob(String),
    /// 子串包含。
    Contains(String),
    /// 完全相等。
    Equals(String),
}

/// 编译后的规则集。正则只在装载时编译一次，不在每次工具调用时重编。
#[derive(Debug)]
pub struct PolicySet {
    rules: Vec<Compiled>,
    /// 没有任何规则命中时的兜底。
    default_effect: DefaultPolicy,
}

#[derive(Debug)]
struct Compiled {
    rule: PolicyRule,
    /// 与 `rule.matcher.any_of` 一一对应。
    patterns: Vec<regex::Regex>,
}

/// 没有规则命中时怎么办。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefaultPolicy {
    /// 只读工具放行；写类工具在带 `prod` tag 的主机上转人工审批。
    ///
    /// 这是默认档：既不会让日常只读操作寸步难行，也不会让 AI 在生产机上
    /// 无声无息地改东西。
    GuardProduction,
    /// 全部放行。只适合完全隔离的沙箱。
    AllowAll,
    /// 写类工具一律拒绝。
    DenyWrites,
}

/// 一次待判决的工具调用。
#[derive(Debug, Clone)]
pub struct ToolCall<'a> {
    pub tool: &'a str,
    pub input: &'a serde_json::Value,
    /// 目标主机的 tag。本机执行时为空。
    pub host_tags: &'a [String],
    pub task_id: TaskId,
}

/// 判决结果。
#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
    pub effect: PolicyEffect,
    /// 命中的规则。走兜底策略时为 `None`。
    pub rule_id: Option<RuleId>,
    pub reason: String,
}

impl Decision {
    #[must_use]
    pub fn is_allowed(&self) -> bool {
        self.effect == PolicyEffect::Allow
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PolicyError {
    #[error("规则 `{rule}` 的模式 `{pattern}` 不是合法的正则：{detail}")]
    BadPattern {
        rule: String,
        pattern: String,
        detail: String,
    },
}

impl PolicySet {
    /// 编译一组规则。任何一条不合法都整体拒绝——半装的策略集比没有策略更危险。
    pub fn compile(
        mut rules: Vec<PolicyRule>,
        default_effect: DefaultPolicy,
    ) -> Result<Self, PolicyError> {
        // 数值大的先判。同优先级按名字排，保证判决是确定的——
        // 否则同一次调用在不同副本上可能命中不同的规则。
        rules.sort_by(|a, b| {
            b.priority
                .cmp(&a.priority)
                .then_with(|| a.name.cmp(&b.name))
        });

        let compiled = rules
            .into_iter()
            .map(|rule| {
                let patterns = rule
                    .matcher
                    .any_of
                    .iter()
                    .map(|p| compile_pattern(&rule.name, p))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(Compiled { rule, patterns })
            })
            .collect::<Result<Vec<_>, PolicyError>>()?;

        Ok(Self {
            rules: compiled,
            default_effect,
        })
    }

    /// 判一次工具调用。首个命中的规则生效。
    #[must_use]
    pub fn decide(&self, call: &ToolCall<'_>) -> Decision {
        for entry in &self.rules {
            if entry.matches(call) {
                return Decision {
                    effect: entry.rule.effect,
                    rule_id: Some(entry.rule.id),
                    reason: entry.rule.reason.clone(),
                };
            }
        }
        self.fallback(call)
    }

    fn fallback(&self, call: &ToolCall<'_>) -> Decision {
        let write = is_write_tool(call.tool);
        let on_prod = call.host_tags.iter().any(|t| t == "prod");

        let (effect, reason) = match self.default_effect {
            DefaultPolicy::AllowAll => (PolicyEffect::Allow, String::new()),
            DefaultPolicy::DenyWrites if write => (
                PolicyEffect::Deny,
                format!(
                    "默认策略：`{}` 是写类工具，当前档位禁止一切写操作",
                    call.tool
                ),
            ),
            DefaultPolicy::DenyWrites => (PolicyEffect::Allow, String::new()),
            DefaultPolicy::GuardProduction if write && on_prod => (
                PolicyEffect::Ask,
                format!(
                    "默认策略：`{}` 是写类工具，目标主机带 prod 标签，需要人工确认",
                    call.tool
                ),
            ),
            DefaultPolicy::GuardProduction => (PolicyEffect::Allow, String::new()),
        };

        Decision {
            effect,
            rule_id: None,
            reason,
        }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.rules.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }
}

impl Compiled {
    fn matches(&self, call: &ToolCall<'_>) -> bool {
        let scope = &self.rule.scope;
        if !scope.host_tags.is_empty()
            && !scope.host_tags.iter().any(|t| call.host_tags.contains(t))
        {
            return false;
        }
        if !scope.task_ids.is_empty() && !scope.task_ids.contains(&call.task_id) {
            return false;
        }
        if let Some(tool) = &self.rule.matcher.tool
            && tool != call.tool
        {
            return false;
        }
        if self.patterns.is_empty() {
            // 没给模式就是「工具名对上即命中」
            return true;
        }

        let haystack = match &self.rule.matcher.arg {
            Some(arg) => match call.input.get(arg) {
                // 字符串取原文，其它类型取 JSON 文本，好让规则也能匹配数字和数组
                Some(serde_json::Value::String(s)) => s.clone(),
                Some(other) => other.to_string(),
                // 参数不存在就不匹配。**不能**退化成匹配整个 input：
                // 那会让 `arg: "command"` 的规则意外命中别的工具的参数。
                None => return false,
            },
            None => call.input.to_string(),
        };

        self.patterns.iter().any(|re| re.is_match(&haystack))
    }
}

/// 工具是不是写类。**未知工具按写类处理**（fail closed）。
#[must_use]
pub fn is_write_tool(tool: &str) -> bool {
    !READ_ONLY_TOOLS.contains(&tool)
}

fn compile_pattern(rule: &str, pattern: &Pattern) -> Result<regex::Regex, PolicyError> {
    let source = match pattern {
        Pattern::Regex(re) => re.clone(),
        Pattern::Glob(glob) => glob_to_regex(glob),
        Pattern::Contains(text) => regex::escape(text),
        Pattern::Equals(text) => format!("^{}$", regex::escape(text)),
    };

    regex::RegexBuilder::new(&source)
        .size_limit(REGEX_SIZE_LIMIT)
        .build()
        .map_err(|err| PolicyError::BadPattern {
            rule: rule.to_string(),
            pattern: source,
            detail: err.to_string(),
        })
}

/// glob → 正则。除 `*` `?` 外全部转义，所以用户写的 `.` `(` 之类是字面量。
fn glob_to_regex(glob: &str) -> String {
    let mut out = String::with_capacity(glob.len() * 2 + 2);
    out.push('^');
    for ch in glob.chars() {
        match ch {
            '*' => out.push_str(".*"),
            '?' => out.push('.'),
            other => out.push_str(&regex::escape(&other.to_string())),
        }
    }
    out.push('$');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_shadow_run_can_still_return_its_result() {
        // StructuredOutput 是模型交还结果的通道，不碰外界。把它当写类工具的话，
        // dry_run 的 run 永远产不出结构化输出——而影子执行的意义就是拿输出比基线。
        assert!(!is_write_tool("StructuredOutput"));
    }

    #[test]
    fn an_unknown_tool_is_treated_as_a_write() {
        // 新增一个能改文件的工具却忘了更新这张表时，后果必须是"更严"而不是"放行"
        assert!(is_write_tool("SomeNewToolNobodyToldUsAbout"));
        assert!(is_write_tool("Bash"));
        assert!(is_write_tool("remote_write"));
    }

    use serde_json::json;

    fn rule(name: &str, effect: PolicyEffect, matcher: ToolMatcher) -> PolicyRule {
        PolicyRule {
            id: RuleId::new(),
            name: name.into(),
            priority: 0,
            effect,
            reason: format!("规则 {name}"),
            scope: RuleScope::default(),
            matcher,
        }
    }

    fn call<'a>(tool: &'a str, input: &'a serde_json::Value) -> ToolCall<'a> {
        ToolCall {
            tool,
            input,
            host_tags: &[],
            task_id: TaskId::new(),
        }
    }

    #[test]
    fn a_regex_rule_blocks_the_command_it_targets() {
        let set = PolicySet::compile(
            vec![rule(
                "禁止递归删除",
                PolicyEffect::Deny,
                ToolMatcher {
                    tool: Some("Bash".into()),
                    arg: Some("command".into()),
                    any_of: vec![Pattern::Regex(r"\brm\s+-[a-zA-Z]*[rR]".into())],
                },
            )],
            DefaultPolicy::AllowAll,
        )
        .expect("编译");

        let danger = json!({"command": "rm -rf /var/lib/data"});
        let decision = set.decide(&call("Bash", &danger));
        assert_eq!(decision.effect, PolicyEffect::Deny);
        assert!(decision.rule_id.is_some());

        let benign = json!({"command": "ls -la"});
        assert!(set.decide(&call("Bash", &benign)).is_allowed());
    }

    #[test]
    fn glob_metacharacters_from_the_user_are_literal_except_star_and_question() {
        let set = PolicySet::compile(
            vec![rule(
                "禁止格式化",
                PolicyEffect::Deny,
                ToolMatcher {
                    tool: Some("Bash".into()),
                    arg: Some("command".into()),
                    any_of: vec![Pattern::Glob("*mkfs.*".into())],
                },
            )],
            DefaultPolicy::AllowAll,
        )
        .expect("编译");

        assert_eq!(
            set.decide(&call(
                "Bash",
                &json!({"command": "sudo mkfs.ext4 /dev/sda"})
            ))
            .effect,
            PolicyEffect::Deny
        );
        // glob 里的 `.` 是字面量，不该匹配任意字符
        assert!(
            set.decide(&call("Bash", &json!({"command": "mkfsXext4"})))
                .is_allowed(),
            "glob 的 `.` 必须当字面量，否则规则会误伤"
        );
    }

    #[test]
    fn first_match_wins_and_priority_decides_the_order() {
        let mut allow = rule(
            "放行只读 git",
            PolicyEffect::Allow,
            ToolMatcher {
                tool: Some("Bash".into()),
                arg: Some("command".into()),
                any_of: vec![Pattern::Regex("^git (status|log|diff)".into())],
            },
        );
        allow.priority = 100;

        let mut deny = rule(
            "禁止一切 git",
            PolicyEffect::Deny,
            ToolMatcher {
                tool: Some("Bash".into()),
                arg: Some("command".into()),
                any_of: vec![Pattern::Regex("^git ".into())],
            },
        );
        deny.priority = 10;

        let set = PolicySet::compile(vec![deny, allow], DefaultPolicy::AllowAll).expect("编译");
        // 高优先级的放行规则先判到
        assert!(
            set.decide(&call("Bash", &json!({"command": "git status"})))
                .is_allowed()
        );
        assert_eq!(
            set.decide(&call("Bash", &json!({"command": "git push --force"})))
                .effect,
            PolicyEffect::Deny
        );
    }

    #[test]
    fn a_missing_argument_does_not_fall_back_to_matching_the_whole_input() {
        // arg 指定了却不存在时必须不匹配。退化成匹配整个 input 的话，
        // 一条针对 Bash.command 的规则会意外命中别的工具的参数。
        let set = PolicySet::compile(
            vec![rule(
                "禁止 rm",
                PolicyEffect::Deny,
                ToolMatcher {
                    tool: None,
                    arg: Some("command".into()),
                    any_of: vec![Pattern::Contains("rm".into())],
                },
            )],
            DefaultPolicy::AllowAll,
        )
        .expect("编译");

        // Write 工具没有 command 参数，但 file_path 里含 "rm"
        let write = json!({"file_path": "/tmp/form.txt", "content": "x"});
        assert!(set.decide(&call("Write", &write)).is_allowed());
    }

    /// 白名单模式的基石：一条只写工具名、不写模式的规则匹配该工具的**所有**调用。
    ///
    /// 实测教训：只用正则去封 `rm -r` 是挡不住的——模型会改用
    /// `find -delete` 之类的写法绕过去。真正管用的是「默认拒绝该工具，
    /// 再用更高优先级的规则放行明确列出的用法」。
    #[test]
    fn a_rule_without_patterns_matches_every_call_of_that_tool() {
        let set = PolicySet::compile(
            vec![rule(
                "默认拒绝所有 shell",
                PolicyEffect::Deny,
                ToolMatcher {
                    tool: Some("Bash".into()),
                    arg: None,
                    any_of: vec![],
                },
            )],
            DefaultPolicy::AllowAll,
        )
        .expect("编译");

        // 换任何写法都拦得住，这正是单条正则做不到的
        for command in [
            "rm -rf junk",
            "find junk -type f -delete",
            "python3 -c \"import shutil; shutil.rmtree('junk')\"",
            "ls",
        ] {
            let input = json!({ "command": command });
            assert_eq!(
                set.decide(&call("Bash", &input)).effect,
                PolicyEffect::Deny,
                "{command} 应当被拦下"
            );
        }
        // 只作用于指定的工具，不影响别的
        assert!(set.decide(&call("Read", &json!({}))).is_allowed());
    }

    #[test]
    fn an_allowlist_rule_at_higher_priority_reopens_specific_usages() {
        let mut deny_all = rule(
            "默认拒绝所有 shell",
            PolicyEffect::Deny,
            ToolMatcher {
                tool: Some("Bash".into()),
                arg: None,
                any_of: vec![],
            },
        );
        deny_all.priority = 10;

        let mut allow = rule(
            "放行只读命令",
            PolicyEffect::Allow,
            ToolMatcher {
                tool: Some("Bash".into()),
                arg: Some("command".into()),
                any_of: vec![Pattern::Regex(r"^\s*(ls|cat|wc)\b".into())],
            },
        );
        allow.priority = 200;

        let set = PolicySet::compile(vec![deny_all, allow], DefaultPolicy::AllowAll).expect("编译");
        assert!(
            set.decide(&call("Bash", &json!({"command": "ls -la"})))
                .is_allowed()
        );
        assert_eq!(
            set.decide(&call("Bash", &json!({"command": "find . -delete"})))
                .effect,
            PolicyEffect::Deny,
            "不在清单里的写法一律拦下——这才是拦得住的形状"
        );
    }

    #[test]
    fn look_around_regexes_are_rejected_at_save_time() {
        // Rust 的 regex 不支持 look-around。让它在保存规则时就报错，
        // 而不是等到凌晨两点工具调用时整个策略集装不起来。
        let err = PolicySet::compile(
            vec![rule(
                "带先行断言",
                PolicyEffect::Allow,
                ToolMatcher {
                    tool: None,
                    arg: None,
                    any_of: vec![Pattern::Regex(r"^ls\b(?!.*-delete)".into())],
                },
            )],
            DefaultPolicy::AllowAll,
        )
        .expect_err("必须被拒");
        let PolicyError::BadPattern { detail, .. } = err;
        assert!(detail.contains("look-around"), "报错要说清原因：{detail}");
    }

    #[test]
    fn scope_limits_a_rule_to_tagged_hosts() {
        let mut prod_only = rule(
            "生产机禁止重启",
            PolicyEffect::Deny,
            ToolMatcher {
                tool: Some("Bash".into()),
                arg: Some("command".into()),
                any_of: vec![Pattern::Contains("systemctl restart".into())],
            },
        );
        prod_only.scope.host_tags = vec!["prod".into()];

        let set = PolicySet::compile(vec![prod_only], DefaultPolicy::AllowAll).expect("编译");
        let input = json!({"command": "systemctl restart nginx"});

        let on_prod = ToolCall {
            host_tags: &["prod".to_string()],
            ..call("Bash", &input)
        };
        assert_eq!(set.decide(&on_prod).effect, PolicyEffect::Deny);

        let on_staging = ToolCall {
            host_tags: &["staging".to_string()],
            ..call("Bash", &input)
        };
        assert!(set.decide(&on_staging).is_allowed());
    }

    #[test]
    fn unknown_tools_are_treated_as_write_class() {
        // 新加了个能改东西的工具却忘了更新策略时，后果必须是"被拦下"
        // 而不是"默认放行"
        assert!(is_write_tool("SomeBrandNewTool"));
        assert!(is_write_tool("Bash"));
        assert!(is_write_tool("Write"));
        assert!(!is_write_tool("Read"));
        assert!(!is_write_tool("Grep"));
    }

    #[test]
    fn the_default_profile_guards_production_without_blocking_reads() {
        let set = PolicySet::compile(vec![], DefaultPolicy::GuardProduction).expect("编译");
        let input = json!({"command": "df -h"});
        let prod = ToolCall {
            host_tags: &["prod".to_string()],
            ..call("Bash", &input)
        };
        // 生产机上的写类工具转人工，不是直接拒——拒了 AI 就什么都干不了
        assert_eq!(set.decide(&prod).effect, PolicyEffect::Ask);

        let read_on_prod = ToolCall {
            host_tags: &["prod".to_string()],
            ..call("Read", &input)
        };
        assert!(set.decide(&read_on_prod).is_allowed(), "只读操作不该被挡");

        // 非生产机放行
        assert!(set.decide(&call("Bash", &input)).is_allowed());
    }

    #[test]
    fn an_explicit_rule_overrides_the_default_profile() {
        let set = PolicySet::compile(
            vec![rule(
                "生产机也允许查看磁盘",
                PolicyEffect::Allow,
                ToolMatcher {
                    tool: Some("Bash".into()),
                    arg: Some("command".into()),
                    any_of: vec![Pattern::Regex("^df ".into())],
                },
            )],
            DefaultPolicy::GuardProduction,
        )
        .expect("编译");

        let input = json!({"command": "df -h"});
        let prod = ToolCall {
            host_tags: &["prod".to_string()],
            ..call("Bash", &input)
        };
        assert!(set.decide(&prod).is_allowed());
    }

    #[test]
    fn a_bad_pattern_rejects_the_whole_set_rather_than_loading_half_of_it() {
        // 半装的策略集比没有策略更危险：以为有防护，实际漏了一半
        let err = PolicySet::compile(
            vec![rule(
                "坏正则",
                PolicyEffect::Deny,
                ToolMatcher {
                    tool: None,
                    arg: None,
                    any_of: vec![Pattern::Regex("(unclosed".into())],
                },
            )],
            DefaultPolicy::AllowAll,
        )
        .expect_err("必须整体拒绝");
        assert!(matches!(err, PolicyError::BadPattern { .. }));
    }

    #[test]
    fn evaluation_order_is_deterministic_for_equal_priorities() {
        // 同优先级按名字排：否则同一次调用在不同副本上可能命中不同规则，
        // 审计里就会出现"同样的命令有时拒有时放"
        let make = || {
            vec![
                rule(
                    "b-deny",
                    PolicyEffect::Deny,
                    ToolMatcher {
                        tool: Some("Bash".into()),
                        arg: None,
                        any_of: vec![],
                    },
                ),
                rule(
                    "a-allow",
                    PolicyEffect::Allow,
                    ToolMatcher {
                        tool: Some("Bash".into()),
                        arg: None,
                        any_of: vec![],
                    },
                ),
            ]
        };
        let input = json!({"command": "ls"});
        for _ in 0..20 {
            let set = PolicySet::compile(make(), DefaultPolicy::AllowAll).expect("编译");
            assert!(
                set.decide(&call("Bash", &input)).is_allowed(),
                "应当命中 a-allow"
            );
        }
    }

    #[test]
    fn rules_round_trip_through_json() {
        let r = rule(
            "禁止递归删除",
            PolicyEffect::Deny,
            ToolMatcher {
                tool: Some("Bash".into()),
                arg: Some("command".into()),
                any_of: vec![
                    Pattern::Regex(r"\brm\s+-rf\s+/".into()),
                    Pattern::Glob("*mkfs*".into()),
                ],
            },
        );
        let json = serde_json::to_value(&r).expect("序列化");
        assert_eq!(json["match"]["tool"], "Bash");
        assert_eq!(json["match"]["any_of"][0]["regex"], r"\brm\s+-rf\s+/");
        let back: PolicyRule = serde_json::from_value(json).expect("反序列化");
        assert_eq!(back, r);
    }

    #[test]
    fn unknown_fields_in_a_rule_are_rejected() {
        // 拼错的字段不能静默忽略——"配了但没生效"的策略是最危险的
        let json = json!({
            "id": RuleId::new(),
            "name": "x",
            "effect": "deny",
            "reason": "x",
            "match": { "tool": "Bash", "anyof": [] }
        });
        assert!(serde_json::from_value::<PolicyRule>(json).is_err());
    }
}
