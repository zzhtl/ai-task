//! 工具调用的策略判决。
//!
//! 调用方是 Claude Code 的 `PreToolUse` hook（经 `ai-task policy-hook` 转发）。
//! **不对外暴露**：工具调用在这个接口返回前是阻塞的，能调它就等于能替 run
//! 批准任意动作。
//!
//! 三条硬性要求：
//! 1. **失败即拒绝。** 判不出来就不许做，绝不放行。
//! 2. **每次判决都进事件日志。** 审计要能回答「第 37 次工具调用为什么被拒」。
//! 3. **不缓存规则。** 每次都从库里读——策略是活的护栏，改了就该立刻生效。
//!    一次工具调用背后是几秒的模型时间，多一次查询是噪音。

use ai_task_core::{DefaultPolicy, PolicySet, ToolCall};
use ai_task_proto::{PolicyEffect, PolicyRequest, PolicyResponse, RunEventBody};
use ai_task_store::PendingEvent;
use axum::extract::State;
use axum::http::HeaderMap;

use crate::error::AppError;
use crate::extract::Json;
use crate::state::AppState;

/// 令牌头。hook 用它证明自己是这个 run 的合法调用方。
pub const TOKEN_HEADER: &str = "x-ai-task-token";

/// `POST /internal/policy/decide`
pub async fn decide(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<PolicyRequest>,
) -> Result<Json<PolicyResponse>, AppError> {
    verify_token(&state, &headers)?;
    // hook 是外部子进程，让它自己轮询，不要把它挂在一个长请求上
    Ok(Json(evaluate(&state, &request, AskMode::Report).await?))
}

/// 校验内部令牌。所有内部接口的第一道。
pub fn verify_token(state: &AppState, headers: &HeaderMap) -> Result<(), AppError> {
    // 没有令牌就不判——同机上的任何进程都能打这个接口
    let presented = headers
        .get(TOKEN_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    if state.verify_internal_token(presented) {
        Ok(())
    } else {
        Err(AppError::Unauthorized("内部接口令牌不匹配"))
    }
}

/// 判一次工具调用，并把判决写进事件日志。
///
/// hook 和 MCP 远端代理共用这一个实现。两条路径的判决口径必须逐字一致，
/// 否则「同一条命令走 Bash 被拒、走 remote_bash 放行」这种洞会长出来。
/// `ask` 命中时怎么办。
///
/// 两条调用路径的约束不一样：`/internal/remote/call` 跑在服务端进程里，
/// 就地 await 不占任何 HTTP 连接；hook 是个外部子进程，让它挂在一个
/// 长请求上会逼着把它的超时放大到审批窗口那么长。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AskMode {
    /// 就地等到有结论。
    Wait,
    /// 建好审批就返回，让调用方自己轮询。
    Report,
}

pub async fn evaluate(
    state: &AppState,
    request: &PolicyRequest,
    ask_mode: AskMode,
) -> Result<PolicyResponse, AppError> {
    let run = state
        .store
        .get_run(state.workspace_id, request.run_id)
        .await
        .map_err(|_| AppError::NotFound(format!("run {} 不存在", request.run_id)))?;

    if run.status.is_terminal() {
        // run 都结束了还有工具调用进来：多半是上一次执行的残留进程。
        // 一律拒绝，不能让它继续动东西。
        return Ok(deny(
            "这次执行已经结束，残留的工具调用一律拒绝",
            None,
            false,
        ));
    }

    let version = state.store.get_task_version(run.task_version_id).await?;
    let rules = state
        .store
        .policy_rules_for(state.workspace_id, &version.rules)
        .await?;

    // 一条规则编译不了就整体拒绝——半装的策略集比没有策略更危险
    let policy = PolicySet::compile(rules, DefaultPolicy::GuardProduction).map_err(|err| {
        tracing::error!(error = %err, "策略集编译失败，本次调用按拒绝处理");
        AppError::Internal(anyhow::anyhow!(err))
    })?;

    // 目标主机的 tag。策略按 tag 生效（`prod` 机上写类操作一律 ask），
    // 所以取不到 tag 时**不能**当成"没有 tag"放行——那会让针对 prod 的
    // 规则在一次数据库抖动里失效。
    let host_tags: Vec<String> = match request.host_id {
        None => Vec::new(),
        Some(host_id) => match state.store.host_tags(state.workspace_id, host_id).await {
            Ok(Some(tags)) => tags,
            Ok(None) => {
                return Ok(deny(
                    &format!("目标主机 {host_id} 不存在，无法判定它的策略范围"),
                    None,
                    run.dry_run,
                ));
            }
            Err(err) => {
                tracing::error!(%host_id, error = %err, "读取主机 tag 失败，本次调用按拒绝处理");
                return Ok(deny(
                    "读不到目标主机的 tag，无法判定策略范围，按拒绝处理",
                    None,
                    run.dry_run,
                ));
            }
        },
    };
    let decision = policy.decide(&ToolCall {
        tool: normalize_tool(&request.tool),
        input: &request.input,
        host_tags: &host_tags,
        task_id: run.task_id,
    });

    // 影子执行：写类工具只记录意图，不真的做。
    // 放在策略判决**之后**，这样审计里能同时看到"策略本来允许"和"因为影子执行被拦"。
    let effect = if run.dry_run && ai_task_core::policy::is_write_tool(&request.tool) {
        PolicyEffect::Deny
    } else {
        decision.effect
    };
    let reason = if effect != decision.effect {
        format!(
            "影子执行：`{}` 是写类工具，只记录意图不执行（策略本身是 {:?}）",
            request.tool, decision.effect
        )
    } else if decision.reason.is_empty() {
        "策略放行".to_string()
    } else {
        decision.reason.clone()
    };

    // 判决先落库，再去等人。否则"这次调用为什么卡住了"在事件流里是看不到的
    // ——界面上只会看到一个不动的节点。
    state
        .store
        .append_events(
            request.run_id,
            &[PendingEvent::new(
                Some(request.node_key.clone()),
                RunEventBody::PolicyDecided {
                    tool_use_id: request.tool_use_id.clone(),
                    effect,
                    rule_id: decision.rule_id,
                    reason: reason.clone(),
                },
            )],
        )
        .await?;

    // `ask` 把这次工具调用挂起等人点头。**带上限地等**：超时后回一个
    // 结构化的 denied 作为 tool_result，让模型自己调整，而不是把调用挂死
    // ——挂死的表现是 run 永远停在 running，谁也看不出在等什么。
    let mut pending_approval_id = None;
    let (allowed, reason) = match effect {
        PolicyEffect::Allow => (true, reason),
        PolicyEffect::Deny => (false, reason),
        PolicyEffect::Ask => {
            let approval_id = open_approval(state, request, &reason, decision.rule_id).await?;
            match ask_mode {
                AskMode::Wait => {
                    let verdict =
                        wait_for(state, request.run_id, &request.node_key, approval_id).await?;
                    (verdict.is_approved(), verdict.describe())
                }
                AskMode::Report => {
                    pending_approval_id = Some(approval_id);
                    (false, format!("{reason}（等待人工确认）"))
                }
            }
        }
    };

    tracing::info!(
        run_id = %request.run_id, tool = %request.tool, %allowed,
        rule = ?decision.rule_id, "策略判决"
    );

    Ok(PolicyResponse {
        allowed,
        reason,
        rule_id: decision.rule_id.map(|id| id.to_string()),
        dry_run: run.dry_run,
        pending_approval_id,
    })
}

/// 建一条待决审批。卡片上要有具体参数：审批人得看见"要跑的到底是哪条命令"。
async fn open_approval(
    state: &AppState,
    request: &PolicyRequest,
    reason: &str,
    rule_id: Option<ai_task_proto::RuleId>,
) -> Result<ai_task_proto::ApprovalId, AppError> {
    let expires_at = chrono::Utc::now() + chrono::Duration::seconds(ASK_TIMEOUT_S);
    let approval_id = state
        .store
        .create_approval(ai_task_store::NewApproval {
            run_id: request.run_id,
            node_key: ai_task_proto::NodeKey::parse(request.node_key.clone()).ok(),
            title: format!("{} 需要确认", request.tool),
            intent: serde_json::json!({
                "tool": request.tool,
                "input": request.input,
                "host_id": request.host_id,
                "policy_reason": reason,
            }),
            rule_id,
            expires_at,
        })
        .await?;

    state
        .store
        .append_events(
            request.run_id,
            &[PendingEvent::new(
                Some(request.node_key.clone()),
                RunEventBody::ApprovalRequested {
                    approval_id,
                    title: format!("{} 需要确认", request.tool),
                    intent: serde_json::json!({
                        "tool": request.tool,
                        "input": request.input,
                        "host_id": request.host_id,
                        "policy_reason": reason,
                    }),
                    expires_at,
                },
            )],
        )
        .await?;
    Ok(approval_id)
}

/// 等到有结论，然后把结论写进事件日志。
///
/// 结论和请求落在**同一个节点**下：以前写成 run 级事件，过程视图按节点分组时
/// 找不到它，那张审批卡就永远停在"等人点头"。
async fn wait_for(
    state: &AppState,
    run_id: ai_task_proto::RunId,
    node_key: &str,
    approval_id: ai_task_proto::ApprovalId,
) -> Result<ai_task_runtime::Verdict, AppError> {
    let verdict = ai_task_runtime::approval::wait_for_verdict(
        &state.store,
        approval_id,
        &tokio_util::sync::CancellationToken::new(),
    )
    .await
    .map_err(AppError::Store)?;

    let decided_by = match &verdict {
        ai_task_runtime::Verdict::Approved { by, .. }
        | ai_task_runtime::Verdict::Denied { by, .. } => by.clone(),
        _ => None,
    };
    state
        .store
        .append_events(
            run_id,
            &[PendingEvent::new(
                Some(node_key.to_owned()),
                RunEventBody::ApprovalDecided {
                    approval_id,
                    approved: verdict.is_approved(),
                    decided_by,
                    reason: Some(verdict.describe()),
                },
            )],
        )
        .await?;
    Ok(verdict)
}

/// `POST /internal/policy/await`
///
/// 有界长轮询：最多等 [`AWAIT_SLICE_S`] 秒就返回，让调用方决定要不要再问一次。
/// 调用方（hook）持有整体截止时间，服务端只负责把单次等待切短。
pub async fn await_approval(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<ai_task_proto::AwaitApprovalRequest>,
) -> Result<Json<ai_task_proto::AwaitApprovalResponse>, AppError> {
    verify_token(&state, &headers)?;

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(AWAIT_SLICE_S);
    loop {
        if let Some((approved, reason, by)) =
            state.store.approval_verdict(request.approval_id).await?
        {
            // 结论进事件日志。**只在这里写一次**：`decide_approval` 的
            // `WHERE decided_at IS NULL` 保证了结论只会产生一次。
            let detail = reason.unwrap_or_else(|| {
                if approved {
                    "已批准".to_owned()
                } else {
                    "已拒绝".to_owned()
                }
            });
            let decided_by = match by {
                Some(user) => state.store.user_display_name(user).await?,
                None => None,
            };
            // 和 ApprovalRequested 落在同一个节点下，过程视图才找得到它
            let node_key = state
                .store
                .get_approval(state.workspace_id, request.approval_id)
                .await?
                .and_then(|a| a.node_key);
            state
                .store
                .append_events(
                    request.run_id,
                    &[PendingEvent::new(
                        node_key,
                        RunEventBody::ApprovalDecided {
                            approval_id: request.approval_id,
                            approved,
                            decided_by,
                            reason: Some(detail.clone()),
                        },
                    )],
                )
                .await?;
            return Ok(Json(ai_task_proto::AwaitApprovalResponse {
                pending: false,
                approved,
                reason: detail,
            }));
        }

        // 过期的一律按拒绝落库，界面上那张卡片才不会永远停在"待审批"
        if !state
            .store
            .expire_approvals(chrono::Utc::now())
            .await?
            .is_empty()
        {
            continue;
        }
        if tokio::time::Instant::now() >= deadline {
            return Ok(Json(ai_task_proto::AwaitApprovalResponse {
                pending: true,
                approved: false,
                reason: "等待人工确认".to_owned(),
            }));
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
}

/// 策略挂起的工具调用最多等多久。
///
/// 比 `approval` 节点的默认值短：那边人是"被安排来审批的"，
/// 这边是模型临时撞到一条 ask 规则，卡太久整个 run 的时间预算就没了。
const ASK_TIMEOUT_S: i64 = 600;

/// 单次长轮询的上限。要明显小于调用方的 HTTP 超时。
const AWAIT_SLICE_S: u64 = 10;

/// 把 MCP 工具名还原成规则里写的名字。
///
/// Claude Code 把 MCP 工具报成 `mcp__<server>__<tool>`，而远端代理自己上报的是
/// `remote_write`。不归一的话，**同一个动作在两道关卡上匹配的是不同的规则**：
/// 写着 `tool: "remote_write"` 的策略在 hook 那道完全不命中，审计日志里就会
///出现「先 allow 后 deny」这种自相矛盾的记录。真正危险的是反过来的情形——
/// 有人以为 hook 会拦住，于是没在别处设防。
fn normalize_tool(tool: &str) -> &str {
    tool.strip_prefix(&format!("mcp__{}__", ai_task_exec::MCP_SERVER_NAME))
        .map_or(tool, |rest| {
            // strip_prefix 借的是 tool，返回的切片和它同生命周期
            &tool[tool.len() - rest.len()..]
        })
}

fn deny(reason: &str, rule_id: Option<String>, dry_run: bool) -> PolicyResponse {
    PolicyResponse {
        allowed: false,
        reason: reason.to_string(),
        rule_id,
        dry_run,
        pending_approval_id: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_gates_match_the_same_rule_for_one_action() {
        // hook 看到的是 mcp__ai_task_remote__remote_write，
        // 代理上报的是 remote_write。规则只写一次，两边都要命中。
        assert_eq!(
            normalize_tool("mcp__ai_task_remote__remote_write"),
            "remote_write"
        );
        assert_eq!(normalize_tool("remote_write"), "remote_write");
    }

    #[test]
    fn only_our_own_server_prefix_is_stripped() {
        // 别人的 MCP server 同名工具不能借我们的规则名混过去
        assert_eq!(
            normalize_tool("mcp__somebody_else__remote_write"),
            "mcp__somebody_else__remote_write"
        );
        assert_eq!(normalize_tool("Bash"), "Bash");
        assert_eq!(normalize_tool(""), "");
    }
}
