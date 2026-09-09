//! 中心节点与远端 agent 之间的线上协议。
//!
//! 一行一条 JSON，走 SSH 的 stdin/stdout。选 JSON 而不是二进制编码：
//! 量很小（是工具调用，不是批量数据），而出问题时能直接把一行贴出来看。
//! 批量文件传输走独立的 SFTP 通道，不挤这条管子。

use serde::{Deserialize, Serialize};

/// 协议版本。中心节点与 agent 不匹配时立刻报错，而不是让字段静默丢失。
pub const PROTOCOL_VERSION: u32 = 1;

/// 中心节点发给 agent 的请求。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// 握手。必须是第一条。
    Hello { protocol: u32 },
    /// 执行一条命令。
    Exec(ExecRequest),
    /// 读文件。
    Read { path: String, max_bytes: u64 },
    /// 写文件。
    Write { path: String, content: String },
    /// 按 glob 列文件。
    Glob { root: String, pattern: String },
    /// 结束会话。
    Shutdown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecRequest {
    /// 交给 `sh -c` 的命令行。
    pub command: String,
    pub cwd: String,
    pub timeout_ms: u64,
    /// 资源上限。`None` 表示不设限（但仍然会记账）。
    #[serde(default)]
    pub limits: Option<Limits>,
    /// stdout/stderr 各自回传的字节上限。超出就截断并标记。
    #[serde(default = "default_output_limit")]
    pub max_output_bytes: u64,
}

fn default_output_limit() -> u64 {
    256 * 1024
}

/// 资源上限。
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Limits {
    /// 内存上限（字节）。超出由内核 OOM killer 处理，**不是**软提示。
    #[serde(default)]
    pub memory_max_bytes: Option<u64>,
    /// CPU 配额，百分比。200 表示最多用满两个核。
    #[serde(default)]
    pub cpu_quota_percent: Option<u32>,
    /// 进程数上限。挡住 fork 炸弹。
    #[serde(default)]
    pub pids_max: Option<u32>,
}

/// agent 发回中心节点的消息。
///
/// 一次请求会产生零到多条流式消息（`Stdout`/`Stderr`/`Metrics`），
/// 最后必然有一条 `Done`。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "msg", rename_all = "snake_case")]
pub enum Response {
    Hello(AgentInfo),
    Stdout { id: u64, data: String },
    Stderr { id: u64, data: String },
    Metrics { id: u64, sample: Sample },
    Done { id: u64, result: OpResult },
}

/// agent 自报家门。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentInfo {
    pub protocol: u32,
    pub agent_version: String,
    pub arch: String,
    pub hostname: String,
    /// 资源归因用的是哪种机制。降级了要如实上报，界面上会标出来。
    pub cgroup_mode: CgroupMode,
    /// 降级的原因。`cgroup_mode` 不是 `Systemd` 时才有值。
    #[serde(default)]
    pub cgroup_detail: Option<String>,
    /// 这台机器上能直接跑的 AI CLI。
    ///
    /// 界面据此决定"这台机器能不能选"——让人选一个装都没装的 CLI，
    /// 失败会发生在凌晨两点，而不是配置的时候。
    #[serde(default)]
    pub ai_clis: Vec<AiCli>,
}

/// 目标机上探测到的一个 AI CLI。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiCli {
    /// 可执行文件名，如 `claude`、`codex`。
    pub name: String,
    /// 解析出来的绝对路径。
    pub path: String,
    /// `--version` 的输出。取不到时为 `None`——**不能因此就当它不存在**，
    /// 有些 CLI 的版本参数不一样。
    #[serde(default)]
    pub version: Option<String>,
}

/// 资源归因的能力档位。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CgroupMode {
    /// `systemd-run --user --scope`：能设上限，也能精确记账。
    Systemd,
    /// 直接读 `/proc` 的进程树：**能记账但不能设限**。
    Proc,
    /// 连进程树都拿不到（`/proc` 不可用）。只有任务状态，没有资源归因。
    None,
}

impl CgroupMode {
    /// 这个档位能不能强制资源上限。
    #[must_use]
    pub fn enforces_limits(self) -> bool {
        matches!(self, Self::Systemd)
    }
}

/// 一次资源采样。
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Sample {
    /// 采样时刻，agent 本地的 Unix 毫秒。
    pub at_ms: u64,
    /// 累计 CPU 时间（微秒）。是累计量，不是瞬时值——中心节点做差得到速率。
    pub cpu_usec: u64,
    /// 当前常驻内存（字节）。
    pub rss_bytes: u64,
    /// 峰值常驻内存（字节）。
    pub peak_rss_bytes: u64,
    /// 当前进程数。
    pub pids: u32,
}

/// 一次操作的结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OpResult {
    Ok,
    Exec(ExecResult),
    Read {
        content: String,
        truncated: bool,
    },
    Glob {
        paths: Vec<String>,
    },
    /// 操作失败。**不是**命令返回非零——那是 `Exec` 的正常结果。
    Error {
        message: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecResult {
    pub exit_code: Option<i32>,
    /// 被信号杀死时的信号号。
    #[serde(default)]
    pub killed_by_signal: Option<i32>,
    /// 超时被我们主动杀掉。
    pub timed_out: bool,
    /// 命中内存上限被内核杀掉。
    ///
    /// 与「命令自己失败」必须分开：前者是资源问题，run 应当落
    /// `resource_exceeded` 而不是 `failed`。
    pub oom_killed: bool,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    /// 本次执行的最终资源用量。
    pub resource: Sample,
}

/// 带 id 的请求信封。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope {
    pub id: u64,
    #[serde(flatten)]
    pub request: Request,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_round_trip_with_a_flat_op_tag() {
        let envelope = Envelope {
            id: 7,
            request: Request::Exec(ExecRequest {
                command: "df -h".into(),
                cwd: "/tmp".into(),
                timeout_ms: 30_000,
                limits: Some(Limits {
                    memory_max_bytes: Some(512 * 1024 * 1024),
                    cpu_quota_percent: Some(200),
                    pids_max: Some(256),
                }),
                max_output_bytes: 1024,
            }),
        };
        let json = serde_json::to_value(&envelope).expect("序列化");
        // op 与 id 同级，一行 JSON 一眼能看懂
        assert_eq!(json["op"], "exec");
        assert_eq!(json["id"], 7);
        assert_eq!(json["command"], "df -h");

        let back: Envelope = serde_json::from_value(json).expect("反序列化");
        assert!(matches!(back.request, Request::Exec(_)));
    }

    #[test]
    fn optional_limits_default_to_none() {
        let request: Envelope = serde_json::from_str(
            r#"{"id":1,"op":"exec","command":"ls","cwd":"/","timeout_ms":1000}"#,
        )
        .expect("最小请求也要能解析");
        let Request::Exec(exec) = request.request else {
            panic!("类型不对")
        };
        assert!(exec.limits.is_none());
        assert_eq!(exec.max_output_bytes, 256 * 1024);
    }

    #[test]
    fn only_the_systemd_mode_can_enforce_limits() {
        // 降级到 /proc 时只能记账不能设限。谎报这一点比不报更糟：
        // 用户会以为自己配的 MemoryMax 在生效。
        assert!(CgroupMode::Systemd.enforces_limits());
        assert!(!CgroupMode::Proc.enforces_limits());
        assert!(!CgroupMode::None.enforces_limits());
    }

    #[test]
    fn oom_is_distinguishable_from_an_ordinary_failure() {
        let oom = ExecResult {
            exit_code: None,
            killed_by_signal: Some(9),
            timed_out: false,
            oom_killed: true,
            stdout_truncated: false,
            stderr_truncated: false,
            resource: Sample::default(),
        };
        let plain = ExecResult {
            exit_code: Some(1),
            killed_by_signal: None,
            timed_out: false,
            oom_killed: false,
            stdout_truncated: false,
            stderr_truncated: false,
            resource: Sample::default(),
        };
        // run 的终态要据此区分 resource_exceeded 和 failed
        assert!(oom.oom_killed && !plain.oom_killed);
    }
}
