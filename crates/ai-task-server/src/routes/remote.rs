//! 远端动作的唯一入口。
//!
//! MCP 代理进程（`ai-task mcp-proxy`）把模型的每一次远端工具调用打到这里。
//! 顺序是固定的：**先判策略，再执行**，两步都进事件日志。
//!
//! 为什么代理不自己连 SSH：那样每个代理进程都要拿到主机私钥，而代理是
//! Claude Code 拉起来的子进程，它的参数和环境对模型是可见的。凭据留在中心，
//! 代理手里只有一个 run 级别的令牌——它能做的事就只有"请中心代为执行"。
//!
//! 目标主机**由中心从任务定义里取**，不接受调用方指定。否则模型只要换一个
//! host_id 就能绕开所有按 tag 生效的策略。

use ai_task_proto::{
    HostSelector, PolicyRequest, RemoteAction, RemoteCallRequest, RemoteCallResponse, RunEventBody,
};
use ai_task_runtime::{Command, HostExecOutcome};
use ai_task_store::PendingEvent;
use axum::extract::State;
use axum::http::HeaderMap;

use crate::error::AppError;
use crate::extract::Json;
use crate::routes::policy;
use crate::state::AppState;

/// 单次远端动作的输出上限。超出会截断并在正文里说明。
const MAX_OUTPUT_CHARS: usize = 32 * 1024;

/// `POST /internal/remote/call`
pub async fn call(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<RemoteCallRequest>,
) -> Result<Json<RemoteCallResponse>, AppError> {
    policy::verify_token(&state, &headers)?;

    let run = state
        .store
        .get_run(state.workspace_id, request.run_id)
        .await
        .map_err(|_| AppError::NotFound(format!("run {} 不存在", request.run_id)))?;
    let version = state.store.get_task_version(run.task_version_id).await?;

    // 从任务定义里取这个节点声明的主机。节点没声明就等于不允许远端动作
    // ——不能默认落到中心本机上，那是把远端操作悄悄变成本地操作。
    let selector = version
        .spec
        .nodes
        .iter()
        .find(|n| n.key.as_str() == request.node_key)
        .and_then(|n| n.host.clone());
    let host_id = match &selector {
        Some(HostSelector::Host { host_id }) => Some(*host_id),
        Some(HostSelector::Tag { tag }) => {
            return Ok(Json(refused(format!(
                "节点 `{}` 按 tag `{tag}` 选主机，这条路径还没实现",
                request.node_key
            ))));
        }
        Some(HostSelector::Local) | None => {
            return Ok(Json(refused(format!(
                "节点 `{}` 没有声明目标主机，不能执行远端动作。\
                 在任务定义里给它加 host 才能用 remote_* 工具",
                request.node_key
            ))));
        }
    };

    // 第一步：策略。判不出来就是拒绝。
    let verdict = policy::evaluate(
        &state,
        &PolicyRequest {
            run_id: request.run_id,
            node_key: request.node_key.clone(),
            tool_use_id: request.tool_use_id.clone(),
            tool: request.action.tool().to_owned(),
            input: serde_json::to_value(&request.action).unwrap_or(serde_json::Value::Null),
            host_id,
        },
        // 这条路径跑在服务端进程里，就地等不占任何 HTTP 连接
        policy::AskMode::Wait,
    )
    .await?;

    if !verdict.allowed {
        return Ok(Json(RemoteCallResponse {
            executed: false,
            content: format!("被策略拒绝：{}", verdict.reason),
            exit_code: None,
            rule_id: verdict.rule_id,
        }));
    }

    // 第二步：执行。
    let Some(host_exec) = state.host_exec.as_ref() else {
        return Ok(Json(refused(
            "这个部署没有配置 agent 二进制，远端动作不可用".to_owned(),
        )));
    };

    let (command, timeout_s) = match &request.action {
        RemoteAction::RemoteBash {
            command,
            working_dir,
            timeout_s,
        } => (
            match working_dir {
                // 目录名交给 shell，必须引起来。命令本身是模型有意提供的，
                // 由策略层把关；工作目录只是个路径，没有理由让它参与解析。
                Some(dir) => format!("cd {} && {command}", shell_quote(dir)),
                None => command.clone(),
            },
            timeout_s.unwrap_or(300),
        ),
        RemoteAction::RemoteRead { path } => (format!("cat -- {}", shell_quote(path)), 60),
        RemoteAction::RemoteWrite { path, content } => (
            // 内容走 stdin heredoc，不进命令行：几十 KB 会撑爆 ARG_MAX，
            // 而且任何引号处理都可能被内容本身骗过去。
            format!(
                "cat > {} <<'AI_TASK_EOF_{tag}'\n{content}\nAI_TASK_EOF_{tag}",
                shell_quote(path),
                tag = heredoc_tag(content),
            ),
            120,
        ),
        RemoteAction::RemoteGlob { pattern } => (
            format!(
                "ls -d -- {} 2>/dev/null | head -n 500",
                shell_quote(pattern)
            ),
            60,
        ),
    };

    let started = std::time::Instant::now();
    let outcome = ai_task_runtime::run_command(
        &state.store,
        host_exec,
        Command {
            workspace_id: state.workspace_id,
            run_id: request.run_id,
            node_key: &request.node_key,
            selector: selector.as_ref(),
            command: &command,
            cwd: ".",
            timeout_ms: u64::from(timeout_s).saturating_mul(1000),
            limits: None,
            roots: &[],
        },
    )
    .await;

    let response = match outcome {
        Ok(outcome) => {
            let elapsed = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
            record(&state, &request, &outcome, elapsed).await;
            to_response(&request.action, &outcome)
        }
        // 连不上目标机是模型该知道的事实，不是它的调用出错了。
        // 作为 tool_result 回给它，让它自己决定重试还是换路子。
        Err(err) => RemoteCallResponse {
            executed: false,
            content: format!("目标机执行失败：{err}"),
            exit_code: None,
            rule_id: None,
        },
    };

    Ok(Json(response))
}

async fn record(
    state: &AppState,
    request: &RemoteCallRequest,
    outcome: &HostExecOutcome,
    duration_ms: u64,
) {
    let mut events = vec![PendingEvent::new(
        Some(request.node_key.clone()),
        RunEventBody::ToolCompleted {
            tool_use_id: request.tool_use_id.clone(),
            ok: outcome.result.exit_code == Some(0),
            output_preview: preview(&outcome.stdout, &outcome.stderr),
            duration_ms,
        },
    )];
    if outcome.cgroup_mode != ai_task_agent::protocol::CgroupMode::Systemd {
        events.push(PendingEvent::new(
            Some(request.node_key.clone()),
            RunEventBody::ResourceDegraded {
                mode: format!("{:?}", outcome.cgroup_mode).to_lowercase(),
                detail: outcome.cgroup_detail.clone(),
            },
        ));
    }
    if let Err(err) = state.store.append_events(request.run_id, &events).await {
        tracing::warn!(run_id = %request.run_id, error = %err, "远端动作的事件写入失败");
    }
}

fn to_response(action: &RemoteAction, outcome: &HostExecOutcome) -> RemoteCallResponse {
    let exec = &outcome.result;
    if exec.oom_killed {
        return RemoteCallResponse {
            executed: true,
            content: "命中内存上限，整个进程组被内核终止。换个占用更小的做法。".to_owned(),
            exit_code: None,
            rule_id: None,
        };
    }
    if exec.timed_out {
        return RemoteCallResponse {
            executed: true,
            content: "命令超时，已连同其子进程一并终止。".to_owned(),
            exit_code: None,
            rule_id: None,
        };
    }

    let body = match (exec.exit_code, action) {
        // 写文件成功时 stdout 是空的，回一句确认比回空字符串有用
        (Some(0), RemoteAction::RemoteWrite { path, content }) => {
            format!("已写入 {path}（{} 字节）", content.len())
        }
        (Some(0), _) => {
            let out = outcome.stdout.trim();
            if out.is_empty() {
                "（无输出）".to_owned()
            } else {
                truncate(out)
            }
        }
        (Some(code), _) => format!(
            "退出码 {code}\n{}",
            truncate(if outcome.stderr.trim().is_empty() {
                outcome.stdout.trim()
            } else {
                outcome.stderr.trim()
            })
        ),
        (None, _) => format!("被信号 {} 终止", exec.killed_by_signal.unwrap_or(0)),
    };

    RemoteCallResponse {
        executed: true,
        content: body,
        exit_code: exec.exit_code,
        rule_id: None,
    }
}

/// 中心侧的拒绝（不是策略拒绝）。
fn refused(reason: String) -> RemoteCallResponse {
    RemoteCallResponse {
        executed: false,
        content: reason,
        exit_code: None,
        rule_id: None,
    }
}

fn truncate(text: &str) -> String {
    if text.chars().count() <= MAX_OUTPUT_CHARS {
        return text.to_owned();
    }
    let head: String = text.chars().take(MAX_OUTPUT_CHARS).collect();
    format!("{head}\n…（输出已截断，共 {} 字符）", text.chars().count())
}

fn preview(stdout: &str, stderr: &str) -> String {
    let source = if stdout.trim().is_empty() {
        stderr
    } else {
        stdout
    };
    source.trim().chars().take(200).collect()
}

/// 单引号包裹。POSIX shell 里单引号内没有任何转义，唯一要处理的是
/// 单引号本身：闭合、插一个转义的单引号、再开一个。
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

/// 选一个不会在内容里出现的 heredoc 结束标记。
fn heredoc_tag(content: &str) -> String {
    let mut n = 0u32;
    loop {
        let tag = format!("{n}");
        if !content.contains(&format!("AI_TASK_EOF_{tag}")) {
            return tag;
        }
        n += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_cannot_break_out_of_its_quoting() {
        // 路径是模型给的字符串。'; rm -rf / #' 这种必须原样变成一个路径，
        // 而不是一条新命令。
        for hostile in [
            "/tmp/a b",
            "'; rm -rf / #",
            "$(whoami)",
            "`id`",
            "a'b'c",
            "\\'; touch /tmp/pwned; '",
        ] {
            let quoted = shell_quote(hostile);
            assert!(quoted.starts_with('\'') && quoted.ends_with('\''));
            // 单引号只以 '\'' 这个组合出现，不会有落单的闭合引号
            let inner = &quoted[1..quoted.len() - 1];
            assert!(
                !inner.contains('\'') || inner.contains(r"'\''"),
                "{hostile} 的引号处理不安全：{quoted}"
            );
        }
    }

    #[test]
    fn shell_quoting_survives_a_real_shell() {
        // 断言引法的语义，而不是它的字面量：改写法但语义不变时测试不该红
        for value in ["/tmp/a b", "'; id #", "$(id)", "a'b", "多字节 路径"] {
            let out = std::process::Command::new("sh")
                .arg("-c")
                .arg(format!("printf %s {}", shell_quote(value)))
                .output()
                .expect("跑 sh");
            assert_eq!(
                String::from_utf8_lossy(&out.stdout),
                value,
                "{value} 经过 shell 之后变了样"
            );
        }
    }

    #[test]
    fn a_heredoc_tag_never_collides_with_the_content() {
        // 内容里正好带着结束标记的话，文件会被截断在那一行
        let content = "line\nAI_TASK_EOF_0\nmore";
        let tag = heredoc_tag(content);
        assert_ne!(tag, "0");
        assert!(!content.contains(&format!("AI_TASK_EOF_{tag}")));
    }

    #[test]
    fn the_tool_names_match_the_serde_tags() {
        // 策略匹配器按工具名匹配。对不上就等于规则永远不命中。
        for action in [
            RemoteAction::RemoteBash {
                command: "ls".into(),
                working_dir: None,
                timeout_s: None,
            },
            RemoteAction::RemoteRead { path: "/x".into() },
            RemoteAction::RemoteWrite {
                path: "/x".into(),
                content: String::new(),
            },
            RemoteAction::RemoteGlob {
                pattern: "*".into(),
            },
        ] {
            let json = serde_json::to_value(&action).expect("序列化");
            assert_eq!(json["tool"].as_str(), Some(action.tool()));
        }
    }
}
