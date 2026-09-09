//! 组装 `claude` 的命令行。
//!
//! 核心是**密闭执行**：一次 run 的行为不能取决于操作机上恰好装了什么。
//! 不做隔离时 CLI 会把宿主的 MCP server、settings、CLAUDE.md 全部拉进上下文。
//! 实测（`tests/fixtures/` 里两份录制）同一个任务：
//!
//! | | 可用工具 | 成本 |
//! |---|---|---|
//! | 继承宿主配置 | 30 个（含 22 个 MCP 工具） | $0.0736 |
//! | 密闭执行 | 3 个 | $0.0336 |
//!
//! 便宜 54%，而且**可复现**——这才是重点。定时任务在不同机器上跑出不同结果，
//! 是最难查的一类问题。

use crate::ExecRequest;

/// 未指定 `tools` 时的默认白名单。
///
/// 刻意只给只读工具。写入类的（Edit/Write/Bash 的破坏性用法）要节点显式声明，
/// 免得「忘了配」变成「默认能改文件」。M2 起策略层会在这之上再做一道强制。
const DEFAULT_TOOLS: &[&str] = &["Read", "Glob", "Grep"];

/// 调用技能包要用的工具。
///
/// 节点勾了 skill 却没有这个工具，技能包会落地但模型调不动它——
/// 表现是「配了但没生效」，最难查的那类问题。
const SKILL_TOOL: &str = "Skill";

/// 拼好的命令行参数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    args: Vec<String>,
}

impl Invocation {
    #[must_use]
    pub fn build(request: &ExecRequest) -> Self {
        let mut args: Vec<String> = Vec::with_capacity(24);
        let mut push = |flag: &str, value: Option<String>| match value {
            Some(v) => {
                args.push(flag.into());
                args.push(v);
            }
            None => args.push(flag.into()),
        };

        push("-p", Some(request.prompt.clone()));

        // stream-json 必须配 --verbose，否则 CLI 直接报错拒绝启动
        push("--output-format", Some("stream-json".into()));
        push("--verbose", None);

        // ---- 密闭执行：三个隔离开关，任何时候都不能省 ----
        // 不读宿主的 MCP 配置
        push("--strict-mcp-config", None);
        // 只读 **project** 作用域，也就是工作目录自己的 `.claude/`——那是我们
        // 每个 run 新建的，内容完全由平台写入。
        //
        // 为什么不是空字符串：空值会连带屏蔽 `.claude/skills/` 的发现，
        // 勾选的技能包落了地也用不上（实测确认）。而 `user` 作用域会把操作机
        // 上的个人 skill、MCP server 和配置全拉进来，那正是要隔离的东西。
        //
        // 前提：`AI_TASK_WORKDIR_ROOT` 不能放在一个自带 `.claude/` 的仓库里，
        // 否则那个仓库的配置会被当成 project 作用域读进来。
        push("--setting-sources", Some("project".into()));
        // 会话不落宿主磁盘。事件日志才是真相来源，CLI 的 session 文件既是
        // 泄漏面也是「同一台机器跑两次结果不同」的来源
        push("--no-session-persistence", None);

        push("--session-id", Some(request.session_id.to_string()));

        if let Some(model) = &request.model {
            push("--model", Some(model.clone()));
        }
        if let Some(effort) = request.effort {
            push("--effort", Some(effort_flag(effort).into()));
        }

        let mut tools = request
            .tools
            .clone()
            .unwrap_or_else(|| DEFAULT_TOOLS.iter().map(|s| (*s).to_string()).collect());
        // 勾了技能包就必须带上 Skill 工具，否则包落了地也调不动
        if request.has_skills && !tools.iter().any(|t| t == SKILL_TOOL) {
            tools.push(SKILL_TOOL.to_string());
        }
        push("--tools", Some(tools.join(",")));

        // 远端工具代理。和 `--strict-mcp-config` 配套：宿主的 MCP 配置一概不读，
        // 只挂这一个我们自己的 server。
        if let Some(mcp) = &request.mcp_config {
            push("--mcp-config", Some(mcp.to_string_lossy().into_owned()));
        }

        // 策略 hook 的配置。放在隔离开关之后：`--setting-sources ""` 关掉的是
        // 宿主的配置，这里显式给的仍然生效（已实测确认）。
        if let Some(settings) = &request.settings_path {
            push("--settings", Some(settings.to_string_lossy().into_owned()));
        }

        if let Some(schema) = &request.output_schema {
            push("--json-schema", Some(schema.to_string()));
        }
        if let Some(budget) = request.budget_usd {
            push("--max-budget-usd", Some(budget.to_decimal_string()));
        }
        if let Some(rules) = &request.system_append
            && !rules.trim().is_empty()
        {
            push("--append-system-prompt", Some(rules.clone()));
        }

        Self { args }
    }

    /// 渲染成一条可以直接复制去复现的命令行。
    ///
    /// **参数按 shell 规则引好**：提示词里几乎必然有换行、引号、反引号，
    /// 不引的话复制出去的那条命令跑起来是另一回事——而这个功能的全部意义
    /// 就是「照着这条命令能复现刚才发生的事」。
    #[must_use]
    pub fn command_line(&self, program: &str) -> String {
        std::iter::once(program)
            .chain(self.args.iter().map(String::as_str))
            .map(shell_quote)
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[must_use]
    pub fn args(&self) -> &[String] {
        &self.args
    }

    /// 有没有带某个 flag。给测试和诊断用。
    #[must_use]
    pub fn has_flag(&self, flag: &str) -> bool {
        self.args.iter().any(|a| a == flag)
    }

    /// 取某个 flag 的值。
    #[must_use]
    pub fn value_of(&self, flag: &str) -> Option<&str> {
        let index = self.args.iter().position(|a| a == flag)?;
        self.args.get(index + 1).map(String::as_str)
    }
}

fn effort_flag(effort: ai_task_proto::Effort) -> &'static str {
    use ai_task_proto::Effort;
    match effort {
        Effort::Low => "low",
        Effort::Medium => "medium",
        Effort::High => "high",
        Effort::Xhigh => "xhigh",
        Effort::Max => "max",
    }
}

/// 单引号包裹。POSIX shell 里单引号内没有任何转义，唯一要处理的是单引号本身。
///
/// 不带特殊字符的短参数不加引号——一整行全是引号的命令没人愿意读。
fn shell_quote(value: &str) -> String {
    let plain = !value.is_empty()
        && value.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | ',' | '=' | ':')
        });
    if plain {
        value.to_owned()
    } else {
        format!("'{}'", value.replace('\'', r"'\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ai_task_proto::{Effort, NodeKey, UsdMicros};

    fn request() -> ExecRequest {
        ExecRequest {
            node_key: NodeKey("probe".into()),
            prompt: "统计行数".into(),
            workdir: std::path::PathBuf::from("/tmp"),
            model: None,
            effort: None,
            tools: None,
            output_schema: None,
            budget_usd: None,
            system_append: None,
            session_id: uuid::Uuid::now_v7(),
            settings_path: None,
            mcp_config: None,
            has_skills: false,
        }
    }

    /// 这三个开关是密闭执行的全部，少一个 run 的行为就开始取决于操作机。
    #[test]
    fn isolation_flags_are_always_present() {
        let inv = Invocation::build(&request());
        for flag in ["--strict-mcp-config", "--no-session-persistence"] {
            assert!(inv.has_flag(flag), "缺少隔离开关 {flag}：{:?}", inv.args());
        }
        assert_eq!(
            inv.value_of("--setting-sources"),
            Some("project"),
            "只读工作目录自己的 .claude/；给空会连带屏蔽技能包发现，给 user 会把操作机上的个人配置拉进来"
        );
    }

    #[test]
    fn a_remote_node_gets_its_proxy_config_alongside_strict_mcp() {
        // --mcp-config 单独给是不够的：没有 --strict-mcp-config 的话，
        // 操作机上 ~/.claude 里的 MCP server 也会被一起挂上。
        let mut req = request();
        req.mcp_config = Some("/tmp/run/.claude/mcp-probe.json".into());
        let inv = Invocation::build(&req);
        assert_eq!(
            inv.value_of("--mcp-config"),
            Some("/tmp/run/.claude/mcp-probe.json")
        );
        assert!(inv.has_flag("--strict-mcp-config"));
    }

    #[test]
    fn quoting_survives_a_real_shell() {
        // 断言语义而不是字面量：换个等价引法时测试不该红
        for value in ["a b", "'; id #", "$(id)", "换行\n还有", "反引号 `x`"] {
            let out = std::process::Command::new("sh")
                .arg("-c")
                .arg(format!("printf %s {}", shell_quote(value)))
                .output()
                .expect("跑 sh");
            assert_eq!(
                String::from_utf8_lossy(&out.stdout),
                value,
                "{value} 经过 shell 变了样"
            );
        }
    }

    #[test]
    fn the_rendered_command_line_can_be_pasted_into_a_shell() {
        // 这个功能的全部意义是「照着这条命令能复现刚才发生的事」。
        // 引错了的话复制出去跑的是另一回事，比不给还糟。
        let mut req = request();
        req.prompt = "看看这个'; rm -rf /tmp/x; echo '\n还有 `id` 和 $(whoami)".into();
        let line = Invocation::build(&req).command_line("claude");

        let script =
            format!("claude() {{ for a in \"$@\"; do printf '%s\\036' \"$a\"; done; }}\n{line}");
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(&script)
            .output()
            .expect("跑 sh");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );

        let stdout = String::from_utf8_lossy(&out.stdout);
        let args: Vec<&str> = stdout.trim_end_matches('\u{1e}').split('\u{1e}').collect();
        let at = args.iter().position(|a| *a == "-p").expect("有 -p");
        assert_eq!(args[at + 1], req.prompt, "提示词没有原样到达：{args:?}");
    }

    #[test]
    fn a_readable_command_does_not_quote_every_single_token() {
        // 一整行全是引号的命令没人愿意读，也就失去了"给人看"的意义
        let line = Invocation::build(&request()).command_line("claude");
        assert!(line.starts_with("claude -p "), "{line}");
        assert!(line.contains(" --output-format stream-json "), "{line}");
        assert!(
            !line.contains("'--verbose'"),
            "简单参数不该被引起来：{line}"
        );
    }

    #[test]
    fn stream_json_is_paired_with_verbose() {
        // 少了 --verbose，CLI 会直接拒绝启动
        let inv = Invocation::build(&request());
        assert_eq!(inv.value_of("--output-format"), Some("stream-json"));
        assert!(inv.has_flag("--verbose"));
    }

    #[test]
    fn default_tool_whitelist_is_read_only() {
        let inv = Invocation::build(&request());
        let tools = inv.value_of("--tools").expect("应当有 --tools");
        assert_eq!(tools, "Read,Glob,Grep");
        for writable in ["Bash", "Edit", "Write"] {
            assert!(
                !tools.split(',').any(|t| t == writable),
                "默认白名单不能包含 {writable}：忘了配不该等于默认能改文件"
            );
        }
    }

    #[test]
    fn declaring_skills_pulls_in_the_skill_tool() {
        // 落了地却调不动的技能包，比没有技能包更难查
        let mut req = request();
        req.tools = Some(vec!["Bash".into()]);
        req.has_skills = true;
        let inv = Invocation::build(&req);
        let tools = inv.value_of("--tools").expect("有 --tools");
        assert!(tools.split(',').any(|t| t == "Skill"), "{tools}");
        assert!(tools.split(',').any(|t| t == "Bash"), "{tools}");
    }

    #[test]
    fn no_skills_means_no_skill_tool() {
        let inv = Invocation::build(&request());
        assert!(
            !inv.value_of("--tools")
                .unwrap_or_default()
                .split(',')
                .any(|t| t == "Skill"),
            "没勾技能包时不该多这个工具"
        );
    }

    #[test]
    fn the_skill_tool_is_not_added_twice() {
        let mut req = request();
        req.tools = Some(vec!["Skill".into(), "Read".into()]);
        req.has_skills = true;
        assert_eq!(
            Invocation::build(&req).value_of("--tools"),
            Some("Skill,Read")
        );
    }

    #[test]
    fn explicit_tools_override_the_default() {
        let mut req = request();
        req.tools = Some(vec!["Bash".into(), "Read".into()]);
        assert_eq!(
            Invocation::build(&req).value_of("--tools"),
            Some("Bash,Read")
        );
    }

    #[test]
    fn budget_is_passed_as_a_decimal_string_not_a_float() {
        let mut req = request();
        req.budget_usd = Some(UsdMicros(50_000));
        assert_eq!(
            Invocation::build(&req).value_of("--max-budget-usd"),
            Some("0.050000")
        );
    }

    #[test]
    fn the_policy_hook_settings_file_is_forwarded() {
        let mut req = request();
        req.settings_path = Some(std::path::PathBuf::from(
            "/runs/x/.claude/hook-settings.json",
        ));
        assert_eq!(
            Invocation::build(&req).value_of("--settings"),
            Some("/runs/x/.claude/hook-settings.json")
        );
    }

    #[test]
    fn optional_knobs_are_absent_when_unset() {
        let inv = Invocation::build(&request());
        for flag in [
            "--model",
            "--effort",
            "--json-schema",
            "--max-budget-usd",
            "--append-system-prompt",
        ] {
            assert!(!inv.has_flag(flag), "未设置时不该出现 {flag}");
        }
    }

    #[test]
    fn blank_rules_do_not_produce_an_empty_system_prompt_flag() {
        let mut req = request();
        req.system_append = Some("   \n  ".into());
        assert!(
            !Invocation::build(&req).has_flag("--append-system-prompt"),
            "空白规则不该占一个 flag"
        );
    }

    #[test]
    fn schema_and_effort_are_forwarded() {
        let mut req = request();
        req.effort = Some(Effort::Xhigh);
        req.output_schema = Some(serde_json::json!({"type": "object"}));
        let inv = Invocation::build(&req);
        assert_eq!(inv.value_of("--effort"), Some("xhigh"));
        assert_eq!(inv.value_of("--json-schema"), Some(r#"{"type":"object"}"#));
    }

    #[test]
    fn prompt_is_passed_as_an_argument_not_interpolated_into_a_shell() {
        // Command 不经过 shell，所以带引号和 $ 的提示词原样传进去就是安全的
        let mut req = request();
        req.prompt = r#"rm -rf / ; echo "$(whoami)" `id`"#.into();
        let inv = Invocation::build(&req);
        assert_eq!(inv.value_of("-p"), Some(req.prompt.as_str()));
    }
}
