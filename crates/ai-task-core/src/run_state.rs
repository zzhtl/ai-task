//! 把事件日志 fold 成 run 的当前状态。
//!
//! 这是「Run 是事件日志，不是状态字段」这条设计的执行点。整个系统里**只有这里**
//! 知道怎么从事件推出状态：调度器、API、前端时间轴回放走的都是同一个函数，
//! 因此不可能出现「界面显示成功、数据库写着失败」这种分叉。
//!
//! 有意不做的事：**不累积 AI 的流式文本**。`agent_text` 增量占事件量的大头，
//! 累进状态里会让每个 run 的内存占用随输出长度线性增长，而服务端的调度决策
//! 从不需要它。前端自己在 SSE 流上拼。

use std::collections::BTreeMap;

use ai_task_proto::{
    ApprovalId, HostId, NodeKey, NodeStatus, PolicyEffect, RunEvent, RunEventBody, RunId,
    RunStatus, TaskVersionId, TriggerKind, UsdMicros,
};
use chrono::{DateTime, Utc};

/// 一次 run 的当前状态，由事件推导而来。
#[derive(Debug, Clone, PartialEq)]
pub struct RunState {
    pub run_id: RunId,
    pub task_version_id: TaskVersionId,
    pub trigger: TriggerKind,
    pub dry_run: bool,
    pub status: RunStatus,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    /// 全 run 累计成本（各节点 `usage` 事件之和）。
    pub cost: UsdMicros,
    pub nodes: BTreeMap<NodeKey, NodeState>,
    /// 尚未决策的审批。引擎据此判断 run 是不是卡在人身上。
    pub open_approvals: BTreeMap<ApprovalId, OpenApproval>,
    /// 已消费的最后一条事件的 seq。SSE 续传的起点。
    pub last_seq: i64,
    pub error: Option<String>,
    /// 输入条件没变、输出却变了。`None` 表示没检出漂移（或没有基线可比）。
    pub drift: Option<DriftFlag>,
}

/// 一次漂移检出。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriftFlag {
    pub baseline_run_id: RunId,
    pub changed_paths: Vec<String>,
}

/// 单个节点实例的状态。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct NodeState {
    pub status: NodeStatus,
    /// 已进行的尝试次数（首次为 1）。
    pub attempt: u32,
    /// 执行所在主机。`None` 表示中心节点本机。
    pub host_id: Option<HostId>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub output: Option<serde_json::Value>,
    pub error: Option<String>,
    /// 目标机实际的归因档位。`None` 表示 systemd（或还没执行）；
    /// `Some("proc")` / `Some("none")` 意味着**资源上限没有被强制**，
    /// 曲线也不完整——界面必须把这件事说出来。
    pub resource_mode: Option<String>,
    pub resource_degraded_reason: Option<String>,
    pub cost: UsdMicros,
    pub tool_calls: u32,
    /// 被策略拒绝的工具调用数。非零就值得在界面上标出来。
    pub denied_tool_calls: u32,
    /// map 节点展开出的实例数。
    pub map_expanded: Option<u32>,
}

/// 等待人工决策的审批。
#[derive(Debug, Clone, PartialEq)]
pub struct OpenApproval {
    pub node_key: Option<NodeKey>,
    pub title: String,
    pub intent: serde_json::Value,
    pub expires_at: DateTime<Utc>,
}

/// fold 过程中发现的事件流问题。
///
/// 这些都不是「业务失败」，而是**事件流本身坏了**——只可能来自 seq 分配 bug
/// 或是客户端把不同 run 的事件混在了一起。遇到就必须整段重放，不能带病继续。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReplayError {
    #[error("事件流的第一条必须是 run_queued，实际是 `{actual}`")]
    NotStartedWithQueued { actual: &'static str },

    #[error("seq 不连续：期望 {expected}，收到 {actual}")]
    SeqGap { expected: i64, actual: i64 },

    #[error("事件属于 run {actual}，但正在重放的是 {expected}")]
    WrongRun { expected: RunId, actual: RunId },

    #[error("run 已进入终态 {status:?}，不应再有事件（seq {seq}，kind `{kind}`）")]
    EventAfterTerminal {
        status: RunStatus,
        seq: i64,
        kind: &'static str,
    },
}

impl RunState {
    /// 从完整事件流重建状态。
    ///
    /// 事件必须从 seq=1 的 `run_queued` 开始且连续。
    pub fn replay<'a>(events: impl IntoIterator<Item = &'a RunEvent>) -> Result<Self, ReplayError> {
        let mut iter = events.into_iter();
        let first = iter
            .next()
            .ok_or(ReplayError::NotStartedWithQueued { actual: "<空>" })?;

        let RunEventBody::RunQueued {
            task_version_id,
            trigger,
            dry_run,
            ..
        } = &first.body
        else {
            return Err(ReplayError::NotStartedWithQueued {
                actual: first.kind(),
            });
        };
        if first.seq != 1 {
            return Err(ReplayError::SeqGap {
                expected: 1,
                actual: first.seq,
            });
        }

        let mut state = Self {
            run_id: first.run_id,
            task_version_id: *task_version_id,
            trigger: *trigger,
            dry_run: *dry_run,
            status: RunStatus::Queued,
            started_at: None,
            finished_at: None,
            cost: UsdMicros::ZERO,
            nodes: BTreeMap::new(),
            open_approvals: BTreeMap::new(),
            last_seq: 1,
            error: None,
            drift: None,
        };
        for event in iter {
            state.apply(event)?;
        }
        Ok(state)
    }

    /// 消费一条新事件。
    ///
    /// 幂等性由 seq 保证：重复投递同一条事件会被 [`ReplayError::SeqGap`] 挡下，
    /// 调用方据此知道自己拿到的是旧数据。
    pub fn apply(&mut self, event: &RunEvent) -> Result<(), ReplayError> {
        if event.run_id != self.run_id {
            return Err(ReplayError::WrongRun {
                expected: self.run_id,
                actual: event.run_id,
            });
        }
        if event.seq != self.last_seq + 1 {
            return Err(ReplayError::SeqGap {
                expected: self.last_seq + 1,
                actual: event.seq,
            });
        }
        if self.status.is_terminal() {
            return Err(ReplayError::EventAfterTerminal {
                status: self.status,
                seq: event.seq,
                kind: event.kind(),
            });
        }
        self.last_seq = event.seq;

        match &event.body {
            RunEventBody::RunQueued { .. } => {
                // seq 检查已经保证它只可能出现在 seq=1，走不到这里
            }
            RunEventBody::RunStarted { .. } => {
                self.status = RunStatus::Running;
                self.started_at = Some(event.ts);
            }
            RunEventBody::RunFinished {
                status,
                error,
                cost_usd,
            } => {
                self.status = *status;
                self.finished_at = Some(event.ts);
                self.error.clone_from(error);
                // 以终态事件里的成本为准：它是写入方的权威口径，
                // 逐条 usage 累加可能因为事件截断而偏低。
                self.cost = *cost_usd;
                // run 结束时还开着的审批已无意义
                self.open_approvals.clear();
            }

            RunEventBody::NodeReady => {
                self.node_mut(event).status = NodeStatus::Ready;
            }
            RunEventBody::NodeStarted { attempt, host_id } => {
                let ts = event.ts;
                let node = self.node_mut(event);
                node.status = NodeStatus::Running;
                node.attempt = *attempt;
                node.host_id = *host_id;
                node.started_at.get_or_insert(ts);
                // 重试时清掉上一次的失败痕迹，否则界面会同时显示错误和运行中
                node.error = None;
            }
            RunEventBody::NodeFinished {
                attempt,
                status,
                output,
                error,
            } => {
                let ts = event.ts;
                let node = self.node_mut(event);
                node.status = *status;
                node.attempt = *attempt;
                node.finished_at = Some(ts);
                node.output.clone_from(output);
                node.error.clone_from(error);
            }
            RunEventBody::NodeRetrying {
                attempt, reason, ..
            } => {
                let node = self.node_mut(event);
                node.status = NodeStatus::Ready;
                node.attempt = *attempt;
                node.error = Some(reason.clone());
            }
            RunEventBody::NodeSkipped { reason } => {
                let ts = event.ts;
                let node = self.node_mut(event);
                node.status = NodeStatus::Skipped;
                node.finished_at = Some(ts);
                node.error = Some(reason.clone());
            }
            RunEventBody::MapExpanded { count, .. } => {
                self.node_mut(event).map_expanded = Some(*count);
            }
            RunEventBody::DriftDetected {
                baseline_run_id,
                changed_paths,
            } => {
                self.drift = Some(DriftFlag {
                    baseline_run_id: *baseline_run_id,
                    changed_paths: changed_paths.clone(),
                });
            }
            RunEventBody::ResourceDegraded { mode, detail } => {
                let node = self.node_mut(event);
                node.resource_mode = Some(mode.clone());
                node.resource_degraded_reason.clone_from(detail);
            }

            RunEventBody::ToolRequested { .. } => {
                self.node_mut(event).tool_calls += 1;
            }
            RunEventBody::PolicyDecided { effect, .. } => {
                if *effect == PolicyEffect::Deny {
                    self.node_mut(event).denied_tool_calls += 1;
                }
            }
            RunEventBody::Usage { cost_usd, .. } => {
                self.cost = self.cost.saturating_add(*cost_usd);
                let node = self.node_mut(event);
                node.cost = node.cost.saturating_add(*cost_usd);
            }

            RunEventBody::ApprovalRequested {
                approval_id,
                title,
                intent,
                expires_at,
            } => {
                self.open_approvals.insert(
                    *approval_id,
                    OpenApproval {
                        node_key: event.node_key.clone(),
                        title: title.clone(),
                        intent: intent.clone(),
                        expires_at: *expires_at,
                    },
                );
                if event.node_key.is_some() {
                    self.node_mut(event).status = NodeStatus::AwaitingApproval;
                }
            }
            RunEventBody::ApprovalDecided { approval_id, .. } => {
                self.open_approvals.remove(approval_id);
            }

            // 纯展示类事件，不参与状态推导
            RunEventBody::AgentInvoked { .. }
            | RunEventBody::AgentTurnStarted { .. }
            | RunEventBody::AgentThinking { .. }
            | RunEventBody::AgentText { .. }
            | RunEventBody::ToolCompleted { .. }
            | RunEventBody::Log { .. } => {}
        }
        Ok(())
    }

    /// run 是否卡在人工审批上。用于界面提示和超时巡检。
    #[must_use]
    pub fn is_awaiting_human(&self) -> bool {
        !self.open_approvals.is_empty()
    }

    /// 还没到终态的节点。引擎重启后据此决定要重投哪些节点。
    #[must_use]
    pub fn unfinished_nodes(&self) -> Vec<&NodeKey> {
        self.nodes
            .iter()
            .filter(|(_, s)| !s.status.is_terminal())
            .map(|(k, _)| k)
            .collect()
    }

    /// 取（必要时创建）事件所属节点的状态。
    ///
    /// 事件没带 node_key 时落到一个哨兵 key 上。这只会发生在事件写入方有 bug 的
    /// 情况下，让它可见好过静默丢弃。
    fn node_mut(&mut self, event: &RunEvent) -> &mut NodeState {
        let key = event
            .node_key
            .clone()
            .unwrap_or_else(|| NodeKey("__run__".into()));
        self.nodes.entry(key).or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ai_task_proto::{LogLevel, RuleId};

    struct Log {
        run_id: RunId,
        seq: i64,
        events: Vec<RunEvent>,
    }

    impl Log {
        fn new() -> Self {
            let run_id = RunId::new();
            let mut log = Self {
                run_id,
                seq: 0,
                events: Vec::new(),
            };
            log.push(
                None,
                RunEventBody::RunQueued {
                    task_version_id: TaskVersionId::new(),
                    trigger: TriggerKind::Schedule,
                    inputs: None,
                    dry_run: false,
                },
            );
            log
        }

        fn push(&mut self, node: Option<&str>, body: RunEventBody) -> &mut Self {
            self.seq += 1;
            self.events.push(RunEvent {
                run_id: self.run_id,
                seq: self.seq,
                ts: Utc::now(),
                node_key: node.map(|n| NodeKey(n.into())),
                body,
            });
            self
        }

        fn replay(&self) -> Result<RunState, ReplayError> {
            RunState::replay(&self.events)
        }
    }

    fn finished(status: NodeStatus) -> RunEventBody {
        RunEventBody::NodeFinished {
            attempt: 1,
            status,
            output: Some(serde_json::json!({"ok": true})),
            error: None,
        }
    }

    #[test]
    fn happy_path_folds_to_succeeded() {
        let mut log = Log::new();
        log.push(None, RunEventBody::RunStarted { worker: "w".into() })
            .push(Some("a"), RunEventBody::NodeReady)
            .push(
                Some("a"),
                RunEventBody::NodeStarted {
                    attempt: 1,
                    host_id: None,
                },
            )
            .push(Some("a"), finished(NodeStatus::Succeeded))
            .push(
                None,
                RunEventBody::RunFinished {
                    status: RunStatus::Succeeded,
                    error: None,
                    cost_usd: UsdMicros(12_000),
                },
            );

        let s = log.replay().expect("重放");
        assert_eq!(s.status, RunStatus::Succeeded);
        assert_eq!(s.cost, UsdMicros(12_000));
        assert_eq!(s.nodes[&NodeKey("a".into())].status, NodeStatus::Succeeded);
        assert!(s.started_at.is_some() && s.finished_at.is_some());
        assert!(s.unfinished_nodes().is_empty());
    }

    #[test]
    fn replay_is_deterministic_and_incremental_apply_agrees() {
        // 赌注 1 的核心保证：整段重放 == 逐条 apply
        let mut log = Log::new();
        log.push(None, RunEventBody::RunStarted { worker: "w".into() })
            .push(Some("a"), RunEventBody::NodeReady)
            .push(
                Some("a"),
                RunEventBody::NodeStarted {
                    attempt: 1,
                    host_id: None,
                },
            )
            .push(
                Some("a"),
                RunEventBody::ToolRequested {
                    tool_use_id: "t1".into(),
                    tool: "remote_bash".into(),
                    input: serde_json::json!({}),
                },
            )
            .push(
                Some("a"),
                RunEventBody::PolicyDecided {
                    tool_use_id: "t1".into(),
                    effect: PolicyEffect::Deny,
                    rule_id: Some(RuleId::new()),
                    reason: "prod 禁止".into(),
                },
            )
            .push(
                Some("a"),
                RunEventBody::Usage {
                    model: "claude-opus-5".into(),
                    input_tokens: 100,
                    output_tokens: 10,
                    cache_read_tokens: 0,
                    cache_creation_tokens: 0,
                    cost_usd: UsdMicros(750),
                },
            )
            .push(Some("a"), finished(NodeStatus::Succeeded));

        let whole = log.replay().expect("整段重放");

        let mut incremental = RunState::replay(&log.events[..1]).expect("首条");
        for e in &log.events[1..] {
            incremental.apply(e).expect("逐条 apply");
        }
        assert_eq!(whole, incremental);

        let node = &whole.nodes[&NodeKey("a".into())];
        assert_eq!(node.tool_calls, 1);
        assert_eq!(node.denied_tool_calls, 1);
        assert_eq!(node.cost, UsdMicros(750));
        assert_eq!(whole.cost, UsdMicros(750));
    }

    #[test]
    fn retry_clears_the_previous_error_so_ui_is_not_contradictory() {
        let mut log = Log::new();
        log.push(None, RunEventBody::RunStarted { worker: "w".into() })
            .push(
                Some("a"),
                RunEventBody::NodeStarted {
                    attempt: 1,
                    host_id: None,
                },
            )
            .push(
                Some("a"),
                RunEventBody::NodeRetrying {
                    attempt: 1,
                    delay_ms: 1_000,
                    reason: "schema 校验失败".into(),
                    feeding_error_to_model: true,
                },
            );
        let s = log.replay().expect("重放");
        let node = &s.nodes[&NodeKey("a".into())];
        assert_eq!(node.status, NodeStatus::Ready);
        assert_eq!(node.error.as_deref(), Some("schema 校验失败"));

        log.push(
            Some("a"),
            RunEventBody::NodeStarted {
                attempt: 2,
                host_id: None,
            },
        );
        let s = log.replay().expect("重放");
        let node = &s.nodes[&NodeKey("a".into())];
        assert_eq!(node.status, NodeStatus::Running);
        assert_eq!(node.attempt, 2);
        assert_eq!(node.error, None, "重试开始后不应还挂着上一轮的错误");
    }

    #[test]
    fn approval_blocks_the_node_and_clears_on_decision() {
        let approval_id = ApprovalId::new();
        let mut log = Log::new();
        log.push(None, RunEventBody::RunStarted { worker: "w".into() })
            .push(
                Some("gate"),
                RunEventBody::ApprovalRequested {
                    approval_id,
                    title: "重启 nginx".into(),
                    intent: serde_json::json!({"command": "systemctl restart nginx"}),
                    expires_at: Utc::now(),
                },
            );
        let s = log.replay().expect("重放");
        assert!(s.is_awaiting_human());
        assert_eq!(
            s.nodes[&NodeKey("gate".into())].status,
            NodeStatus::AwaitingApproval
        );

        log.push(
            Some("gate"),
            RunEventBody::ApprovalDecided {
                approval_id,
                approved: false,
                decided_by: Some("zzh".into()),
                reason: None,
            },
        );
        let s = log.replay().expect("重放");
        assert!(!s.is_awaiting_human());
    }

    #[test]
    fn unfinished_nodes_drive_crash_recovery() {
        let mut log = Log::new();
        log.push(None, RunEventBody::RunStarted { worker: "w".into() })
            .push(Some("done"), finished(NodeStatus::Succeeded))
            .push(
                Some("inflight"),
                RunEventBody::NodeStarted {
                    attempt: 1,
                    host_id: None,
                },
            )
            .push(Some("waiting"), RunEventBody::NodeReady);
        let s = log.replay().expect("重放");
        assert_eq!(
            s.unfinished_nodes(),
            vec![&NodeKey("inflight".into()), &NodeKey("waiting".into())]
        );
    }

    #[test]
    fn rejects_a_seq_gap_instead_of_silently_diverging() {
        let mut log = Log::new();
        log.push(None, RunEventBody::RunStarted { worker: "w".into() });
        log.events[1].seq = 5;
        assert_eq!(
            log.replay(),
            Err(ReplayError::SeqGap {
                expected: 2,
                actual: 5
            })
        );
    }

    #[test]
    fn rejects_a_replayed_duplicate_event() {
        let mut log = Log::new();
        log.push(None, RunEventBody::RunStarted { worker: "w".into() });
        let mut s = log.replay().expect("重放");
        let dup = log.events[1].clone();
        assert_eq!(
            s.apply(&dup),
            Err(ReplayError::SeqGap {
                expected: 3,
                actual: 2
            })
        );
    }

    #[test]
    fn rejects_events_from_another_run() {
        let log = Log::new();
        let mut s = log.replay().expect("重放");
        let mut stray = log.events[0].clone();
        stray.run_id = RunId::new();
        stray.seq = 2;
        assert!(matches!(s.apply(&stray), Err(ReplayError::WrongRun { .. })));
    }

    #[test]
    fn rejects_events_after_a_terminal_status() {
        let mut log = Log::new();
        log.push(
            None,
            RunEventBody::RunFinished {
                status: RunStatus::Cancelled,
                error: None,
                cost_usd: UsdMicros::ZERO,
            },
        );
        let mut s = log.replay().expect("重放");
        let mut late = log.events[0].clone();
        late.seq = 3;
        late.body = RunEventBody::Log {
            level: LogLevel::Info,
            message: "迟到".into(),
        };
        assert!(matches!(
            s.apply(&late),
            Err(ReplayError::EventAfterTerminal { .. })
        ));
    }

    #[test]
    fn rejects_a_stream_that_does_not_start_with_run_queued() {
        let run_id = RunId::new();
        let events = vec![RunEvent {
            run_id,
            seq: 1,
            ts: Utc::now(),
            node_key: None,
            body: RunEventBody::RunStarted { worker: "w".into() },
        }];
        assert_eq!(
            RunState::replay(&events),
            Err(ReplayError::NotStartedWithQueued {
                actual: "run_started"
            })
        );
    }

    #[test]
    fn terminal_cost_wins_over_accumulated_usage() {
        // 事件被截断时 usage 累加会偏低，终态事件里的成本才是权威口径
        let mut log = Log::new();
        log.push(None, RunEventBody::RunStarted { worker: "w".into() })
            .push(
                Some("a"),
                RunEventBody::Usage {
                    model: "m".into(),
                    input_tokens: 1,
                    output_tokens: 1,
                    cache_read_tokens: 0,
                    cache_creation_tokens: 0,
                    cost_usd: UsdMicros(100),
                },
            )
            .push(
                None,
                RunEventBody::RunFinished {
                    status: RunStatus::Succeeded,
                    error: None,
                    cost_usd: UsdMicros(999),
                },
            );
        assert_eq!(log.replay().expect("重放").cost, UsdMicros(999));
    }
}
