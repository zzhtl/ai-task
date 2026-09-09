//! `ai-task policy-hook` 子进程的行为测试。
//!
//! 这组测试真的**执行二进制**。原因是踩过一次：hook 里用 `reqwest::blocking`，
//! 而 `main` 当时是 `#[tokio::main]`，于是每次调用都 panic
//! （"Cannot drop a runtime in a context where blocking is not allowed"）。
//! 单测覆盖不到这种「进程起来就崩」的问题——而 PreToolUse hook 崩掉会被
//! Claude Code 当成「无决策」放行，也就是 **fail-open**：策略层整个失效，
//! 而且从事件日志上完全看不出来。

use std::io::Write as _;
use std::process::{Command, Stdio};

const HOOK_INPUT: &str = r#"{"tool_name":"Bash","tool_input":{"command":"rm -rf /"},"tool_use_id":"toolu_probe","hook_event_name":"PreToolUse"}"#;

/// 跑一次 hook，返回 (退出码, stdout)。
fn run_hook(endpoint: &str, stdin: &str) -> (i32, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_ai-task"))
        .args([
            "policy-hook",
            "--endpoint",
            endpoint,
            "--run-id",
            "01a08098-8972-7bd0-a78c-059c8c8b0163",
            "--token",
            "irrelevant",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("拉起 hook 进程");

    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(stdin.as_bytes())
        .expect("写入");

    let output = child.wait_with_output().expect("等待 hook");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).to_string(),
    )
}

fn decision(stdout: &str) -> (String, String) {
    let json: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("输出不是 JSON（{e}）：{stdout}"));
    let out = &json["hookSpecificOutput"];
    assert_eq!(out["hookEventName"], "PreToolUse");
    (
        out["permissionDecision"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        out["permissionDecisionReason"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
    )
}

#[test]
fn an_unreachable_policy_layer_denies_instead_of_crashing() {
    // 连不上就放行，等于「把服务端打挂 = 关掉所有策略」
    let (code, stdout) = run_hook("http://127.0.0.1:1/nope", HOOK_INPUT);
    assert_eq!(code, 0, "hook 必须正常退出，非零退出码不等于拒绝");

    let (decision_kind, reason) = decision(&stdout);
    assert_eq!(decision_kind, "deny");
    assert!(
        reason.contains("拒绝"),
        "拒绝理由要说清发生了什么：{reason}"
    );
}

#[test]
fn a_malformed_payload_denies() {
    // 喂一段垃圾不能绕过策略
    let (code, stdout) = run_hook("http://127.0.0.1:1/nope", "not json at all");
    assert_eq!(code, 0);
    assert_eq!(decision(&stdout).0, "deny");
}

#[test]
fn empty_stdin_denies() {
    let (code, stdout) = run_hook("http://127.0.0.1:1/nope", "");
    assert_eq!(code, 0);
    assert_eq!(decision(&stdout).0, "deny");
}

#[test]
fn the_hook_never_writes_an_empty_decision() {
    // 空 stdout 会被 CLI 当成「无决策」，那就是 fail-open
    for input in [HOOK_INPUT, "not json", "", "{}", r#"{"tool_name":123}"#] {
        let (_, stdout) = run_hook("http://127.0.0.1:1/nope", input);
        assert!(
            !stdout.trim().is_empty(),
            "输入 {input:?} 时 hook 输出为空——CLI 会当成放行"
        );
        assert_eq!(decision(&stdout).0, "deny");
    }
}
