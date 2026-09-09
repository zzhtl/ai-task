//! 在目标机上执行命令。
//!
//! 本机与远端走**同一套协议和同一个 agent 二进制**：本机执行因此也拿到了资源
//! 归因和资源上限，`HostSelector::Local` 不再是「什么都没有」的那一档。
//! 同一条代码路径也意味着只有远端才会暴露的 bug 在本地就能撞见。
//!
//! 资源采样**不进事件日志**：1Hz × N 节点会把它撑爆，而这些点对状态推导没有
//! 贡献。它们进按时间分区的 `run_metrics` 表，归档时直接 DETACH。
//!
//! 采样是**边采边落库**的，不是攒到命令结束再一次性写。一次跑几十分钟的
//! 任务如果要等它结束才有曲线，"实时监控对服务器的影响"就无从谈起；
//! 而且连接一断，攒在内存里的那些点会一起消失——那正是最该看到曲线的时候。

use std::path::PathBuf;

use ai_task_agent::protocol::{CgroupMode, ExecRequest, ExecResult, Limits};
use ai_task_exec::remote::{
    AgentEvent, HostKeyPolicy, RemoteAgent, SshConfig, SshSession, sha256_hex, start_local_agent,
};
use ai_task_proto::{HostId, HostSelector, NodeStatus, ResourceLimits, RunId};
use ai_task_store::{MetricSample, Store, StoreError};

use crate::dag::NodeResult;

/// agent 的部署位置。
const REMOTE_AGENT_DIR: &str = "/tmp";

/// 一次 shell 执行的落点。
#[derive(Debug, Clone)]
pub struct HostExecConfig {
    /// 本机 agent 二进制。
    pub local_agent: PathBuf,
    /// 推给远端的 musl 静态二进制。`None` 表示没准备，远端执行会明确报错。
    pub remote_agent: Option<PathBuf>,
    pub known_hosts: PathBuf,
    pub host_key_policy: HostKeyPolicy,
}

#[derive(Debug, thiserror::Error)]
pub enum HostExecError {
    #[error(transparent)]
    Store(#[from] StoreError),

    #[error(
        "远端执行不可用：没有可推送的 agent 二进制。先执行 `cargo build -p ai-task-agent --profile agent-release --target x86_64-unknown-linux-musl`，再用 --remote-agent 指向它"
    )]
    NoRemoteAgent,

    #[error("按 tag 选主机（tag={0}）在 M4 里还没实现，先用 host_id 指定")]
    TagSelectorUnsupported(String),

    #[error(transparent)]
    Agent(#[from] ai_task_exec::remote::AgentError),

    #[error(transparent)]
    Ssh(#[from] ai_task_exec::remote::SshError),

    #[error("读取 agent 二进制 {path} 失败：{detail}")]
    AgentBinary { path: String, detail: String },
}

/// 一次执行的产物。
pub struct HostExecOutcome {
    pub result: ExecResult,
    pub stdout: String,
    pub stderr: String,
    /// 已落库的采样点数。曲线本身在 `run_metrics` 里，不在这里。
    pub samples: usize,
    pub host_id: Option<HostId>,
    /// 目标机实际的归因档位。降级了要写进事件日志。
    pub cgroup_mode: CgroupMode,
    pub cgroup_detail: Option<String>,
    /// 本次执行申报的上限。被 kill 时要报出来的是它，不是采样到的峰值。
    pub limits: Option<ResourceLimits>,
}

/// 一次命令执行的全部参数。
pub struct Command<'a> {
    pub workspace_id: ai_task_proto::WorkspaceId,
    pub run_id: RunId,
    pub node_key: &'a str,
    /// `None` 等同 [`HostSelector::Local`]。
    pub selector: Option<&'a HostSelector>,
    /// 交给 `sh -c`。Debian/Ubuntu 上那是 dash，不是 bash。
    pub command: &'a str,
    pub cwd: &'a str,
    pub timeout_ms: u64,
    pub limits: Option<ResourceLimits>,
    /// 文件操作允许的根目录。**空表示禁止所有文件操作**，不是"不限制"。
    pub roots: &'a [String],
}

/// 在选定的主机上跑一条命令。
pub async fn run_command(
    store: &Store,
    config: &HostExecConfig,
    req: Command<'_>,
) -> Result<HostExecOutcome, HostExecError> {
    let Command {
        workspace_id,
        run_id,
        node_key,
        selector,
        command,
        cwd,
        timeout_ms,
        limits,
        roots,
    } = req;
    let (mut agent, host_id) = match selector {
        None | Some(HostSelector::Local) => {
            let agent = start_local_agent(&config.local_agent, roots).await?;
            (agent, None)
        }
        Some(HostSelector::Host { host_id }) => {
            let agent = connect_remote(store, config, workspace_id, *host_id, roots).await?;
            (agent, Some(*host_id))
        }
        Some(HostSelector::Tag { tag }) => {
            return Err(HostExecError::TagSelectorUnsupported(tag.clone()));
        }
    };

    let info = agent.info().clone();
    let mut stdout = String::new();
    let mut stderr = String::new();
    let mut sampled = 0usize;

    // 采样点走独立通道边采边落库。用无界通道是安全的：生产端是 1Hz，
    // 消费端是一条批量 INSERT，堆积不起来；用有界通道反而要在
    // "阻塞采样"和"丢点"之间选一个。
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let flusher = tokio::spawn(flush_metrics(
        store.clone(),
        run_id,
        node_key.to_owned(),
        host_id,
        rx,
    ));

    let result = agent
        .exec(
            ExecRequest {
                command: command.to_string(),
                cwd: cwd.to_string(),
                timeout_ms,
                limits: limits.map(to_agent_limits),
                max_output_bytes: 256 * 1024,
            },
            |event| match event {
                AgentEvent::Stdout(data) => stdout.push_str(&data),
                AgentEvent::Stderr(data) => stderr.push_str(&data),
                // 发送失败只意味着落库任务已经退了，不该让执行跟着失败
                AgentEvent::Metrics(sample) => {
                    sampled += 1;
                    let _ = tx.send(MetricSample {
                        at: chrono::DateTime::from_timestamp_millis(
                            i64::try_from(sample.at_ms).unwrap_or(0),
                        )
                        .unwrap_or_else(chrono::Utc::now),
                        cpu_usec: i64::try_from(sample.cpu_usec).unwrap_or(i64::MAX),
                        rss_bytes: i64::try_from(sample.rss_bytes).unwrap_or(i64::MAX),
                        pids: i32::try_from(sample.pids).unwrap_or(i32::MAX),
                    });
                }
            },
        )
        .await;

    // 连接断了也要把已经采到的点落库——那正是最需要看曲线的时候。
    // 所以先收尾 flusher，再把执行结果的错误抛出去。
    // 命令比一个采样周期还短时，一个点都没有——那样曲线是空的。
    // **只在这种情况下**补终态那一条：进程已经退出，它和上一个采样点之间
    // 的 CPU 增量是 0，无条件补的话每条曲线末尾都会多一段掉到零的假下降。
    if sampled == 0
        && let Ok(exec) = &result
    {
        let _ = tx.send(MetricSample {
            at: chrono::Utc::now(),
            cpu_usec: i64::try_from(exec.resource.cpu_usec).unwrap_or(i64::MAX),
            rss_bytes: i64::try_from(exec.resource.peak_rss_bytes.max(exec.resource.rss_bytes))
                .unwrap_or(i64::MAX),
            pids: i32::try_from(exec.resource.pids).unwrap_or(i32::MAX),
        });
    }
    drop(tx);
    let samples = flusher.await.unwrap_or(0);
    let result = result?;

    let _ = agent.shutdown().await;

    Ok(HostExecOutcome {
        result,
        stdout,
        stderr,
        samples,
        host_id,
        cgroup_mode: info.cgroup_mode,
        cgroup_detail: info.cgroup_detail,
        limits,
    })
}

pub(crate) async fn connect_remote(
    store: &Store,
    config: &HostExecConfig,
    workspace_id: ai_task_proto::WorkspaceId,
    host_id: HostId,
    roots: &[String],
) -> Result<RemoteAgent, HostExecError> {
    let binary_path = config
        .remote_agent
        .as_ref()
        .ok_or(HostExecError::NoRemoteAgent)?;
    let binary = tokio::fs::read(binary_path)
        .await
        .map_err(|err| HostExecError::AgentBinary {
            path: binary_path.display().to_string(),
            detail: err.to_string(),
        })?;
    let sha = sha256_hex(&binary);

    let (host, private_key) = store.get_host(workspace_id, host_id).await?;

    // 私钥只在内存里存在。写临时文件是因为 russh 的装载接口吃路径；
    // 权限设 0600，用完立刻删。
    let key_path = write_private_key(&private_key).await?;
    let session = SshSession::connect(&SshConfig {
        host: host.address.clone(),
        port: u16::try_from(host.port).unwrap_or(22),
        user: host.username.clone(),
        key_path: key_path.clone(),
        key_passphrase: None,
        known_hosts: config.known_hosts.clone(),
        policy: config.host_key_policy,
        connect_timeout: std::time::Duration::from_secs(15),
    })
    .await;
    let _ = tokio::fs::remove_file(&key_path).await;
    let session = session?;

    let deployed = session
        .ensure_agent(&binary, &sha, REMOTE_AGENT_DIR)
        .await?;
    if deployed.uploaded {
        tracing::info!(host = %host.name, path = %deployed.path, "已投送 agent");
    }

    let agent = RemoteAgent::start(&session, &deployed.path, roots).await?;
    // 记下这次探到的能力，界面上据此标出降级的主机
    let mode = format!("{:?}", agent.info().cgroup_mode).to_lowercase();
    let clis =
        serde_json::to_value(&agent.info().ai_clis).unwrap_or_else(|_| serde_json::json!([]));
    let _ = store.record_host_probe(host_id, &sha, &mode, &clis).await;

    // session 要活到 agent 用完为止。RemoteAgent 持有 channel，
    // 而 channel 的生命周期绑在 handle 上，所以这里把 session 泄漏给它。
    // M5 上连接池时会换成显式持有。
    std::mem::forget(session);
    Ok(agent)
}

async fn write_private_key(key: &str) -> Result<PathBuf, HostExecError> {
    let path = std::env::temp_dir().join(format!("ai-task-key-{}", uuid::Uuid::now_v7()));
    tokio::fs::write(&path, key)
        .await
        .map_err(|err| HostExecError::AgentBinary {
            path: path.display().to_string(),
            detail: err.to_string(),
        })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).await;
    }
    Ok(path)
}

fn to_agent_limits(limits: ResourceLimits) -> Limits {
    Limits {
        memory_max_bytes: limits.memory_mib.map(|m| u64::from(m) * 1024 * 1024),
        cpu_quota_percent: limits.cpu_percent,
        pids_max: limits.pids_max,
    }
}

/// 把一次执行的结果翻译成节点结果。
///
/// **资源超限必须与普通失败区分开**：前者 run 落 `resource_exceeded`，
/// 后者落 `failed`。混在一起的话，"任务写错了"和"机器扛不住"在界面上长得一样。
#[must_use]
pub fn to_node_result(outcome: &HostExecOutcome) -> NodeResult {
    let exec = &outcome.result;
    if exec.oom_killed {
        // **不要报采样到的峰值。** 进程可能在两次采样之间就冲到上限被杀，
        // 那时 cgroup 已经没了，`memory.peak` 读不回来，高水位只剩最后一次
        // 采样的值——报出来会是"命中 512 MiB 上限（峰值 13 MiB）"这种
        // 自相矛盾的话。确定为真的是上限本身。
        let limit = outcome.limits.and_then(|l| l.memory_mib).map_or_else(
            || "内存上限".to_owned(),
            |mib| format!("{mib} MiB 内存上限"),
        );
        return NodeResult {
            status: NodeStatus::Failed,
            output: None,
            error: Some(format!(
                "命中 {limit}，整个进程组已被内核终止。调高 limits.memory_mib，或让任务分批处理"
            )),
            cost: ai_task_proto::UsdMicros::ZERO,
            cli_version: None,
        };
    }
    if exec.timed_out {
        return NodeResult::failed("命令超时，已连同其子进程一并终止");
    }
    match exec.exit_code {
        Some(0) => NodeResult::succeeded(Some(serde_json::json!({
            "stdout": outcome.stdout.trim(),
            "stderr": outcome.stderr.trim(),
            "exit_code": 0,
        }))),
        Some(code) => NodeResult::failed(format!(
            "命令退出码 {code}：{}",
            first_line(&outcome.stderr)
                .unwrap_or_else(|| first_line(&outcome.stdout).unwrap_or_default())
        )),
        None => NodeResult::failed(format!(
            "命令被信号 {} 终止",
            exec.killed_by_signal.unwrap_or(0)
        )),
    }
}

/// run 的终态是不是该记成资源超限。
#[must_use]
pub fn is_resource_exceeded(outcome: &HostExecOutcome) -> bool {
    outcome.result.oom_killed
}

fn first_line(text: &str) -> Option<String> {
    text.lines()
        .find(|l| !l.trim().is_empty())
        .map(|l| l.trim().chars().take(200).collect())
}

/// 攒够一小批或到点就写一次。返回成功落库的点数。
///
/// 每来一个点写一次 INSERT 太浪费（1Hz × N 节点 × 多副本）；攒太久又会在
/// 断线时丢掉一段。1 秒 / 8 个点是个折中：界面上看不出延迟，断线最多丢 1 秒。
async fn flush_metrics(
    store: Store,
    run_id: RunId,
    node_key: String,
    host_id: Option<HostId>,
    mut rx: tokio::sync::mpsc::UnboundedReceiver<MetricSample>,
) -> usize {
    const MAX_BATCH: usize = 8;
    const FLUSH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

    let mut buffer: Vec<MetricSample> = Vec::with_capacity(MAX_BATCH);
    let mut written = 0usize;
    let mut ticker = tokio::time::interval(FLUSH_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut closed = false;

    loop {
        // 注意别在这里用 `try_recv()` 去"看看还有没有"——它会把消息取走。
        // 攒批的条件只能看已经收进 buffer 的量。
        let flush_now = tokio::select! {
            received = rx.recv() => match received {
                Some(sample) => {
                    buffer.push(sample);
                    buffer.len() >= MAX_BATCH
                }
                None => {
                    closed = true;
                    true
                }
            },
            _ = ticker.tick() => true,
        };

        if flush_now && !buffer.is_empty() {
            match store
                .append_metrics(run_id, &node_key, host_id, &buffer)
                .await
            {
                // 采样丢了不该让 run 失败——它是观测数据，不是状态
                Err(err) => {
                    tracing::warn!(%run_id, node = %node_key, error = %err, "写入资源采样失败");
                }
                Ok(()) => written += buffer.len(),
            }
            buffer.clear();
        }
        if closed {
            return written;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ai_task_agent::protocol::Sample;

    fn outcome(exec: ExecResult) -> HostExecOutcome {
        HostExecOutcome {
            result: exec,
            stdout: String::new(),
            stderr: String::new(),
            samples: 0,
            host_id: None,
            cgroup_mode: CgroupMode::Systemd,
            cgroup_detail: None,
            limits: None,
        }
    }

    fn exec(exit_code: Option<i32>) -> ExecResult {
        ExecResult {
            exit_code,
            killed_by_signal: None,
            timed_out: false,
            oom_killed: false,
            stdout_truncated: false,
            stderr_truncated: false,
            resource: Sample::default(),
        }
    }

    #[test]
    fn an_oom_kill_is_not_reported_as_an_ordinary_command_failure() {
        // "任务写错了"和"机器扛不住"必须能在界面上分开
        let mut killed = exec(None);
        killed.oom_killed = true;
        // 进程在两次采样之间冲到上限，高水位停在一个很小的值
        killed.resource.peak_rss_bytes = 13 * 1024 * 1024;
        let mut killed = outcome(killed);
        killed.limits = Some(ResourceLimits {
            memory_mib: Some(512),
            cpu_percent: None,
            pids_max: None,
        });

        assert!(is_resource_exceeded(&killed));
        let result = to_node_result(&killed);
        assert_eq!(result.status, NodeStatus::Failed);
        let error = result.error.expect("要有原因");
        assert!(error.contains("512 MiB"), "要报出申报的上限：{error}");
        assert!(
            !error.contains("13"),
            "不能把采样到的高水位当峰值报出来——它比上限还小，读起来是自相矛盾的：{error}"
        );

        assert!(!is_resource_exceeded(&outcome(exec(Some(1)))));
    }

    #[test]
    fn an_oom_kill_without_a_declared_limit_still_says_what_happened() {
        // 机器上有别的来源的上限（容器、systemd slice）时 limits 会是 None
        let mut killed = exec(None);
        killed.oom_killed = true;
        let error = to_node_result(&outcome(killed)).error.expect("要有原因");
        assert!(error.contains("内存上限"), "{error}");
    }

    #[test]
    fn a_timeout_says_it_also_killed_the_children() {
        let mut timed = exec(None);
        timed.timed_out = true;
        let error = to_node_result(&outcome(timed)).error.expect("要有原因");
        assert!(error.contains("子进程"), "{error}");
    }

    #[test]
    fn a_successful_command_exposes_stdout_to_downstream_nodes() {
        let mut ok = outcome(exec(Some(0)));
        ok.stdout = "  42\n".into();
        let result = to_node_result(&ok);
        assert_eq!(result.status, NodeStatus::Succeeded);
        assert_eq!(
            result.output.expect("有输出")["stdout"],
            serde_json::json!("42")
        );
    }

    #[test]
    fn a_failing_command_surfaces_the_first_line_of_stderr() {
        let mut failed = outcome(exec(Some(2)));
        failed.stderr = "\n\ncp: cannot stat '/nope': No such file\nmore noise\n".into();
        let error = to_node_result(&failed).error.expect("要有原因");
        assert!(error.contains("退出码 2"), "{error}");
        assert!(error.contains("cannot stat"), "{error}");
        assert!(!error.contains("more noise"), "只取第一行：{error}");
    }

    #[test]
    fn limits_are_converted_to_bytes() {
        let limits = to_agent_limits(ResourceLimits {
            memory_mib: Some(512),
            cpu_percent: Some(200),
            pids_max: Some(64),
        });
        assert_eq!(limits.memory_max_bytes, Some(512 * 1024 * 1024));
        assert_eq!(limits.cpu_quota_percent, Some(200));
    }
}
