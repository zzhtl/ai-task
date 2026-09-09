//! Run 事件日志。
//!
//! 这是整个系统的中枢契约。一次 run 的状态**不是**数据库里的一个 status 字段，
//! 而是这串 append-only 事件的 fold。一次投入换来四件事：
//!
//! - 实时监控直接 tail 事件流，不轮询
//! - 时间旅行调试：拖时间轴即可重建任意时刻的完整状态
//! - 审计天然完备：谁在什么时候让 AI 做了什么、调了什么工具、被哪条策略拦了
//! - 崩溃恢复靠重放，不写 checkpoint
//!
//! **资源采样不在这里。** 1Hz × N 节点会把事件日志撑爆，走独立的 `run_metrics`
//! 表和独立的 SSE 通道。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::ids::{ApprovalId, HostId, NodeKey, RuleId, RunId, TaskVersionId};
use crate::money::UsdMicros;
use crate::spec::{NodeStatus, PolicyEffect, RunStatus, TriggerKind};

/// 事件日志里的一条记录。
///
/// 字段与 `run_events` 表一一对应：`(run_id, seq)` 是主键，`body` 落在 `payload`
/// 这个 jsonb 列里，`kind` 另外冗余成一列供过滤用。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RunEvent {
    pub run_id: RunId,
    /// run 内单调递增，从 1 开始。
    ///
    /// 由该 run 的单写者任务分配，不是全局序列——没有跨 run 的竞争点。
    /// SSE 的 `Last-Event-ID` 直接用它做断点续传。
    pub seq: i64,
    pub ts: DateTime<Utc>,
    /// 事件归属的节点。run 级事件为 `None`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_key: Option<NodeKey>,
    pub body: RunEventBody,
}

impl RunEvent {
    /// 事件类型的稳定标识，对应 `run_events.kind` 列。
    #[must_use]
    pub fn kind(&self) -> &'static str {
        self.body.kind()
    }
}

/// 事件负载。
///
/// **新增 variant 是兼容变更，客户端必须容忍未知 `kind`。** 前端对未识别的事件
/// 应当降级显示为一行原始 JSON，而不是崩溃或静默丢弃。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RunEventBody {
    // ---- run 生命周期 ----
    /// run 已入库，等待调度。事件流的第一条，永远是 seq=1。
    RunQueued {
        task_version_id: TaskVersionId,
        trigger: TriggerKind,
        /// 触发时传入的参数。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        inputs: Option<serde_json::Value>,
        /// 影子执行：所有副作用工具只记录意图不执行。
        dry_run: bool,
    },
    /// worker 已领取，开始执行。
    RunStarted {
        worker: String,
    },
    /// 终态。之后不会再有本 run 的事件，SSE 可以关闭。
    RunFinished {
        status: RunStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
        /// run 全程累计成本。
        cost_usd: UsdMicros,
    },

    // ---- 节点生命周期 ----
    /// 入边条件已满足，等 worker 领取。
    NodeReady,
    NodeStarted {
        attempt: u32,
        /// `None` 表示在中心节点本机执行。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        host_id: Option<HostId>,
    },
    NodeFinished {
        attempt: u32,
        status: NodeStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// 本次尝试失败，将在 `delay_ms` 后重试。
    NodeRetrying {
        attempt: u32,
        delay_ms: u32,
        reason: String,
        /// 是否把 `reason` 回喂给模型（节点契约化的执行点）。
        feeding_error_to_model: bool,
    },
    /// 入边条件不满足，整支被跳过。
    NodeSkipped {
        reason: String,
    },
    /// 输入条件没变，输出却变了。
    ///
    /// **只在真该告警时才发。**输入条件变了而输出跟着变，那是改动生效了，
    /// 不是漂移——那种情况不发这条事件，界面上照样能看到 diff。
    DriftDetected {
        /// 拿来比较的基线 run。
        baseline_run_id: RunId,
        /// 有差异的字段路径，最多列前几条。完整 diff 走 `/runs/{id}/drift`。
        changed_paths: Vec<String>,
    },
    /// 目标机做不到 cgroup 级归因，只能降级采样。
    ///
    /// **这条必须显式发出来。**资源曲线在降级档位下是不完整的（`/proc` 采样
    /// 收不到已经退出的进程的峰值，`none` 档连数都没有），而且**上限根本没被强制**
    /// ——界面上不标出来的话，用户会以为 `MemoryMax` 生效了。
    ResourceDegraded {
        /// `proc` 或 `none`。`systemd` 不会发这条事件。
        mode: String,
        /// 降级的原因，直接来自目标机的探测结果。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    /// map 节点按上游数组展开成了 `count` 个子节点实例。
    MapExpanded {
        count: u32,
        /// 上游数组的真实长度。被 `max_items` 截断时会大于 `count`。
        available: u32,
    },

    // ---- AI 执行内部 ----
    AgentTurnStarted {
        turn: u32,
    },
    /// 模型的思考摘要（`thinking.display = "summarized"` 时才有内容）。
    AgentThinking {
        text: String,
    },
    /// 模型的可见输出增量。
    AgentText {
        text: String,
    },
    /// 模型请求调用工具。**此时还没有执行**，要先过策略层。
    ToolRequested {
        tool_use_id: String,
        tool: String,
        input: serde_json::Value,
    },
    /// 策略判决。这是审计链路的关键一环：能回答「第 37 次工具调用为什么被拒」。
    PolicyDecided {
        tool_use_id: String,
        effect: PolicyEffect,
        /// 命中的规则。没有规则命中（走默认策略）时为 `None`。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rule_id: Option<RuleId>,
        reason: String,
    },
    /// 工具执行完毕。输出只存摘要，全文在节点输出或对象存储里。
    ToolCompleted {
        tool_use_id: String,
        ok: bool,
        output_preview: String,
        duration_ms: u64,
    },
    /// 一次模型调用的用量。累加即 run 成本。
    Usage {
        model: String,
        input_tokens: u64,
        output_tokens: u64,
        cache_read_tokens: u64,
        cache_creation_tokens: u64,
        cost_usd: UsdMicros,
    },

    // ---- 人在回路 ----
    /// 走到 approval 节点，或工具调用命中 `ask` 策略。
    ApprovalRequested {
        approval_id: ApprovalId,
        title: String,
        /// 结构化的意图：要跑的命令、要改的文件、影响的主机。
        /// 审批卡片渲染的是它，不是一段自然语言。
        intent: serde_json::Value,
        expires_at: DateTime<Utc>,
    },
    ApprovalDecided {
        approval_id: ApprovalId,
        approved: bool,
        /// 超时自动决策时为 `None`。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        decided_by: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },

    // ---- 其它 ----
    /// 执行器或引擎的诊断输出。不参与状态推导。
    Log {
        level: LogLevel,
        message: String,
    },
}

impl RunEventBody {
    /// 事件类型的稳定标识，对应 `run_events.kind` 列。
    ///
    /// 手写而不是从 serde 反推：这些字符串是持久化到数据库的契约，
    /// 改 Rust 变体名不应该悄悄改掉历史行的语义。
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::RunQueued { .. } => "run_queued",
            Self::RunStarted { .. } => "run_started",
            Self::RunFinished { .. } => "run_finished",
            Self::NodeReady => "node_ready",
            Self::NodeStarted { .. } => "node_started",
            Self::NodeFinished { .. } => "node_finished",
            Self::NodeRetrying { .. } => "node_retrying",
            Self::NodeSkipped { .. } => "node_skipped",
            Self::DriftDetected { .. } => "drift_detected",
            Self::ResourceDegraded { .. } => "resource_degraded",
            Self::MapExpanded { .. } => "map_expanded",
            Self::AgentTurnStarted { .. } => "agent_turn_started",
            Self::AgentThinking { .. } => "agent_thinking",
            Self::AgentText { .. } => "agent_text",
            Self::ToolRequested { .. } => "tool_requested",
            Self::PolicyDecided { .. } => "policy_decided",
            Self::ToolCompleted { .. } => "tool_completed",
            Self::Usage { .. } => "usage",
            Self::ApprovalRequested { .. } => "approval_requested",
            Self::ApprovalDecided { .. } => "approval_decided",
            Self::Log { .. } => "log",
        }
    }

    /// 高频事件：AI 的流式增量。
    ///
    /// 归档时可以只保留这类事件的聚合结果，它们占了事件量的大头但
    /// 对状态推导没有贡献。
    #[must_use]
    pub fn is_stream_delta(&self) -> bool {
        matches!(self, Self::AgentText { .. } | Self::AgentThinking { .. })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(body: RunEventBody) -> RunEvent {
        RunEvent {
            run_id: RunId::new(),
            seq: 1,
            ts: Utc::now(),
            node_key: None,
            body,
        }
    }

    #[test]
    fn body_is_a_discriminated_union_on_kind() {
        let e = event(RunEventBody::NodeRetrying {
            attempt: 2,
            delay_ms: 2_000,
            reason: "schema 校验失败".into(),
            feeding_error_to_model: true,
        });
        let json = serde_json::to_value(&e).expect("序列化");
        assert_eq!(json["body"]["kind"], "node_retrying");
        assert_eq!(json["body"]["attempt"], 2);
        assert_eq!(json["seq"], 1);
    }

    #[test]
    fn kind_string_matches_serde_tag_for_every_variant() {
        // kind() 是落库的契约，必须和线上的 tag 逐字一致，否则
        // run_events.kind 列会和 payload 里的 kind 对不上。
        let bodies = all_variants();
        assert_eq!(bodies.len(), 21, "新增 variant 时请补进 all_variants()");
        for body in bodies {
            let json = serde_json::to_value(&body).expect("序列化");
            assert_eq!(
                json["kind"].as_str(),
                Some(body.kind()),
                "{:?} 的 kind() 与 serde tag 不一致",
                body.kind()
            );
        }
    }

    #[test]
    fn every_variant_round_trips() {
        for body in all_variants() {
            let json = serde_json::to_value(&body).expect("序列化");
            let back: RunEventBody = serde_json::from_value(json).expect("反序列化");
            assert_eq!(back, body);
        }
    }

    #[test]
    fn run_level_events_omit_node_key() {
        let json = serde_json::to_value(event(RunEventBody::NodeReady)).expect("序列化");
        assert!(json.get("node_key").is_none());
    }

    #[test]
    fn money_in_events_is_a_string() {
        let json = serde_json::to_value(RunEventBody::RunFinished {
            status: RunStatus::Succeeded,
            error: None,
            cost_usd: UsdMicros(42_500),
        })
        .expect("序列化");
        assert_eq!(json["cost_usd"], serde_json::json!("0.042500"));
    }

    #[test]
    fn stream_deltas_are_identifiable_for_archival() {
        assert!(RunEventBody::AgentText { text: "x".into() }.is_stream_delta());
        assert!(RunEventBody::AgentThinking { text: "x".into() }.is_stream_delta());
        assert!(!RunEventBody::NodeReady.is_stream_delta());
    }

    fn all_variants() -> Vec<RunEventBody> {
        vec![
            RunEventBody::RunQueued {
                task_version_id: TaskVersionId::new(),
                trigger: TriggerKind::Schedule,
                inputs: None,
                dry_run: false,
            },
            RunEventBody::RunStarted {
                worker: "w-1".into(),
            },
            RunEventBody::RunFinished {
                status: RunStatus::Succeeded,
                error: None,
                cost_usd: UsdMicros::ZERO,
            },
            RunEventBody::NodeReady,
            RunEventBody::NodeStarted {
                attempt: 1,
                host_id: Some(HostId::new()),
            },
            RunEventBody::NodeFinished {
                attempt: 1,
                status: NodeStatus::Succeeded,
                output: Some(serde_json::json!({"ok": true})),
                error: None,
            },
            RunEventBody::DriftDetected {
                baseline_run_id: RunId::new(),
                changed_paths: vec!["errors".into()],
            },
            RunEventBody::ResourceDegraded {
                mode: "proc".into(),
                detail: Some("目标机没有 systemd".into()),
            },
            RunEventBody::NodeRetrying {
                attempt: 1,
                delay_ms: 1_000,
                reason: "boom".into(),
                feeding_error_to_model: true,
            },
            RunEventBody::NodeSkipped {
                reason: "入边条件不满足".into(),
            },
            RunEventBody::MapExpanded {
                count: 3,
                available: 3,
            },
            RunEventBody::AgentTurnStarted { turn: 1 },
            RunEventBody::AgentThinking { text: "…".into() },
            RunEventBody::AgentText { text: "…".into() },
            RunEventBody::ToolRequested {
                tool_use_id: "toolu_1".into(),
                tool: "remote_bash".into(),
                input: serde_json::json!({"command": "df -h"}),
            },
            RunEventBody::PolicyDecided {
                tool_use_id: "toolu_1".into(),
                effect: PolicyEffect::Deny,
                rule_id: Some(RuleId::new()),
                reason: "生产机禁止递归删除".into(),
            },
            RunEventBody::ToolCompleted {
                tool_use_id: "toolu_1".into(),
                ok: true,
                output_preview: "…".into(),
                duration_ms: 12,
            },
            RunEventBody::Usage {
                model: "claude-opus-5".into(),
                input_tokens: 100,
                output_tokens: 20,
                cache_read_tokens: 0,
                cache_creation_tokens: 0,
                cost_usd: UsdMicros(1_000),
            },
            RunEventBody::ApprovalRequested {
                approval_id: ApprovalId::new(),
                title: "在 prod-1 上执行 systemctl restart".into(),
                intent: serde_json::json!({"command": "systemctl restart nginx"}),
                expires_at: Utc::now(),
            },
            RunEventBody::ApprovalDecided {
                approval_id: ApprovalId::new(),
                approved: false,
                decided_by: Some("zzh".into()),
                reason: None,
            },
            RunEventBody::Log {
                level: LogLevel::Warn,
                message: "cgroup v2 不可用，降级到 /proc 采样".into(),
            },
        ]
    }
}
