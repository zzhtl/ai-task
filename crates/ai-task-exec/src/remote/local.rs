//! 在本机跑 agent。
//!
//! 与远端走的是**同一套协议和同一个二进制**。这样做的好处不只是代码复用：
//! 本机执行的节点因此也拿到了资源归因和资源上限——`HostSelector::Local` 不再是
//! 「什么都没有」的那一档。
//!
//! 同一套代码路径也意味着远端才会暴露的 bug 在本地就能撞见。

use std::path::Path;
use std::process::Stdio;

use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::process::{Child, ChildStdin};

use super::agent::{AgentError, AgentIo, RemoteAgent};

/// 子进程管道上的传输。
struct LocalIo {
    child: Child,
    stdin: ChildStdin,
    lines: tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
}

#[async_trait::async_trait]
impl AgentIo for LocalIo {
    async fn write_line(&mut self, line: &str) -> Result<(), AgentError> {
        self.stdin
            .write_all(format!("{line}\n").as_bytes())
            .await
            .map_err(|_| AgentError::Disconnected)?;
        self.stdin
            .flush()
            .await
            .map_err(|_| AgentError::Disconnected)?;
        Ok(())
    }

    async fn next_line(&mut self) -> Result<Option<String>, AgentError> {
        self.lines
            .next_line()
            .await
            .map_err(|err| AgentError::Protocol(format!("读取 agent 输出失败：{err}")))
    }
}

impl Drop for LocalIo {
    fn drop(&mut self) {
        // 会话结束就带走进程。留着它会变成没人管的孤儿，
        // 还占着这台机器的资源。
        self.child.start_kill().ok();
    }
}

/// 在本机起一个 agent 并完成握手。
pub async fn start_local_agent(
    agent_path: &Path,
    roots: &[String],
) -> Result<RemoteAgent, AgentError> {
    let mut command = tokio::process::Command::new(agent_path);
    for root in roots {
        command.arg("--root").arg(root);
    }

    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        // agent 的日志走 stderr。转发到本进程的 stderr，不进协议流。
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .map_err(|err| {
            AgentError::Protocol(format!(
                "启动本机 agent {} 失败：{err}。确认二进制存在且可执行",
                agent_path.display()
            ))
        })?;

    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| AgentError::Protocol("拿不到 agent 的 stdin".into()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| AgentError::Protocol("拿不到 agent 的 stdout".into()))?;

    RemoteAgent::handshake(Box::new(LocalIo {
        child,
        stdin,
        lines: BufReader::new(stdout).lines(),
    }))
    .await
}
