//! 执行后端。
//!
//! 对上暴露统一的 [`Executor`]，两个实现产出同一套 [`ExecEvent`]，
//! 上层（`ai-task-runtime`）不需要知道底下跑的是 CLI 还是 API：
//!
//! - [`claude_code::ClaudeCodeExecutor`]：驱动 `claude -p --output-format stream-json`。
//!   开放式任务用它——完整工具集、上下文压缩、hook、预算熔断都是白送的。
//! - `ApiExecutor`（M3）：自建 Messages API 循环，给 assert 这类封闭式推理用。
//!
//! 这个 crate 同时是**防腐层**。CLI 的 `stream-json` 不是稳定契约：字段会加、
//! 会改、会重排。所有解析都关在 [`claude_code::decode`] 里，它的回归基线是
//! `tests/fixtures/` 下真实录制的 JSONL。CLI 变了，坏的是那一层的测试，
//! 不是整个引擎。

pub mod claude_code;
pub mod event;
pub mod remote;
pub mod remote_cli;
pub mod workdir;

pub use event::{ExecEvent, ExecOutcome, ToolOutcome};
pub use workdir::{
    HookConfig, MCP_SERVER_NAME, McpProxyConfig, RunWorkdir, SkillFiles, WorkdirError,
    remote_tool_names,
};

use ai_task_proto::NodeKey;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// 一次节点执行的输入。
#[derive(Debug, Clone)]
pub struct ExecRequest {
    /// 事件归属的节点，只用于日志。
    pub node_key: NodeKey,
    /// 已经把 inputs 渲染进去的完整提示词。
    pub prompt: String,
    /// 工作目录。执行器不会创建它，调用方负责。
    pub workdir: std::path::PathBuf,
    pub model: Option<String>,
    pub effort: Option<ai_task_proto::Effort>,
    /// 允许的内置工具。`None` 表示用执行器的默认白名单。
    pub tools: Option<Vec<String>>,
    /// 输出契约。给上就要求模型产出符合该 schema 的 JSON。
    pub output_schema: Option<serde_json::Value>,
    /// 本节点的成本上限。
    pub budget_usd: Option<ai_task_proto::UsdMicros>,
    /// 追加到系统提示的规则文本（软规则）。
    pub system_append: Option<String>,
    /// 会话 ID。同一节点重试时复用它即可续上上下文。
    pub session_id: uuid::Uuid,
    /// 本次执行是否勾选了技能包。
    ///
    /// 勾了就要额外带上 `Skill` 工具——技能包落了地但模型调不动，
    /// 表现是「配了但没生效」。
    pub has_skills: bool,
    /// 额外的 settings 文件（`PreToolUse` hook 就配在里面）。
    ///
    /// 与 `--setting-sources ""` 并存：后者只关掉**宿主的** user/project/local
    /// 配置，显式给的 `--settings` 仍然生效。已实测确认。
    pub settings_path: Option<std::path::PathBuf>,
    /// 远端工具代理的 `--mcp-config` 文件。
    ///
    /// 给上就意味着这个节点在远程机器上干活：`tools` 里必须去掉本地的
    /// Bash/Read/Write/Edit，只留 `mcp__ai_task_remote__*`。否则模型会
    /// 老老实实在**中心机**上执行本地工具，而那不是它以为的目标机。
    pub mcp_config: Option<std::path::PathBuf>,
}

/// 一次执行的句柄。
#[derive(Debug)]
pub struct ExecHandle {
    /// 事件流。发送端关闭即表示执行结束。
    pub events: mpsc::Receiver<ExecEvent>,
    /// 取消令牌。触发后执行器负责杀掉子进程并发出
    /// [`ExecOutcome::Cancelled`]。
    pub cancel: CancellationToken,
}

#[async_trait::async_trait]
pub trait Executor: Send + Sync {
    /// 启动一次执行，立即返回句柄；真正的工作在后台任务里跑。
    async fn spawn(&self, request: ExecRequest) -> Result<ExecHandle, ExecError>;

    /// 用于日志和 `runs.cli_version` 之类的诊断字段。
    fn name(&self) -> &'static str;

    /// 这次执行**将要**跑的命令行，参数已按 shell 规则引好。
    ///
    /// 不放在事件流里由 `spawn` 产出：CLI 起不来的时候恰恰是最需要看这条
    /// 命令的时候，而那时事件流还不存在。
    fn command_line(&self, request: &ExecRequest) -> String;
}

#[derive(Debug, thiserror::Error)]
pub enum ExecError {
    #[error("找不到可执行文件 {program}：{source}。确认它在 PATH 里，或用配置指定绝对路径")]
    NotFound {
        program: String,
        #[source]
        source: std::io::Error,
    },

    #[error("启动 {program} 失败：{source}")]
    Spawn {
        program: String,
        #[source]
        source: std::io::Error,
    },

    #[error("工作目录 {0} 不存在")]
    MissingWorkdir(std::path::PathBuf),

    /// 目标机上的执行失败。**不复用 `Spawn`**：那个变体带一个
    /// `std::io::Error`，而远端的失败来自 agent 协议，硬凑一个 io 错误
    /// 只会让日志里出现一个假的 errno。
    #[error("目标机上执行失败：{0}")]
    Remote(String),
}
