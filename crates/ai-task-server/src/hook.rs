//! `ai-task policy-hook` —— Claude Code 的 `PreToolUse` hook。
//!
//! 从 stdin 读一条工具调用，问策略层，把判决写回 stdout。
//!
//! **失败一律拒绝。** 连不上服务端、超时、响应看不懂——统统当拒绝。
//! 反过来（连不上就放行）意味着「把服务端打挂 = 关掉所有策略」，
//! 那这层防护就是纸糊的。

use std::io::Read as _;
use std::time::Duration;

use clap::Args;
use serde::{Deserialize, Serialize};

/// hook 自己的超时。比策略接口的处理时间宽裕，但不能久到把工具调用卡死。
const TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Args)]
pub struct HookArgs {
    /// 判决接口地址。
    #[arg(long)]
    pub endpoint: String,
    /// 当前 run。
    #[arg(long)]
    pub run_id: String,
    /// 调用方令牌。
    #[arg(long)]
    pub token: String,
    /// 事件归属的节点。
    #[arg(long, default_value = "")]
    pub node_key: String,
}

/// Claude Code 发给 `PreToolUse` hook 的负载（只取用得上的字段）。
#[derive(Debug, Deserialize)]
struct HookInput {
    tool_name: String,
    #[serde(default)]
    tool_input: serde_json::Value,
    #[serde(default)]
    tool_use_id: String,
}

/// hook 的返回格式。
#[derive(Debug, Serialize)]
struct HookOutput {
    #[serde(rename = "hookSpecificOutput")]
    hook_specific_output: PreToolUseOutput,
}

#[derive(Debug, Serialize)]
struct PreToolUseOutput {
    #[serde(rename = "hookEventName")]
    hook_event_name: &'static str,
    #[serde(rename = "permissionDecision")]
    permission_decision: &'static str,
    #[serde(rename = "permissionDecisionReason")]
    permission_decision_reason: String,
}

impl HookOutput {
    fn allow(reason: String) -> Self {
        Self::new("allow", reason)
    }

    fn deny(reason: String) -> Self {
        Self::new("deny", reason)
    }

    fn new(decision: &'static str, reason: String) -> Self {
        Self {
            hook_specific_output: PreToolUseOutput {
                hook_event_name: "PreToolUse",
                permission_decision: decision,
                permission_decision_reason: reason,
            },
        }
    }
}

/// 跑一次 hook。永远返回 0：非零退出码在 Claude Code 里的语义和
/// 「拒绝这次调用」不是一回事，我们要的是后者。
pub fn run(args: &HookArgs) -> ! {
    let output = decide(args).unwrap_or_else(HookOutput::deny);
    // 序列化失败也要吐一个合法的拒绝出去，不能让 hook 输出空字符串
    let json = serde_json::to_string(&output).unwrap_or_else(|_| {
        r#"{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"策略层响应无法序列化"}}"#.into()
    });
    println!("{json}");
    std::process::exit(0)
}

/// 返回 `Err(reason)` 表示「拒绝，理由是 reason」。
fn decide(args: &HookArgs) -> Result<HookOutput, String> {
    let mut raw = String::new();
    std::io::stdin()
        .read_to_string(&mut raw)
        .map_err(|err| format!("读取 hook 输入失败：{err}"))?;

    let input: HookInput =
        serde_json::from_str(&raw).map_err(|err| format!("hook 输入不是预期的 JSON：{err}"))?;

    let body = serde_json::json!({
        "run_id": args.run_id,
        "node_key": args.node_key,
        "tool_use_id": input.tool_use_id,
        "tool": input.tool_name,
        "input": input.tool_input,
    });

    let client = build_client()?;
    let response = post(&client, &args.endpoint, &args.token, &body)?;
    if response.allowed {
        return Ok(HookOutput::allow(response.reason));
    }
    // 命中 `ask`：审批已经建好，这里等人点头。
    //
    // 分成多次短请求，而不是挂在一个长请求上。后者会逼着把 TIMEOUT 放大到
    // 审批窗口那么长——于是服务端一旦卡住，每次工具调用都要挂十分钟才失败，
    // 而不是二十秒内快速关门。失败模式比等待时长重要。
    if let Some(approval_id) = response.pending_approval_id {
        return Ok(await_decision(&client, args, &approval_id));
    }
    Ok(HookOutput::deny(response.reason))
}

/// 轮询到有结论，或者到自己的截止时间。
///
/// 任何一次请求出错都**当场关门**，不重试到天荒地老：连不上策略层时
/// 继续等下去等于把"判不出来"变成"无限期挂起"。
fn await_decision(
    client: &reqwest::blocking::Client,
    args: &HookArgs,
    approval_id: &str,
) -> HookOutput {
    let endpoint = args.endpoint.replace("/policy/decide", "/policy/await");
    let deadline = std::time::Instant::now() + APPROVAL_DEADLINE;
    let body = serde_json::json!({ "run_id": args.run_id, "approval_id": approval_id });

    while std::time::Instant::now() < deadline {
        match post_await(client, &endpoint, &args.token, &body) {
            Ok(response) if !response.pending => {
                return if response.approved {
                    HookOutput::allow(response.reason)
                } else {
                    HookOutput::deny(response.reason)
                };
            }
            // 还没人决策，接着问
            Ok(_) => {}
            Err(err) => return HookOutput::deny(format!("等待审批时失败（{err}），按拒绝处理")),
        }
    }
    HookOutput::deny("等待人工确认超时，按拒绝处理".into())
}

/// hook 侧等待审批的总上限。要略大于服务端的 ASK_TIMEOUT_S，
/// 这样"超时"的结论由服务端给出并落库，而不是两边各自认定。
const APPROVAL_DEADLINE: std::time::Duration = std::time::Duration::from_secs(660);

#[derive(Debug, Deserialize)]
struct DecideResponse {
    allowed: bool,
    #[serde(default)]
    reason: String,
    /// 命中 `ask` 时给出，需要轮询等结论。
    #[serde(default)]
    pending_approval_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AwaitResponse {
    pending: bool,
    #[serde(default)]
    approved: bool,
    #[serde(default)]
    reason: String,
}

/// hook 是个短命进程，起一个 tokio 运行时不划算，直接用阻塞客户端。
fn build_client() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .timeout(TIMEOUT)
        // 判决接口只在回环上，不该走代理——环境里配了 HTTP_PROXY 时
        // 请求会被送到外面去，那既连不上也泄漏了工具参数
        .no_proxy()
        .build()
        .map_err(|err| format!("构造 HTTP 客户端失败：{err}"))
}

/// 打一次判决接口。
fn post(
    client: &reqwest::blocking::Client,
    endpoint: &str,
    token: &str,
    body: &serde_json::Value,
) -> Result<DecideResponse, String> {
    let response = client
        .post(endpoint)
        .header(crate::routes::policy::TOKEN_HEADER, token)
        .json(body)
        .send()
        .map_err(|err| format!("连不上策略层（{err}），按拒绝处理"))?;

    let status = response.status();
    if !status.is_success() {
        return Err(format!("策略层返回 {status}，按拒绝处理"));
    }
    response
        .json::<DecideResponse>()
        .map_err(|err| format!("策略层响应无法解析（{err}），按拒绝处理"))
}

fn post_await(
    client: &reqwest::blocking::Client,
    endpoint: &str,
    token: &str,
    body: &serde_json::Value,
) -> Result<AwaitResponse, String> {
    let response = client
        .post(endpoint)
        .header(crate::routes::policy::TOKEN_HEADER, token)
        .json(body)
        .send()
        .map_err(|err| format!("连不上策略层（{err}）"))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("策略层返回 {status}"));
    }
    response
        .json::<AwaitResponse>()
        .map_err(|err| format!("响应无法解析（{err}）"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hook_output_shape_matches_what_claude_code_expects() {
        // 这个形状是实测出来的：deny 会让命令不执行，
        // 并把 reason 作为 tool_result 回给模型
        let json = serde_json::to_value(HookOutput::deny("禁止递归删除".into())).expect("序列化");
        assert_eq!(json["hookSpecificOutput"]["hookEventName"], "PreToolUse");
        assert_eq!(json["hookSpecificOutput"]["permissionDecision"], "deny");
        assert_eq!(
            json["hookSpecificOutput"]["permissionDecisionReason"],
            "禁止递归删除"
        );
    }

    #[test]
    fn allow_and_deny_differ_only_in_the_decision_field() {
        let allow = serde_json::to_value(HookOutput::allow("放行".into())).expect("序列化");
        assert_eq!(allow["hookSpecificOutput"]["permissionDecision"], "allow");
    }

    #[test]
    fn a_malformed_hook_payload_results_in_denial() {
        // 解析不了就拒绝。放行意味着"喂一段垃圾就能绕过策略"。
        let err = serde_json::from_str::<HookInput>("not json").expect_err("垃圾输入必须解析失败");
        assert!(!err.to_string().is_empty());
    }

    #[test]
    fn hook_input_tolerates_missing_optional_fields() {
        // CLI 加字段是常事；少了可选字段不该让 hook 崩掉变成"放行"
        let input: HookInput =
            serde_json::from_str(r#"{"tool_name":"Bash"}"#).expect("最小负载也要能解析");
        assert_eq!(input.tool_name, "Bash");
        assert!(input.tool_use_id.is_empty());
    }
}
