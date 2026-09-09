//! 驱动 Claude Code CLI 的执行器。

pub mod decode;
pub mod invocation;

use std::process::Stdio;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::event::{ExecEvent, ExecOutcome};
use crate::{ExecError, ExecHandle, ExecRequest, Executor};
use decode::Decoder;
use invocation::Invocation;

/// 事件通道容量。
///
/// 事件是流式产出的，消费端（事件写入器）落库有 IO 延迟。给一点缓冲，
/// 但**不能无界**——一个刷屏的工具输出会把内存吃光。满了就背压到读取循环，
/// 也就是背压到 CLI 的 stdout，这正是我们想要的。
const EVENT_CHANNEL_CAPACITY: usize = 256;

/// 子进程退出后仍未见到 `result` 事件时的兜底原因。
const NO_TERMINAL_EVENT: &str = "CLI 退出但没有产出 result 事件";

pub struct ClaudeCodeExecutor {
    program: String,
}

impl ClaudeCodeExecutor {
    /// `program` 是 claude 可执行文件的路径或名字。
    #[must_use]
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
        }
    }
}

impl Default for ClaudeCodeExecutor {
    fn default() -> Self {
        Self::new("claude")
    }
}

#[async_trait::async_trait]
impl Executor for ClaudeCodeExecutor {
    fn name(&self) -> &'static str {
        "claude_code"
    }

    fn command_line(&self, request: &ExecRequest) -> String {
        Invocation::build(request).command_line(&self.program)
    }

    async fn spawn(&self, request: ExecRequest) -> Result<ExecHandle, ExecError> {
        if !request.workdir.is_dir() {
            return Err(ExecError::MissingWorkdir(request.workdir.clone()));
        }

        let invocation = Invocation::build(&request);
        // 完整参数进 debug 日志。CLI 的行为对参数很敏感，出问题时第一件事
        // 就是确认「我们到底传了什么」——靠猜会浪费很多时间。
        tracing::debug!(
            target: "claude_code::invocation",
            node = %request.node_key,
            command = %invocation.command_line(&self.program),
            "启动 claude"
        );
        let mut command = Command::new(&self.program);
        command
            .args(invocation.args())
            .current_dir(&request.workdir)
            // stdin 必须给个空管道并立刻关掉：不接 stdin 时 CLI 会先等 3 秒，
            // 每个节点白白多花 3 秒
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let mut child = command.spawn().map_err(|source| {
            if source.kind() == std::io::ErrorKind::NotFound {
                ExecError::NotFound {
                    program: self.program.clone(),
                    source,
                }
            } else {
                ExecError::Spawn {
                    program: self.program.clone(),
                    source,
                }
            }
        })?;

        let stdout = child.stdout.take().expect("已配置 piped stdout");
        let stderr = child.stderr.take().expect("已配置 piped stderr");
        let (tx, rx) = mpsc::channel(EVENT_CHANNEL_CAPACITY);
        let cancel = CancellationToken::new();
        let child_cancel = cancel.clone();
        let node_key = request.node_key.clone();

        tokio::spawn(async move {
            // stderr 只进日志，不进事件流：CLI 往那儿写的是告警和进度，
            // 掺进审计链路只会稀释信噪比
            let stderr_task = tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                let mut tail = Vec::new();
                while let Ok(Some(line)) = lines.next_line().await {
                    tracing::debug!(target: "claude_code::stderr", "{line}");
                    tail.push(line);
                    if tail.len() > 20 {
                        tail.remove(0);
                    }
                }
                tail
            });

            let mut decoder = Decoder::new();
            let mut lines = BufReader::new(stdout).lines();
            let mut cancelled = false;

            loop {
                tokio::select! {
                    biased;
                    () = child_cancel.cancelled() => {
                        cancelled = true;
                        // kill_on_drop 只在 drop 时生效，这里要主动结束，
                        // 否则取消后 CLI 还会继续烧钱
                        let _ = child.start_kill();
                        break;
                    }
                    line = lines.next_line() => {
                        match line {
                            Ok(Some(line)) => {
                                for event in decoder.line(&line) {
                                    if tx.send(event).await.is_err() {
                                        // 接收端没了（run 被丢弃），没必要继续跑
                                        let _ = child.start_kill();
                                        return;
                                    }
                                }
                            }
                            Ok(None) => break,
                            Err(err) => {
                                let _ = tx.send(ExecEvent::Warning {
                                    message: format!("读取 CLI 输出失败：{err}"),
                                }).await;
                                break;
                            }
                        }
                    }
                }
            }

            let status = child.wait().await;
            let stderr_tail = stderr_task.await.unwrap_or_default();

            // 已经解出终态就不要再补一条，否则 run 会看到两个结局
            if !decoder.is_finished() {
                let outcome = if cancelled {
                    ExecOutcome::Cancelled
                } else {
                    ExecOutcome::Failed {
                        reason: terminal_reason(status, &stderr_tail),
                    }
                };
                let _ = tx.send(ExecEvent::Finished(outcome)).await;
            }
            tracing::debug!(node = %node_key, "claude code 执行结束");
        });

        Ok(ExecHandle { events: rx, cancel })
    }
}

/// 子进程异常结束时，拼一条能直接看懂的原因。
fn terminal_reason(
    status: std::io::Result<std::process::ExitStatus>,
    stderr_tail: &[String],
) -> String {
    let head = match status {
        Ok(status) => match status.code() {
            Some(code) => format!("{NO_TERMINAL_EVENT}（退出码 {code}）"),
            None => format!("{NO_TERMINAL_EVENT}（被信号终止）"),
        },
        Err(err) => format!("等待 CLI 进程失败：{err}"),
    };
    if stderr_tail.is_empty() {
        head
    } else {
        format!("{head}。stderr 末尾：{}", stderr_tail.join(" / "))
    }
}
