//! 远端工具代理（MCP server）。
//!
//! 中心节点跑 AI 循环，工具调用经这里 → 策略引擎 → SSH → 目标机上的
//! `ai-task-agent` 执行。
//!
//! 为什么工具代理必须是 MCP server 而不是只靠 hook：这个进程是我们自己的，
//! 每一个远端动作**必然**流经它，策略无法被绕过。Claude Code 的 `PreToolUse`
//! hook 作为第二道，覆盖内置工具并统一 deny 的反馈格式。两道都要。
//!
//! **这个进程里没有任何主机凭据。**它拿到的只是一个 run 级别的令牌和中心的
//! 地址；SSH 私钥、目标主机的选择、资源上限全在中心。代理是 Claude Code 拉起
//! 的子进程，它的命令行和环境对模型是可见的——所以它手里能有的东西越少越好。

mod proxy;

pub use proxy::{ProxyConfig, serve_stdio};
