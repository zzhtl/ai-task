//! 目标机上的轻量执行体。
//!
//! 由中心节点经 SFTP 投送的 musl 静态二进制，按 sha256 复用。职责只有四件：
//!
//! 1. 在自己的 cgroup 里执行**已被策略批准**的命令
//! 2. 1Hz 采样那个 cgroup 的资源用量并回传
//! 3. 把文件操作限制在声明的根目录内
//! 4. 流式回传输出，带字节上限
//!
//! 目标机上**不需要 Node，也不需要任何 API key**——模型上下文和凭据全部留在
//! 中心节点，每一次工具调用都先回中心过策略层，批准了才下发到这里。
//!
//! 通信走 SSH 的 stdin/stdout，一行一条 JSON。日志一律走 stderr：stdout 是
//! 协议通道，往里写一个字节就会把对端的解析打乱。

mod cgroup;
mod fsops;

use std::io::{BufRead, BufReader, Read, Write};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ai_task_agent::protocol::{
    AgentInfo, AiCli, CgroupMode, Envelope, ExecRequest, ExecResult, OpResult, PROTOCOL_VERSION,
    Request, Response, Sample,
};
use cgroup::Scope;
use fsops::Roots;

/// 采样间隔。1Hz 足够画出曲线，又不会把目标机的 CPU 吃在采样上。
const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);

/// glob 一次最多返回多少条。
const GLOB_LIMIT: usize = 2_000;

fn main() {
    let roots: Vec<String> = std::env::args()
        .skip(1)
        .scan(false, |expect_root, arg| {
            let take = *expect_root;
            *expect_root = arg == "--root";
            Some((take, arg))
        })
        .filter_map(|(take, arg)| take.then_some(arg))
        .collect();

    let (mode, detail) = cgroup::detect();
    if let Some(detail) = &detail {
        eprintln!("[agent] 资源归因降级：{detail}");
    }

    let out = Arc::new(Mutex::new(std::io::stdout()));
    let roots = Roots::new(&roots);
    eprintln!("[agent] 允许的目录：{}", roots.describe());

    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let envelope: Envelope = match serde_json::from_str(&line) {
            Ok(envelope) => envelope,
            Err(err) => {
                // 解析不了也要回一条，否则中心节点会一直等
                send(
                    &out,
                    &Response::Done {
                        id: 0,
                        result: OpResult::Error {
                            message: format!("请求解析失败：{err}"),
                        },
                    },
                );
                continue;
            }
        };

        let id = envelope.id;
        match envelope.request {
            Request::Hello { protocol } => {
                if protocol != PROTOCOL_VERSION {
                    // 版本不匹配立刻报错，而不是让字段静默丢失
                    send(
                        &out,
                        &Response::Done {
                            id,
                            result: OpResult::Error {
                                message: format!(
                                    "协议版本不匹配：中心节点 {protocol}，agent {PROTOCOL_VERSION}"
                                ),
                            },
                        },
                    );
                    continue;
                }
                send(
                    &out,
                    &Response::Hello(AgentInfo {
                        protocol: PROTOCOL_VERSION,
                        agent_version: env!("CARGO_PKG_VERSION").into(),
                        arch: std::env::consts::ARCH.into(),
                        hostname: hostname(),
                        cgroup_mode: mode,
                        cgroup_detail: detail.clone(),
                        ai_clis: detect_ai_clis(),
                    }),
                );
                send(
                    &out,
                    &Response::Done {
                        id,
                        result: OpResult::Ok,
                    },
                );
            }
            Request::Exec(request) => {
                let result = exec(id, mode, &request, &out);
                send(&out, &Response::Done { id, result });
            }
            Request::Read { path, max_bytes } => {
                send(
                    &out,
                    &Response::Done {
                        id,
                        result: read_file(&roots, &path, max_bytes),
                    },
                );
            }
            Request::Write { path, content } => {
                send(
                    &out,
                    &Response::Done {
                        id,
                        result: write_file(&roots, &path, &content),
                    },
                );
            }
            Request::Glob { root, pattern } => {
                send(
                    &out,
                    &Response::Done {
                        id,
                        result: match roots.resolve(&root) {
                            Ok(dir) => OpResult::Glob {
                                paths: fsops::glob(&dir, &pattern, GLOB_LIMIT),
                            },
                            Err(err) => OpResult::Error {
                                message: err.to_string(),
                            },
                        },
                    },
                );
            }
            Request::Shutdown => {
                send(
                    &out,
                    &Response::Done {
                        id,
                        result: OpResult::Ok,
                    },
                );
                break;
            }
        }
    }
    eprintln!("[agent] 会话结束");
}

/// 执行一条命令：起进程、流式回传输出、按 1Hz 采样、超时就整棵树杀掉。
fn exec(
    id: u64,
    mode: CgroupMode,
    request: &ExecRequest,
    out: &Arc<Mutex<std::io::Stdout>>,
) -> OpResult {
    let unit = format!("ai-task-{id}-{}", std::process::id());
    let argv = Scope::wrap(mode, &unit, request.limits, &request.command);

    let mut command = std::process::Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .current_dir(&request.cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(err) => {
            // ENOENT 在这里几乎总是"工作目录不存在"，而不是"命令不存在"
            // ——命令是交给 sh -c 的，找不到会是退出码 127。裸报 errno
            // 的话，中心侧看到的只有 "No such file or directory"。
            let hint = if err.kind() == std::io::ErrorKind::NotFound
                && !std::path::Path::new(&request.cwd).is_dir()
            {
                format!("（工作目录 {} 在本机不存在）", request.cwd)
            } else {
                String::new()
            };
            return OpResult::Error {
                message: format!("启动命令失败：{err}{hint}"),
            };
        }
    };

    let mut scope = Scope::attach(mode, &unit, child.id());
    let limit = request.max_output_bytes;

    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();
    let stdout_task = spawn_reader(id, stdout_pipe, limit, out.clone(), true);
    let stderr_task = spawn_reader(id, stderr_pipe, limit, out.clone(), false);

    // 采样就在主循环里做。单独开线程的话 Scope 要在两个线程间共享，
    // 而主线程结束后还要读它的 OOM 状态——用一个循环把这两件事串起来更简单。
    let deadline = Instant::now() + Duration::from_millis(request.timeout_ms);
    let mut timed_out = false;
    let mut last_sample = Sample::default();

    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {}
            Err(err) => {
                return OpResult::Error {
                    message: format!("等待命令失败：{err}"),
                };
            }
        }
        if Instant::now() >= deadline {
            timed_out = true;
            scope.kill_all();
            let _ = child.kill();
            break;
        }
        last_sample = scope.sample();
        send(
            out,
            &Response::Metrics {
                id,
                sample: last_sample,
            },
        );
        std::thread::sleep(SAMPLE_INTERVAL);
    }

    let status = child.wait().ok();
    // 进程刚退出时 cgroup 还在，最后补一次采样才拿得到完整用量
    let final_sample = scope.sample();
    if final_sample.cpu_usec >= last_sample.cpu_usec {
        last_sample = final_sample;
    }
    let oom_killed = scope.was_oom_killed();
    scope.cleanup();

    let (stdout_truncated, _) = stdout_task.join().unwrap_or((false, 0));
    let (stderr_truncated, _) = stderr_task.join().unwrap_or((false, 0));

    OpResult::Exec(ExecResult {
        exit_code: status.as_ref().and_then(std::process::ExitStatus::code),
        killed_by_signal: status.as_ref().and_then(exit_signal),
        timed_out,
        oom_killed,
        stdout_truncated,
        stderr_truncated,
        resource: last_sample,
    })
}

/// 探测这台机器上能直接跑的 AI CLI。
///
/// 名单是写死的：让调用方传名字进来等于允许在目标机上执行任意名字的东西，
/// 而这个探测本身是在 hello 阶段做的，没有经过策略层。
const KNOWN_AI_CLIS: &[&str] = &[
    "claude",
    "codex",
    "gemini",
    "aider",
    "cursor-agent",
    "amp",
    "q",
];

fn detect_ai_clis() -> Vec<AiCli> {
    KNOWN_AI_CLIS
        .iter()
        .filter_map(|name| {
            // `command -v` 是 POSIX 的，`which` 在精简镜像里经常没有
            let found = Command::new("sh")
                .arg("-c")
                .arg(format!("command -v {name} 2>/dev/null"))
                .output()
                .ok()?;
            let path = String::from_utf8_lossy(&found.stdout).trim().to_string();
            if path.is_empty() {
                return None;
            }
            // 版本取不到不代表它不存在——不同 CLI 的版本参数不一样，
            // 而且有些会等在交互提示上，所以给个短超时的下限：只试一次。
            let version = Command::new(&path)
                .arg("--version")
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| {
                    // **只取第一行、且截断。** 有些 CLI 的 --version 会打一大段
                    // （帮助、横幅，甚至像我们的测试替身那样直接开工），
                    // 原样存进去就是往数据库和界面里灌垃圾。
                    String::from_utf8_lossy(&o.stdout)
                        .lines()
                        .next()
                        .unwrap_or_default()
                        .trim()
                        .chars()
                        .take(120)
                        .collect::<String>()
                })
                .filter(|v| !v.is_empty());
            Some(AiCli {
                name: (*name).to_string(),
                path,
                version,
            })
        })
        .collect()
}

#[cfg(unix)]
fn exit_signal(status: &std::process::ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt as _;
    status.signal()
}

#[cfg(not(unix))]
fn exit_signal(_: &std::process::ExitStatus) -> Option<i32> {
    None
}

/// 起一个线程把管道内容流式送回去，超过上限就停止转发并标记截断。
fn spawn_reader<R: Read + Send + 'static>(
    id: u64,
    pipe: Option<R>,
    limit: u64,
    out: Arc<Mutex<std::io::Stdout>>,
    is_stdout: bool,
) -> std::thread::JoinHandle<(bool, u64)> {
    std::thread::spawn(move || {
        let Some(pipe) = pipe else {
            return (false, 0);
        };
        let mut reader = BufReader::new(pipe);
        let mut sent = 0u64;
        let mut truncated = false;
        let mut buffer = [0u8; 8192];

        loop {
            let read = match reader.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            if truncated {
                // 已经超限：继续读干净管道，免得子进程写阻塞，但不再回传
                continue;
            }
            let remaining = limit.saturating_sub(sent);
            let take = usize::try_from(remaining).unwrap_or(usize::MAX).min(read);
            if take < read {
                truncated = true;
            }
            if take == 0 {
                continue;
            }
            sent += take as u64;
            let data = String::from_utf8_lossy(&buffer[..take]).to_string();
            let message = if is_stdout {
                Response::Stdout { id, data }
            } else {
                Response::Stderr { id, data }
            };
            send(&out, &message);
        }
        (truncated, sent)
    })
}

fn read_file(roots: &Roots, path: &str, max_bytes: u64) -> OpResult {
    let resolved = match roots.resolve(path) {
        Ok(p) => p,
        Err(err) => {
            return OpResult::Error {
                message: err.to_string(),
            };
        }
    };
    match std::fs::read(&resolved) {
        Ok(bytes) => {
            let limit = usize::try_from(max_bytes).unwrap_or(usize::MAX);
            let truncated = bytes.len() > limit;
            OpResult::Read {
                content: String::from_utf8_lossy(&bytes[..bytes.len().min(limit)]).to_string(),
                truncated,
            }
        }
        Err(err) => OpResult::Error {
            message: format!("读取 {path} 失败：{err}"),
        },
    }
}

fn write_file(roots: &Roots, path: &str, content: &str) -> OpResult {
    let resolved = match roots.resolve_for_create(path) {
        Ok(p) => p,
        Err(err) => {
            return OpResult::Error {
                message: err.to_string(),
            };
        }
    };
    match std::fs::write(&resolved, content) {
        Ok(()) => OpResult::Ok,
        Err(err) => OpResult::Error {
            message: format!("写入 {path} 失败：{err}"),
        },
    }
}

/// 往 stdout 写一条消息。
///
/// 加锁是必须的：三个线程（stdout/stderr 读取、主循环）都会往这里写，
/// 不加锁会把 JSON 行交错在一起，对端解析全乱。
fn send(out: &Arc<Mutex<std::io::Stdout>>, message: &Response) {
    let Ok(json) = serde_json::to_string(message) else {
        return;
    };
    if let Ok(mut handle) = out.lock() {
        let _ = writeln!(handle, "{json}");
        let _ = handle.flush();
    }
}

fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "unknown".into())
}
