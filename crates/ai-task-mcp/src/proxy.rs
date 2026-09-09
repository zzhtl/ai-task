//! stdio MCP server：把远端工具调用转发给中心。

use ai_task_proto::{RemoteAction, RemoteCallRequest, RemoteCallResponse};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerInfo};
use rmcp::{ErrorData as McpError, ServerHandler, tool, tool_handler, tool_router};
use serde::Deserialize;

/// 代理进程需要知道的全部东西。**不含任何凭据。**
#[derive(Debug, Clone)]
pub struct ProxyConfig {
    /// 中心的 `/internal/remote/call` 地址。
    pub endpoint: String,
    /// run 级令牌。只能替这一个 run 请求执行。
    pub token: String,
    pub run_id: ai_task_proto::RunId,
    pub node_key: String,
}

#[derive(Clone)]
struct RemoteTools {
    config: ProxyConfig,
    http: reqwest::Client,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct BashParams {
    /// 要在目标机上执行的命令，交给 `sh -c`。
    command: String,
    /// 工作目录。不给就用登录用户的家目录。
    #[serde(default)]
    working_dir: Option<String>,
    /// 墙钟上限（秒），默认 300。
    #[serde(default)]
    timeout_s: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ReadParams {
    /// 目标机上的绝对路径。
    path: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct WriteParams {
    /// 目标机上的绝对路径。父目录必须已存在。
    path: String,
    /// 文件的完整内容，会整个覆盖原文件。
    content: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct GlobParams {
    /// shell glob，如 `/var/log/*.log`。最多返回 500 条。
    pattern: String,
}

#[tool_router]
impl RemoteTools {
    fn new(config: ProxyConfig) -> Self {
        Self {
            config,
            http: reqwest::Client::new(),
        }
    }

    /// 在这个任务节点声明的目标机上执行一条命令。
    /// 目标机由任务定义决定，不能在调用里指定。
    #[tool(name = "remote_bash")]
    async fn remote_bash(
        &self,
        Parameters(params): Parameters<BashParams>,
    ) -> Result<CallToolResult, McpError> {
        self.call(RemoteAction::RemoteBash {
            command: params.command,
            working_dir: params.working_dir,
            timeout_s: params.timeout_s,
        })
        .await
    }

    /// 读目标机上的一个文件。
    #[tool(name = "remote_read")]
    async fn remote_read(
        &self,
        Parameters(params): Parameters<ReadParams>,
    ) -> Result<CallToolResult, McpError> {
        self.call(RemoteAction::RemoteRead { path: params.path })
            .await
    }

    /// 覆盖写目标机上的一个文件。
    #[tool(name = "remote_write")]
    async fn remote_write(
        &self,
        Parameters(params): Parameters<WriteParams>,
    ) -> Result<CallToolResult, McpError> {
        self.call(RemoteAction::RemoteWrite {
            path: params.path,
            content: params.content,
        })
        .await
    }

    /// 在目标机上按 glob 列文件。
    #[tool(name = "remote_glob")]
    async fn remote_glob(
        &self,
        Parameters(params): Parameters<GlobParams>,
    ) -> Result<CallToolResult, McpError> {
        self.call(RemoteAction::RemoteGlob {
            pattern: params.pattern,
        })
        .await
    }
}

impl RemoteTools {
    async fn call(&self, action: RemoteAction) -> Result<CallToolResult, McpError> {
        let request = RemoteCallRequest {
            run_id: self.config.run_id,
            node_key: self.config.node_key.clone(),
            tool_use_id: uuid::Uuid::now_v7().to_string(),
            action,
        };

        let response = self
            .http
            .post(&self.config.endpoint)
            .header("x-ai-task-token", &self.config.token)
            .json(&request)
            .send()
            .await;

        // 中心不可达时**不能**假装成功，也不该让整个会话崩掉：
        // 作为一次失败的工具结果回给模型，它自己会调整。
        let response = match response {
            Ok(response) => response,
            Err(err) => {
                return Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                    "连不上控制平面，这次远端动作没有执行：{err}"
                ))]));
            }
        };
        if !response.status().is_success() {
            let status = response.status();
            return Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                "控制平面拒绝了这次请求（HTTP {status}），远端动作没有执行"
            ))]));
        }

        let body: RemoteCallResponse = match response.json().await {
            Ok(body) => body,
            Err(err) => {
                return Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                    "控制平面的响应解析失败：{err}"
                ))]));
            }
        };

        // 被拒绝的调用要标成 error：模型据此知道该换做法，
        // 而不是把拒绝原因当成命令的输出继续往下推理。
        let content = vec![ContentBlock::text(body.content)];
        Ok(if body.executed && body.exit_code.unwrap_or(0) == 0 {
            CallToolResult::success(content)
        } else {
            CallToolResult::error(content)
        })
    }
}

#[tool_handler]
impl ServerHandler for RemoteTools {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(
                "ai-task-remote",
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(
                "在这个任务节点声明的远程主机上执行动作。目标主机由任务定义决定，\
                 调用时不需要也不能指定。每次调用都会先过策略引擎，\
                 被拒绝时会告诉你原因——照着原因换个做法，不要原样重试。",
            )
    }
}

/// 在 stdin/stdout 上跑这个 MCP server，直到对端断开。
pub async fn serve_stdio(config: ProxyConfig) -> anyhow::Result<()> {
    use rmcp::ServiceExt as _;
    let service = RemoteTools::new(config)
        .serve(rmcp::transport::io::stdio())
        .await?;
    service.waiting().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_advertised_tools_are_exactly_the_four_remote_actions() {
        // 多一个工具就是多一条策略要覆盖的路径。这里钉死。
        let tools = RemoteTools::tool_router();
        let mut names: Vec<String> = tools
            .list_all()
            .into_iter()
            .map(|t| t.name.to_string())
            .collect();
        names.sort();
        assert_eq!(
            names,
            ["remote_bash", "remote_glob", "remote_read", "remote_write"]
        );
    }

    #[test]
    fn the_proxy_config_carries_no_credentials() {
        // 这个结构体是代理进程知道的全部。它是 Claude Code 拉起来的子进程，
        // 命令行和环境对模型可见——加一个私钥字段就是把凭据递到模型手里。
        //
        // 穷举解构：新增字段会让这里编译不过，而不是等到某天审计时才发现。
        let ProxyConfig {
            endpoint: _,
            token: _,
            run_id: _,
            node_key: _,
        } = ProxyConfig {
            endpoint: "http://127.0.0.1:8930/internal/remote/call".into(),
            token: "t".into(),
            run_id: ai_task_proto::RunId::new(),
            node_key: "n".into(),
        };
    }
}
