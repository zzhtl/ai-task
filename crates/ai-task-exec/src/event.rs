//! 执行器事件。
//!
//! 与 `ai_task_proto::RunEventBody` 是两件事：`ExecEvent` 描述「一次执行内部
//! 发生了什么」，不带 run / seq / 节点身份，也不知道自己是第几次重试。
//! 把它翻译成带身份的 `RunEventBody` 是 `ai-task-runtime` 的职责。

use ai_task_proto::UsdMicros;

/// 执行过程中的一件事。
#[derive(Debug, Clone, PartialEq)]
pub enum ExecEvent {
    /// 执行器已就位。带上实际生效的配置，用于诊断
    /// 「为什么这次和上次表现不一样」。
    Started {
        session_id: String,
        model: String,
        /// 实际可用的工具。**密闭执行下应当只有请求的那几个**；
        /// 出现意外的工具说明配置隔离没生效。
        tools: Vec<String>,
        cli_version: Option<String>,
    },
    /// 模型的思考摘要。
    Thinking { text: String },
    /// 模型的可见输出。
    Text { text: String },
    /// 模型请求调用工具。此时尚未执行。
    ToolRequested {
        tool_use_id: String,
        tool: String,
        input: serde_json::Value,
    },
    /// 工具执行完毕。
    ToolCompleted {
        tool_use_id: String,
        outcome: ToolOutcome,
        /// 输出摘要。全文不进事件日志。
        output_preview: String,
    },
    /// 工具调用被权限层拦下。
    ///
    /// M2 起策略层会主动产生拒绝；这里捕获的是 CLI 自身的权限判定，
    /// 两者都要落到审计里。
    PermissionDenied {
        tool_use_id: String,
        tool: String,
        reason: String,
    },
    /// 权威用量。**整个执行只会有一条**，来自 CLI 的 `result` 事件。
    ///
    /// 见 [`crate::claude_code::decode`] 里对「为什么不用逐条 assistant 事件
    /// 的 usage」的说明。
    Usage {
        model: String,
        input_tokens: u64,
        output_tokens: u64,
        cache_read_tokens: u64,
        cache_creation_tokens: u64,
        cost_usd: UsdMicros,
    },
    /// 执行结束。事件流的最后一条。
    Finished(ExecOutcome),
    /// 解析层遇到了不认识或不合法的输入。
    ///
    /// **不中断执行**：CLI 加了个新事件类型不该让 run 挂掉。
    Warning { message: String },
}

/// 工具调用的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolOutcome {
    Ok,
    Error,
}

/// 执行的终态。
#[derive(Debug, Clone, PartialEq)]
pub enum ExecOutcome {
    Success {
        /// 模型的最终输出。声明了 `output_schema` 时是一段 JSON 文本。
        result: String,
        turns: u32,
    },
    Failed {
        reason: String,
    },
    /// 命中成本上限被熔断。
    BudgetExceeded,
    /// 被取消令牌中止。
    Cancelled,
}

impl ExecOutcome {
    #[must_use]
    pub fn is_success(&self) -> bool {
        matches!(self, Self::Success { .. })
    }

    /// 给日志和错误信息用的短标签。
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::Success { .. } => "success",
            Self::Failed { .. } => "failed",
            Self::BudgetExceeded => "budget_exceeded",
            Self::Cancelled => "cancelled",
        }
    }
}

impl ExecEvent {
    /// 是不是终止事件。收到之后不应再有其它事件。
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Finished(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_finished_is_terminal() {
        assert!(ExecEvent::Finished(ExecOutcome::Cancelled).is_terminal());
        assert!(!ExecEvent::Text { text: "x".into() }.is_terminal());
        assert!(
            !ExecEvent::Warning {
                message: "x".into()
            }
            .is_terminal(),
            "解析告警不能终止执行：CLI 加个新事件类型不该让 run 挂掉"
        );
    }

    #[test]
    fn outcome_labels_are_stable() {
        assert_eq!(ExecOutcome::BudgetExceeded.label(), "budget_exceeded");
        assert!(!ExecOutcome::BudgetExceeded.is_success());
        assert!(
            ExecOutcome::Success {
                result: "4".into(),
                turns: 5
            }
            .is_success()
        );
    }
}
