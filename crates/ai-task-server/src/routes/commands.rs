//! 临时批量执行：在几台机器上各跑一次同一条命令，不用先建任务。
//!
//! 落成一个 run：挂在每个 workspace 一个的系统任务（kind = 'adhoc'）下，每次执行给它插一个
//! 新版本——那一版就是"这次跑了什么、在哪些机器上"。所以它出现在执行记录里，有每台机器的
//! 结果、资源采样和审计，和任务里的多主机 shell 步骤走同一条执行路径。
//!
//! **要过策略。**任务里的 shell 步骤不过策略：那是保存下来、有版本、有审计的定义。
//! 临时命令是当场敲的，按 `remote_bash` 加目标机的 tag 判一遍——护栏对手滑同样有效：
//! 有一台被拒就整条不执行，有需要人工确认的必须显式确认。

use std::collections::{BTreeSet, HashMap};

use ai_task_core::ValidatedDag;
use ai_task_proto::{
    CheckCommand, CommandCheck, DagSpec, FieldError, HostId, HostSelector, HostVerdict,
    MAX_FANOUT_HOSTS, NodeConfig, NodeKey, NodeSpec, PolicyEffect, RetryPolicy, RuleId, RunCommand,
    RunEventBody, ShellNode, TaskId, TriggerKind,
};
use ai_task_store::idempotency::{IdempotentCreate, IdempotentRun};
use ai_task_store::{NewRun, PendingEvent};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};

use crate::error::AppError;
use crate::extract::Json;
use crate::idempotency::IdempotentJson;
use crate::routes::policy::judge;
use crate::routes::runs::to_summary;
use crate::state::AppState;

/// 命令最长多少字节。和执行日志里记命令的截断口径一致。
const MAX_COMMAND_BYTES: usize = 16 * 1024;
const DEFAULT_TIMEOUT_S: u32 = 300;
const MAX_TIMEOUT_S: u32 = 3600;

/// 解析出来的一台目标机：id、名字、tag。
type Target = (HostId, String, Vec<String>);

/// `POST /api/v1/commands/check` —— 这条命令在每台机器上会被策略怎么判。不执行、不写任何东西。
pub async fn check(
    State(state): State<AppState>,
    Json(body): Json<CheckCommand>,
) -> Result<Json<CommandCheck>, AppError> {
    validate_input(&body.command, None, &body.targets)?;
    let targets = resolve(&state, &body.targets).await?;
    Ok(Json(assess(&state, body.command.trim(), &targets).await?))
}

/// `POST /api/v1/commands` —— 在几台机器上各跑一次，返回 202 和新建的 run。
pub async fn run(
    State(state): State<AppState>,
    request: IdempotentJson<RunCommand>,
) -> Result<Response, AppError> {
    let IdempotentJson { body, key, hash } = request;
    validate_input(&body.command, body.timeout_s, &body.targets)?;
    let command = body.command.trim();
    let targets = resolve(&state, &body.targets).await?;

    // 和 check 同一套判法，这里再判一次：check 之后可能有人改了规则
    let verdicts = assess(&state, command, &targets).await?;
    if verdicts.denied > 0 {
        return Err(AppError::Conflict(format!(
            "策略不允许在这些机器上执行：{}",
            name_hosts(&verdicts, PolicyEffect::Deny)
        )));
    }
    if verdicts.needs_confirmation > 0 && !body.confirm {
        return Err(AppError::Conflict(format!(
            "有 {} 台机器需要人工确认：{}。确认之后再执行",
            verdicts.needs_confirmation,
            name_hosts(&verdicts, PolicyEffect::Ask)
        )));
    }

    // 执行的就是判过策略的这几台：按 tag 选的也在这里固定成具体主机，
    // 免得判完之后新加进这个 tag 的机器没过策略就被带上
    let host_ids: Vec<HostId> = targets.iter().map(|(id, _, _)| *id).collect();
    let timeout_s = body.timeout_s.unwrap_or(DEFAULT_TIMEOUT_S);
    let spec = adhoc_spec(command, host_ids, timeout_s);
    ValidatedDag::validate(spec.clone()).map_err(|errors| {
        AppError::Internal(anyhow::anyhow!(
            "临时命令拼出来的编排不合法：{}",
            errors
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("；")
        ))
    })?;

    // 先记下这一版再建 run。幂等键重放时这一版用不上，留着也只是一条没人引用的历史
    let (task_id, version_id) = state
        .store
        .record_adhoc_command(state.workspace_id, &spec)
        .await?;
    let outcome = state
        .store
        .create_run_idempotent(
            IdempotentCreate {
                workspace_id: state.workspace_id,
                key: key.as_deref(),
                request_hash: &hash,
                status: i32::from(StatusCode::ACCEPTED.as_u16()),
            },
            NewRun {
                workspace_id: state.workspace_id,
                task_id,
                task_version_id: version_id,
                trigger: TriggerKind::Manual,
                dry_run: false,
                inputs: None,
                compare_to: None,
                created_by: crate::middleware::rbac::current_user(),
            },
            PendingEvent::run(RunEventBody::RunQueued {
                task_version_id: version_id,
                trigger: TriggerKind::Manual,
                inputs: None,
                dry_run: false,
            }),
            |run| serde_json::to_value(to_summary(run)).unwrap_or(serde_json::Value::Null),
        )
        .await?;
    let run = match outcome {
        IdempotentRun::Created(run) => *run,
        IdempotentRun::Replayed { status, body } => {
            return Ok(crate::idempotency::replay(status, body));
        }
        IdempotentRun::Conflict => {
            return Err(AppError::Conflict(
                "同一个 Idempotency-Key 配了不同的请求体".into(),
            ));
        }
        IdempotentRun::InFlight => {
            return Err(AppError::Conflict(
                "同一个 Idempotency-Key 的上一次请求还在处理中".into(),
            ));
        }
    };

    // 当场敲的命令审计里要看得见原文、落到了哪些机器、哪些是确认过才跑的
    state
        .audit(
            "command.run",
            "run",
            run.id.to_string(),
            None,
            Some(serde_json::json!({
                "command": command,
                "targets": body.targets,
                "hosts": targets.iter().map(|(_, name, _)| name).collect::<Vec<_>>(),
                "confirmed": verdicts
                    .hosts
                    .iter()
                    .filter(|h| h.effect == PolicyEffect::Ask)
                    .map(|h| &h.host_name)
                    .collect::<Vec<_>>(),
            })),
        )
        .await;

    state.supervisor.spawn(state.workspace_id, run.id);

    let mut headers = HeaderMap::new();
    if let Ok(location) = format!("/api/v1/runs/{}", run.id).parse() {
        headers.insert(axum::http::header::LOCATION, location);
    }
    Ok((StatusCode::ACCEPTED, headers, Json(to_summary(&run))).into_response())
}

/// 不查库就能判的那部分，一次把问题报全。
fn validate_input(
    command: &str,
    timeout_s: Option<u32>,
    targets: &HostSelector,
) -> Result<(), AppError> {
    let mut errors = Vec::new();
    let command = command.trim();
    if command.is_empty() {
        errors.push(field("command", "命令不能为空"));
    } else if command.len() > MAX_COMMAND_BYTES {
        errors.push(field(
            "command",
            &format!("命令最长 {} KiB", MAX_COMMAND_BYTES / 1024),
        ));
    }
    if let Some(timeout) = timeout_s
        && !(1..=MAX_TIMEOUT_S).contains(&timeout)
    {
        errors.push(field(
            "timeout_s",
            &format!("超时在 1 到 {MAX_TIMEOUT_S} 秒之间"),
        ));
    }
    match targets {
        HostSelector::Local => errors.push(field(
            "targets",
            "临时命令只下发到远端主机；要在中心本机上跑，建一个任务",
        )),
        HostSelector::Hosts { host_ids } => {
            if host_ids.is_empty() {
                errors.push(field("targets", "至少选一台主机"));
            } else if host_ids.len() > MAX_FANOUT_HOSTS {
                errors.push(field("targets", &format!("一次最多 {MAX_FANOUT_HOSTS} 台")));
            } else if host_ids.iter().collect::<BTreeSet<_>>().len() != host_ids.len() {
                errors.push(field("targets", "同一台主机选了不止一次"));
            }
        }
        HostSelector::Tag { tag } if tag.trim().is_empty() => {
            errors.push(field("targets", "tag 不能为空"));
        }
        HostSelector::Host { .. } | HostSelector::Tag { .. } => {}
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(AppError::Validation(errors))
    }
}

/// 落到哪几台机器上，连同它们的 tag（判策略要用）。
///
/// 不在这个 workspace 里的主机和不存在的一样处理：报在字段上，不说别处有没有。
async fn resolve(state: &AppState, targets: &HostSelector) -> Result<Vec<Target>, AppError> {
    let ids: Vec<HostId> = match targets {
        HostSelector::Host { host_id } => vec![*host_id],
        HostSelector::Hosts { host_ids } => host_ids.clone(),
        HostSelector::Tag { tag } => {
            let tag = tag.trim();
            let hosts = state.store.hosts_with_tag(state.workspace_id, tag).await?;
            if hosts.is_empty() {
                return Err(AppError::Validation(vec![field(
                    "targets",
                    &format!("tag `{tag}` 下没有主机"),
                )]));
            }
            if hosts.len() > MAX_FANOUT_HOSTS {
                return Err(AppError::Validation(vec![field(
                    "targets",
                    &format!(
                        "tag `{tag}` 下有 {} 台主机，一次最多 {MAX_FANOUT_HOSTS} 台",
                        hosts.len()
                    ),
                )]));
            }
            hosts.into_iter().map(|(id, _)| id).collect()
        }
        HostSelector::Local => {
            return Err(AppError::Validation(vec![field("targets", "要选主机")]));
        }
    };
    let briefs = state.store.host_briefs(state.workspace_id, &ids).await?;
    if briefs.len() != ids.len() {
        return Err(AppError::Validation(vec![field(
            "targets",
            &format!("有 {} 台主机不存在或已删除", ids.len() - briefs.len()),
        )]));
    }
    Ok(briefs)
}

/// 每台机器上的判决。和 AI 在这台机器上调 `remote_bash` 走同一个判决函数、同一份规则。
async fn assess(
    state: &AppState,
    command: &str,
    targets: &[Target],
) -> Result<CommandCheck, AppError> {
    let rules = state
        .store
        .policy_rules_for(state.workspace_id, &[])
        .await?;
    let names: HashMap<RuleId, String> = rules.iter().map(|r| (r.id, r.name.clone())).collect();
    // 不属于任何任务：只对特定任务生效的规则不会命中
    let task_id = TaskId::new();
    let input = serde_json::json!({ "command": command });
    let hosts: Vec<HostVerdict> = targets
        .iter()
        .map(|(host_id, name, tags)| {
            match judge(rules.clone(), "remote_bash", &input, tags, task_id, false) {
                Ok(judgement) => HostVerdict {
                    host_id: *host_id,
                    host_name: name.clone(),
                    effect: judgement.effect,
                    reason: judgement.reason,
                    rule_name: judgement.rule_id.and_then(|id| names.get(&id).cloned()),
                },
                // AI 的工具调用遇到这种情况是整体拒绝；临时命令也一样
                Err(err) => HostVerdict {
                    host_id: *host_id,
                    host_name: name.clone(),
                    effect: PolicyEffect::Deny,
                    reason: format!("策略集装不起来：{err}"),
                    rule_name: None,
                },
            }
        })
        .collect();
    let count = |effect| {
        u32::try_from(hosts.iter().filter(|h| h.effect == effect).count()).unwrap_or(u32::MAX)
    };
    Ok(CommandCheck {
        denied: count(PolicyEffect::Deny),
        needs_confirmation: count(PolicyEffect::Ask),
        hosts,
    })
}

/// 点名前三台和原因，给错误消息用。
fn name_hosts(check: &CommandCheck, effect: PolicyEffect) -> String {
    let matching: Vec<&HostVerdict> = check.hosts.iter().filter(|h| h.effect == effect).collect();
    let mut named: Vec<String> = matching
        .iter()
        .take(3)
        .map(|h| format!("{}（{}）", h.host_name, h.reason))
        .collect();
    if matching.len() > 3 {
        named.push(format!("等 {} 台", matching.len()));
    }
    named.join("、")
}

/// 一条命令、固定的一组主机：一个 shell 步骤的编排。不重试——当场敲的命令失败了由人决定下一步。
fn adhoc_spec(command: &str, host_ids: Vec<HostId>, timeout_s: u32) -> DagSpec {
    DagSpec {
        nodes: vec![NodeSpec {
            key: NodeKey::parse("command").expect("静态 key 合法"),
            name: Some("临时命令".into()),
            config: NodeConfig::Shell(ShellNode {
                command: command.to_owned(),
                working_dir: None,
            }),
            inputs: Default::default(),
            output_schema: None,
            retry: RetryPolicy::default(),
            on_failure: Default::default(),
            timeout_s: Some(timeout_s),
            host: Some(HostSelector::Hosts { host_ids }),
            limits: None,
        }],
        edges: vec![],
        input_schema: None,
        budget_usd: None,
        timeout_s: None,
    }
}

fn field(field: &str, message: &str) -> FieldError {
    FieldError {
        field: field.into(),
        code: "invalid".into(),
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields(err: AppError) -> Vec<String> {
        match err {
            AppError::Validation(errors) => errors.into_iter().map(|e| e.field).collect(),
            other => panic!("应该是 422，实际 {other:?}"),
        }
    }

    #[test]
    fn every_bad_field_is_reported_at_once() {
        let err = validate_input("  ", Some(0), &HostSelector::Local).expect_err("三处都不对");
        assert_eq!(fields(err), ["command", "timeout_s", "targets"]);
    }

    #[test]
    fn targets_must_be_real_remote_hosts() {
        let a = HostId::new();
        for (targets, why) in [
            (HostSelector::Hosts { host_ids: vec![] }, "空的"),
            (
                HostSelector::Hosts {
                    host_ids: vec![a, a],
                },
                "重复",
            ),
            (HostSelector::Tag { tag: " ".into() }, "空 tag"),
            (HostSelector::Local, "本机"),
        ] {
            assert_eq!(
                fields(validate_input("uptime", None, &targets).expect_err(why)),
                ["targets"],
                "{why}"
            );
        }
        assert!(validate_input("uptime", Some(60), &HostSelector::Host { host_id: a }).is_ok());
    }

    #[test]
    fn the_command_becomes_one_shell_step_over_the_checked_hosts() {
        let hosts = vec![HostId::new(), HostId::new()];
        let spec = adhoc_spec("df -h /", hosts.clone(), 120);
        assert!(ValidatedDag::validate(spec.clone()).is_ok());
        let node = &spec.nodes[0];
        assert_eq!(node.host, Some(HostSelector::Hosts { host_ids: hosts }));
        assert_eq!(node.timeout_s, Some(120));
        assert_eq!(node.retry.max_attempts, 1, "当场敲的命令不自动重试");
    }
}
