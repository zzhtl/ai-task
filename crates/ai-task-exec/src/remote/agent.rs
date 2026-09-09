//! 与 agent 的协议会话。
//!
//! 一行一条 JSON。请求带 id，回应按 id 归属；一次请求会产生零到多条流式消息，
//! 最后必然有一条 `Done`。
//!
//! 传输有两种：SSH 通道（远端）和子进程管道（本机）。抽成 [`AgentIo`] 是因为
//! 两边都必须**把 agent 的 stderr 与协议流分开**——agent 的日志走 stderr，
//! 混进协议流会把解析打乱。russh 的 `into_stream()` 正好会合并两者，所以这里
//! 不能用它。

use ai_task_agent::protocol::{
    AgentInfo, Envelope, ExecRequest, ExecResult, OpResult, PROTOCOL_VERSION, Request, Response,
    Sample,
};
use russh::client;
use tokio::io::AsyncWriteExt as _;

use super::ssh::{SshError, SshSession};

/// 单条协议行的长度上限。
///
/// agent 已经对输出做了截断，正常不会超。设这个上限是为了挡住「远端疯掉了往
/// 管子里灌数据」——那会把中心节点的内存吃光。
const MAX_LINE_BYTES: usize = 8 * 1024 * 1024;

/// 执行过程中的流式事件。
#[derive(Debug, Clone)]
pub enum AgentEvent {
    Stdout(String),
    Stderr(String),
    Metrics(Sample),
}

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error(transparent)]
    Ssh(#[from] SshError),

    #[error("agent 协议错误：{0}")]
    Protocol(String),

    #[error("agent 连接已断开")]
    Disconnected,

    #[error("agent 返回错误：{0}")]
    Remote(String),
}

/// 协议流的传输层。
#[async_trait::async_trait]
pub trait AgentIo: Send {
    /// 发一行（不含换行符）。
    async fn write_line(&mut self, line: &str) -> Result<(), AgentError>;
    /// 收一行。`None` 表示通道已关闭。
    async fn next_line(&mut self) -> Result<Option<String>, AgentError>;
}

/// 一个已握手的 agent 会话。
pub struct RemoteAgent {
    io: Box<dyn AgentIo>,
    info: AgentInfo,
    next_id: u64,
}

impl RemoteAgent {
    /// 经 SSH 起一个远端 agent 并完成握手。
    pub async fn start(
        session: &SshSession,
        agent_path: &str,
        roots: &[String],
    ) -> Result<Self, AgentError> {
        let channel = session.start_agent(agent_path, roots).await?;
        Self::handshake(Box::new(SshIo {
            channel,
            buffer: Vec::new(),
        }))
        .await
    }

    /// 在给定传输上完成握手。
    pub async fn handshake(io: Box<dyn AgentIo>) -> Result<Self, AgentError> {
        let mut agent = Self {
            io,
            // 握手前先放一个占位；下面立刻会被真实值替换
            info: AgentInfo {
                ai_clis: Vec::new(),
                protocol: 0,
                agent_version: String::new(),
                arch: String::new(),
                hostname: String::new(),
                cgroup_mode: ai_task_agent::protocol::CgroupMode::None,
                cgroup_detail: None,
            },
            next_id: 0,
        };

        let id = agent
            .send(Request::Hello {
                protocol: PROTOCOL_VERSION,
            })
            .await?;
        let mut info = None;
        let result = agent.collect(id, |_| {}).await.map_err(|err| match err {
            // 握手就断说明二进制根本没起来，这个提示比"连接断开"有用
            AgentError::Disconnected => {
                AgentError::Protocol("agent 没有响应握手。确认目标机架构与推送的二进制匹配".into())
            }
            other => other,
        })?;
        // Hello 是在 Done 之前作为流式消息发来的
        for response in result.1 {
            if let Response::Hello(hello) = response {
                info = Some(hello);
            }
        }
        match result.0 {
            OpResult::Ok => {}
            OpResult::Error { message } => return Err(AgentError::Remote(message)),
            other => return Err(AgentError::Protocol(format!("握手返回了 {other:?}"))),
        }

        agent.info = info.ok_or_else(|| AgentError::Protocol("握手没有返回 agent 信息".into()))?;
        Ok(agent)
    }

    #[must_use]
    pub fn info(&self) -> &AgentInfo {
        &self.info
    }

    /// 执行一条命令，流式事件通过回调交出去。
    pub async fn exec(
        &mut self,
        request: ExecRequest,
        on_event: impl FnMut(AgentEvent),
    ) -> Result<ExecResult, AgentError> {
        let id = self.send(Request::Exec(request)).await?;
        let (result, _) = self.collect(id, on_event).await?;
        match result {
            OpResult::Exec(exec) => Ok(exec),
            OpResult::Error { message } => Err(AgentError::Remote(message)),
            other => Err(AgentError::Protocol(format!("exec 返回了 {other:?}"))),
        }
    }

    /// 读一个文件。
    pub async fn read(&mut self, path: &str, max_bytes: u64) -> Result<(String, bool), AgentError> {
        let id = self
            .send(Request::Read {
                path: path.into(),
                max_bytes,
            })
            .await?;
        match self.collect(id, |_| {}).await?.0 {
            OpResult::Read { content, truncated } => Ok((content, truncated)),
            OpResult::Error { message } => Err(AgentError::Remote(message)),
            other => Err(AgentError::Protocol(format!("read 返回了 {other:?}"))),
        }
    }

    /// 写一个文件。
    pub async fn write(&mut self, path: &str, content: &str) -> Result<(), AgentError> {
        let id = self
            .send(Request::Write {
                path: path.into(),
                content: content.into(),
            })
            .await?;
        match self.collect(id, |_| {}).await?.0 {
            OpResult::Ok => Ok(()),
            OpResult::Error { message } => Err(AgentError::Remote(message)),
            other => Err(AgentError::Protocol(format!("write 返回了 {other:?}"))),
        }
    }

    /// 按 glob 列文件。
    pub async fn glob(&mut self, root: &str, pattern: &str) -> Result<Vec<String>, AgentError> {
        let id = self
            .send(Request::Glob {
                root: root.into(),
                pattern: pattern.into(),
            })
            .await?;
        match self.collect(id, |_| {}).await?.0 {
            OpResult::Glob { paths } => Ok(paths),
            OpResult::Error { message } => Err(AgentError::Remote(message)),
            other => Err(AgentError::Protocol(format!("glob 返回了 {other:?}"))),
        }
    }

    /// 让 agent 正常退出。
    pub async fn shutdown(&mut self) -> Result<(), AgentError> {
        let id = self.send(Request::Shutdown).await?;
        // 对端可能在回 Done 之前就关了通道，那也算正常结束
        match self.collect(id, |_| {}).await {
            Ok(_) | Err(AgentError::Disconnected) => Ok(()),
            Err(other) => Err(other),
        }
    }

    async fn send(&mut self, request: Request) -> Result<u64, AgentError> {
        self.next_id += 1;
        let json = serde_json::to_string(&Envelope {
            id: self.next_id,
            request,
        })
        .map_err(|e| AgentError::Protocol(e.to_string()))?;

        self.io.write_line(&json).await?;
        Ok(self.next_id)
    }

    /// 收到指定 id 的 `Done` 为止。
    async fn collect(
        &mut self,
        id: u64,
        mut on_event: impl FnMut(AgentEvent),
    ) -> Result<(OpResult, Vec<Response>), AgentError> {
        let mut others = Vec::new();
        loop {
            let Some(line) = self.io.next_line().await? else {
                return Err(AgentError::Disconnected);
            };
            let response: Response = match serde_json::from_str(&line) {
                Ok(response) => response,
                Err(err) => {
                    // agent 往 stdout 混了非协议内容，或者版本对不上。
                    // 记下来继续读——一行坏数据不该让整个会话报废。
                    tracing::warn!(error = %err, line = %truncate(&line), "无法解析 agent 输出");
                    continue;
                }
            };
            match response {
                Response::Done { id: got, result } if got == id => return Ok((result, others)),
                Response::Stdout { id: got, data } if got == id => {
                    on_event(AgentEvent::Stdout(data));
                }
                Response::Stderr { id: got, data } if got == id => {
                    on_event(AgentEvent::Stderr(data));
                }
                Response::Metrics { id: got, sample } if got == id => {
                    on_event(AgentEvent::Metrics(sample));
                }
                other => others.push(other),
            }
        }
    }
}

/// SSH 通道上的传输。
struct SshIo {
    channel: russh::Channel<client::Msg>,
    buffer: Vec<u8>,
}

#[async_trait::async_trait]
impl AgentIo for SshIo {
    async fn write_line(&mut self, line: &str) -> Result<(), AgentError> {
        let mut writer = self.channel.make_writer();
        writer
            .write_all(format!("{line}\n").as_bytes())
            .await
            .map_err(|_| AgentError::Disconnected)?;
        writer.flush().await.map_err(|_| AgentError::Disconnected)?;
        Ok(())
    }

    async fn next_line(&mut self) -> Result<Option<String>, AgentError> {
        loop {
            if let Some(line) = take_line(&mut self.buffer) {
                return Ok(Some(line));
            }
            if self.buffer.len() > MAX_LINE_BYTES {
                return Err(AgentError::Protocol(format!(
                    "单行超过 {MAX_LINE_BYTES} 字节，疑似远端异常输出"
                )));
            }

            let Some(message) = self.channel.wait().await else {
                return Ok(None);
            };
            match message {
                russh::ChannelMsg::Data { data } => self.buffer.extend_from_slice(&data),
                // agent 的日志走 stderr。转到本地日志，**不进协议缓冲**——
                // 混进去会把 JSON 行解析打乱。
                russh::ChannelMsg::ExtendedData { data, .. } => log_agent_stderr(&data),
                russh::ChannelMsg::Eof | russh::ChannelMsg::Close => return Ok(None),
                _ => {}
            }
        }
    }
}

/// 从缓冲里切出一整行（含处理 CRLF）。
fn take_line(buffer: &mut Vec<u8>) -> Option<String> {
    let position = buffer.iter().position(|b| *b == b'\n')?;
    let line: Vec<u8> = buffer.drain(..=position).collect();
    Some(
        String::from_utf8_lossy(&line[..line.len() - 1])
            .trim_end_matches('\r')
            .to_string(),
    )
}

fn log_agent_stderr(data: &[u8]) {
    let text = String::from_utf8_lossy(data);
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        tracing::debug!(target: "agent::stderr", "{line}");
    }
}

fn truncate(line: &str) -> String {
    line.chars().take(200).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_framing_handles_partial_and_multiple_lines() {
        // SSH 的 Data 消息边界与协议行边界无关：一条消息可能带半行，
        // 也可能带三行半
        let mut buffer: Vec<u8> = Vec::new();
        buffer.extend_from_slice(b"{\"a\":1}\n{\"b\"");
        let first = buffer.iter().position(|b| *b == b'\n').expect("有换行");
        let line: Vec<u8> = buffer.drain(..=first).collect();
        assert_eq!(
            String::from_utf8_lossy(&line[..line.len() - 1]),
            r#"{"a":1}"#
        );
        assert_eq!(buffer, b"{\"b\"");

        buffer.extend_from_slice(b":2}\n");
        let second = buffer.iter().position(|b| *b == b'\n').expect("有换行");
        let line: Vec<u8> = buffer.drain(..=second).collect();
        assert_eq!(
            String::from_utf8_lossy(&line[..line.len() - 1]),
            r#"{"b":2}"#
        );
        assert!(buffer.is_empty());
    }

    #[test]
    fn the_line_cap_is_generous_but_finite() {
        // agent 已经截断过输出，正常到不了这里；这是防远端疯掉的兜底
        const { assert!(MAX_LINE_BYTES >= 1024 * 1024) };
        const { assert!(MAX_LINE_BYTES <= 64 * 1024 * 1024) };
    }
}
