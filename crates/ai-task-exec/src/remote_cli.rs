//! 在**目标机上**跑 AI CLI。
//!
//! 和 [`crate::claude_code::ClaudeCodeExecutor`] 是两种模式，各有代价：
//!
//! | | 中心执行（默认） | 目标机执行（这里） |
//! |---|---|---|
//! | AI 进程在哪 | 中心 | 目标机 |
//! | 目标机要装什么 | **什么都不用** | CLI + 它的凭据 |
//! | 工具调用怎么走 | MCP 代理回中心过策略 | **CLI 自己在目标机上执行** |
//! | 爆炸半径 | 一次执行 | 那台机器的整个生命周期 |
//!
//! 目标机执行的诱惑是"简单直接"：不用配 MCP、不用过网络、文件就在本地。
//! 代价是**策略层管不到 CLI 的内置工具**——它的 Bash/Write 在目标机上直接落地，
//! 中心只能看见 stdout。所以这条路径必须由人显式选择，而且界面上要说清楚。
//!
//! 好消息是显示是免费的：CLI 的 `stream-json` 从 agent 的 stdout 逐行回来，
//! 喂给和本机执行**同一个** [`Decoder`]，产出同一套 `ExecEvent`，
//! 于是事件流、时间轴、成本统计全都不用改。

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::claude_code::decode::Decoder;
use crate::remote::{AgentEvent, RemoteAgent};
use crate::{ExecError, ExecEvent, ExecOutcome, ExecRequest};

/// 一次远端 CLI 执行需要的东西。
pub struct RemoteCliConfig {
    /// CLI 的名字，如 `claude`。必须是目标机 hello 时报上来的那几个之一。
    pub cli: String,
    /// 在目标机上的工作目录。
    pub workdir: String,
    /// 墙钟上限（秒）。
    pub timeout_s: u32,
}

/// 把一次 `ExecRequest` 翻译成目标机上的命令行。
///
/// 只支持 `stream-json` 输出的 CLI。别的形状的 CLI 需要各自的防腐层，
/// 那是新增一个 executor 的工作，不是在这里加分支。
#[must_use]
pub fn build_command(cli: &str, request: &ExecRequest) -> String {
    // 引用规则只有一份（见 `Invocation::command_line`）。两份的话，
    // 界面上展示的命令和真正下发的命令会在某个边界上悄悄分叉。
    crate::claude_code::invocation::Invocation::build(request).command_line(cli)
}

/// 在已经连上的 agent 上跑一次 CLI，把它的 stream-json 变成 `ExecEvent`。
///
/// 不实现 [`Executor`] trait：那个 trait 的 `spawn` 不带 agent，而 agent 的
/// 生命周期由调用方（引擎）管理。硬套 trait 只会让所有权变得别扭。
pub async fn run(
    agent: &mut RemoteAgent,
    config: &RemoteCliConfig,
    request: &ExecRequest,
    events: &mpsc::Sender<ExecEvent>,
    cancel: &CancellationToken,
) -> Result<(), ExecError> {
    let command = build_command(&config.cli, request);
    let mut decoder = Decoder::new();
    // stdout 是按块回来的，不保证按行切分：一行 JSON 可能横跨两个块，
    // 也可能一个块里挤了好几行。攒着按 \n 切才对得上。
    let mut pending = String::new();
    let mut stderr_tail = String::new();

    let exec = agent
        .exec(
            ai_task_agent::protocol::ExecRequest {
                command,
                cwd: config.workdir.clone(),
                timeout_ms: u64::from(config.timeout_s).saturating_mul(1000),
                limits: None,
                // stream-json 一次执行可能几百 KB，按默认的 256 KiB 会被截断，
                // 而截断发生在一行 JSON 中间就等于整条事件流从那里开始全废
                max_output_bytes: 16 * 1024 * 1024,
            },
            |event| match event {
                AgentEvent::Stdout(chunk) => {
                    pending.push_str(&chunk);
                    while let Some(at) = pending.find('\n') {
                        let line: String = pending.drain(..=at).collect();
                        for produced in decoder.line(line.trim_end()) {
                            // 发送失败只意味着接收端没了（run 已取消），
                            // 不该让执行本身失败
                            let _ = events.try_send(produced);
                        }
                    }
                }
                // CLI 的 stderr 是诊断信息，不是事件。留最后一段，
                // 失败时它通常就是原因。
                AgentEvent::Stderr(chunk) => {
                    stderr_tail.push_str(&chunk);
                    if stderr_tail.len() > 4096 {
                        let cut = stderr_tail.len() - 4096;
                        stderr_tail = stderr_tail.split_off(cut);
                    }
                }
                AgentEvent::Metrics(_) => {}
            },
        )
        .await
        .map_err(|err| ExecError::Remote(err.to_string()))?;

    // 收尾：最后一行可能没有换行符
    if !pending.trim().is_empty() {
        for produced in decoder.line(pending.trim_end()) {
            let _ = events.try_send(produced);
        }
    }

    if cancel.is_cancelled() {
        let _ = events
            .send(ExecEvent::Finished(ExecOutcome::Cancelled))
            .await;
        return Ok(());
    }

    // CLI 自己报了终态就用它的。没报（崩了、被杀了、输出被截断了）时
    // **必须自己造一个终态**，否则这个节点会永远停在 running。
    if !decoder.is_finished() {
        let reason = if exec.timed_out {
            format!("{} 在目标机上超时", config.cli)
        } else {
            format!(
                "{} 在目标机上以退出码 {} 结束，但没有产出终态事件{}",
                config.cli,
                exec.exit_code.unwrap_or(-1),
                first_line(&stderr_tail).map_or(String::new(), |l| format!("：{l}"))
            )
        };
        let _ = events
            .send(ExecEvent::Finished(ExecOutcome::Failed { reason }))
            .await;
    }
    Ok(())
}

fn first_line(text: &str) -> Option<String> {
    text.lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .map(|l| l.trim().chars().take(240).collect())
}

/// 目标机执行是不是可用。给界面用，也给保存任务时的校验用。
#[must_use]
pub fn cli_available(available: &[ai_task_agent::protocol::AiCli], wanted: &str) -> bool {
    available.iter().any(|c| c.name == wanted)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(prompt: &str) -> ExecRequest {
        ExecRequest {
            node_key: ai_task_proto::NodeKey::parse("n").expect("key"),
            prompt: prompt.to_string(),
            workdir: "/tmp".into(),
            model: Some("claude-sonnet-5".into()),
            effort: None,
            tools: None,
            output_schema: None,
            budget_usd: None,
            system_append: None,
            session_id: uuid::Uuid::nil(),
            has_skills: false,
            settings_path: None,
            mcp_config: None,
        }
    }

    #[test]
    fn a_hostile_prompt_arrives_as_exactly_one_argument() {
        // 提示词是人写的自由文本，里面几乎必然有引号、换行、反引号。
        // 引错的话，一句提示词就能变成目标机上的一条新命令。
        //
        // 断言的是**语义**：拿一个会把每个参数各打一行的假 CLI 跑一遍，
        // 看提示词是不是原样作为一个参数到达。查子串是查不出来的——
        // `; rm -rf` 出现在命令里完全正常，关键是它在不在引号内。
        let hostile = "看看这个'; rm -rf /tmp/x; echo '\n还有 `id` 和 $(whoami)";
        let command = build_command("fake_cli", &request(hostile));

        let script = format!(
            // 假 CLI：每个参数打一行，用 \x1e 分隔，好把换行和分隔符区分开
            "fake_cli() {{ for a in \"$@\"; do printf '%s\\036' \"$a\"; done; }}\n{command}"
        );
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

        // -p 后面紧跟的就是整段提示词，不是被切碎的一段
        let at = args.iter().position(|a| *a == "-p").expect("有 -p");
        assert_eq!(args[at + 1], hostile, "提示词没有原样到达：{args:?}");
    }

    #[test]
    fn the_command_carries_the_same_isolation_flags_as_local_execution() {
        // 目标机执行不该比本机执行更松：隔离开关一个都不能少
        let command = build_command("claude", &request("巡检"));
        for flag in [
            "--strict-mcp-config",
            "--no-session-persistence",
            "--output-format",
            "stream-json",
        ] {
            assert!(command.contains(flag), "少了 {flag}：{command}");
        }
    }

    #[test]
    fn availability_is_checked_against_what_the_host_reported() {
        use ai_task_agent::protocol::AiCli;
        let found = vec![AiCli {
            name: "claude".into(),
            path: "/usr/bin/claude".into(),
            version: None,
        }];
        assert!(cli_available(&found, "claude"));
        // 让人选一个装都没装的 CLI，失败会发生在凌晨两点而不是配置的时候
        assert!(!cli_available(&found, "codex"));
        assert!(!cli_available(&[], "claude"));
    }
}
