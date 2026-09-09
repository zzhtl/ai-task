//! 远端执行。
//!
//! 中心跑 AI 循环，工具调用经策略层判决后经 SSH 下发到目标机上的 agent 执行。
//! 目标机零依赖：只要有 sshd，不需要 Node、不需要 claude、不需要 API key。

pub mod agent;
pub mod local;
pub mod ssh;

pub use agent::{AgentError, AgentEvent, AgentIo, RemoteAgent};
pub use local::start_local_agent;
pub use ssh::{HostKeyPolicy, SshConfig, SshError, SshSession, sha256_hex};
