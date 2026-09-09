//! 驱动真实 agent 进程的协议测试。
//!
//! 这些必须跑真进程：cgroup 归因、OOM 检测、超时杀整棵进程树，全都是
//! 「代码看着对但在这台机器上不成立」的重灾区，单测覆盖不到。

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::time::Duration;

use ai_task_agent::protocol::{
    AgentInfo, CgroupMode, Envelope, ExecRequest, Limits, OpResult, PROTOCOL_VERSION, Request,
    Response,
};

struct Session {
    child: Child,
    stdin: ChildStdin,
    lines: std::io::Lines<BufReader<std::process::ChildStdout>>,
    next_id: u64,
}

impl Session {
    fn start(roots: &[&str]) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_ai-task-agent"));
        for root in roots {
            command.args(["--root", root]);
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("拉起 agent");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = child.stdout.take().expect("stdout");
        Self {
            child,
            stdin,
            lines: BufReader::new(stdout).lines(),
            next_id: 0,
        }
    }

    fn send(&mut self, request: Request) -> u64 {
        self.next_id += 1;
        let json = serde_json::to_string(&Envelope {
            id: self.next_id,
            request,
        })
        .expect("序列化");
        writeln!(self.stdin, "{json}").expect("写入");
        self.stdin.flush().expect("flush");
        self.next_id
    }

    /// 收到这个 id 的 `Done` 为止，把中间的流式消息一并带回。
    fn collect(&mut self, id: u64) -> (OpResult, Vec<Response>) {
        let mut streamed = Vec::new();
        loop {
            let line = self
                .lines
                .next()
                .expect("agent 提前关闭了输出")
                .expect("读取一行");
            let response: Response =
                serde_json::from_str(&line).unwrap_or_else(|e| panic!("无法解析 {line}：{e}"));
            match response {
                Response::Done { id: got, result } if got == id => return (result, streamed),
                other => streamed.push(other),
            }
        }
    }

    fn hello(&mut self) -> AgentInfo {
        let id = self.send(Request::Hello {
            protocol: PROTOCOL_VERSION,
        });
        let (_, streamed) = self.collect(id);
        streamed
            .into_iter()
            .find_map(|r| match r {
                Response::Hello(info) => Some(info),
                _ => None,
            })
            .expect("握手必须回一条 Hello")
    }

    fn exec(&mut self, request: ExecRequest) -> (OpResult, Vec<Response>) {
        let id = self.send(Request::Exec(request));
        self.collect(id)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn exec_request(command: &str) -> ExecRequest {
    ExecRequest {
        command: command.into(),
        cwd: "/tmp".into(),
        timeout_ms: 30_000,
        limits: None,
        max_output_bytes: 64 * 1024,
    }
}

fn stdout_of(streamed: &[Response]) -> String {
    streamed
        .iter()
        .filter_map(|r| match r {
            Response::Stdout { data, .. } => Some(data.as_str()),
            _ => None,
        })
        .collect()
}

// ---------------------------------------------------------------- 测试

#[test]
fn a_session_reports_its_real_attribution_capability() {
    let mut session = Session::start(&["/tmp"]);
    let info = session.hello();

    assert_eq!(info.protocol, PROTOCOL_VERSION);
    assert!(!info.hostname.is_empty());
    assert_eq!(info.arch, std::env::consts::ARCH);
    // 降级了必须给出原因——用户要能知道为什么自己配的限额没生效
    if info.cgroup_mode != CgroupMode::Systemd {
        assert!(
            info.cgroup_detail.is_some(),
            "降级到 {:?} 却没说原因",
            info.cgroup_mode
        );
    }
    eprintln!(
        "本机资源归因档位：{:?}（{:?}）",
        info.cgroup_mode, info.cgroup_detail
    );
}

#[test]
fn a_protocol_mismatch_is_refused_at_the_handshake() {
    // 版本对不上时立刻报错，而不是让字段静默丢失
    let mut session = Session::start(&["/tmp"]);
    let id = session.send(Request::Hello {
        protocol: PROTOCOL_VERSION + 99,
    });
    let (result, _) = session.collect(id);
    let OpResult::Error { message } = result else {
        panic!("版本不匹配必须被拒：{result:?}");
    };
    assert!(message.contains("协议版本不匹配"), "{message}");
}

#[test]
fn a_command_streams_its_output_and_reports_the_exit_code() {
    let mut session = Session::start(&["/tmp"]);
    session.hello();

    let (result, streamed) = session.exec(exec_request("echo hello; echo oops >&2; exit 3"));
    let OpResult::Exec(exec) = result else {
        panic!("{result:?}")
    };
    assert_eq!(exec.exit_code, Some(3));
    assert!(!exec.oom_killed);
    assert!(!exec.timed_out);
    assert!(stdout_of(&streamed).contains("hello"));
    assert!(
        streamed
            .iter()
            .any(|r| matches!(r, Response::Stderr { data, .. } if data.contains("oops")))
    );
}

#[test]
fn output_beyond_the_cap_is_truncated_and_flagged() {
    let mut session = Session::start(&["/tmp"]);
    session.hello();

    let mut request = exec_request("head -c 200000 /dev/zero | tr '\\0' 'x'");
    request.max_output_bytes = 4096;
    let (result, streamed) = session.exec(request);

    let OpResult::Exec(exec) = result else {
        panic!("{result:?}")
    };
    assert!(exec.stdout_truncated, "超限必须标记，不能悄悄少传");
    // 回传量要真的受控——不然"上限"只是个装饰
    assert!(
        stdout_of(&streamed).len() <= 8192,
        "实际回传了 {} 字节",
        stdout_of(&streamed).len()
    );
}

#[test]
fn a_cpu_burning_command_produces_attributed_samples() {
    let mut session = Session::start(&["/tmp"]);
    let info = session.hello();

    // 用 date 而不是 $SECONDS：命令走的是 `sh -c`，在 Debian/Ubuntu 上那是
    // dash，没有 SECONDS 这个 bash 扩展。这条踩过一次。
    let (result, streamed) = session.exec(exec_request(
        "end=$(( $(date +%s) + 3 )); while [ $(date +%s) -lt $end ]; do :; done",
    ));
    let OpResult::Exec(exec) = result else {
        panic!("{result:?}")
    };

    let samples: Vec<_> = streamed
        .iter()
        .filter_map(|r| match r {
            Response::Metrics { sample, .. } => Some(*sample),
            _ => None,
        })
        .collect();
    assert!(
        samples.len() >= 2,
        "3 秒的命令至少该采到两个点：{}",
        samples.len()
    );

    // 累计 CPU 只能涨不能落——中心节点靠做差算速率
    for pair in samples.windows(2) {
        assert!(pair[1].cpu_usec >= pair[0].cpu_usec, "{pair:?}");
        assert!(pair[1].at_ms >= pair[0].at_ms);
    }
    assert!(
        exec.resource.cpu_usec > 500_000,
        "空转 3 秒的 CPU 记账只有 {} 微秒，归因没生效（档位 {:?}）",
        exec.resource.cpu_usec,
        info.cgroup_mode
    );
    assert!(exec.resource.rss_bytes > 0);
}

#[test]
fn exceeding_the_memory_limit_is_reported_as_an_oom_kill_not_a_plain_failure() {
    let mut session = Session::start(&["/tmp"]);
    let info = session.hello();
    if !info.cgroup_mode.enforces_limits() {
        eprintln!("跳过：本机档位 {:?} 无法强制上限", info.cgroup_mode);
        return;
    }

    let mut request = exec_request(
        "python3 -c 'buf=[]\nwhile True: buf.append(bytearray(4*1024*1024))' 2>/dev/null",
    );
    request.limits = Some(Limits {
        memory_max_bytes: Some(32 * 1024 * 1024),
        cpu_quota_percent: None,
        pids_max: None,
    });
    request.timeout_ms = 30_000;

    let (result, _) = session.exec(request);
    let OpResult::Exec(exec) = result else {
        panic!("{result:?}")
    };
    // 资源问题必须和"命令自己失败"区分开：run 的终态一个是
    // resource_exceeded，一个是 failed
    assert!(exec.oom_killed, "命中内存上限却没被识别成 OOM：{exec:?}");
    assert!(!exec.timed_out);
}

#[test]
fn a_hung_command_is_killed_at_the_deadline_along_with_its_children() {
    let mut session = Session::start(&["/tmp"]);
    session.hello();

    let marker = format!("ai-task-hang-{}", std::process::id());
    let mut request = exec_request(&format!("sh -c 'sleep 300 # {marker}' & sleep 300"));
    request.timeout_ms = 2_000;

    let started = std::time::Instant::now();
    let (result, _) = session.exec(request);
    let elapsed = started.elapsed();

    let OpResult::Exec(exec) = result else {
        panic!("{result:?}")
    };
    assert!(exec.timed_out);
    assert!(
        elapsed < Duration::from_secs(15),
        "超时后收敛太慢：{elapsed:?}"
    );

    // 只杀直接子进程是不够的：孙子进程会活下来继续占目标机的资源
    std::thread::sleep(Duration::from_millis(500));
    let survivors = Command::new("pgrep")
        .args(["-f", &marker])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).lines().count())
        .unwrap_or(0);
    assert_eq!(survivors, 0, "超时后还有 {survivors} 个孙子进程活着");
}

#[test]
fn file_operations_stay_inside_the_declared_roots() {
    let dir = std::env::temp_dir().join(format!("ai-task-agent-roots-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("建目录");
    std::fs::write(dir.join("inside.txt"), "ok").expect("写文件");

    let mut session = Session::start(&[&dir.display().to_string()]);
    session.hello();

    // 根目录内：能读
    let id = session.send(Request::Read {
        path: dir.join("inside.txt").display().to_string(),
        max_bytes: 1024,
    });
    let (result, _) = session.collect(id);
    let OpResult::Read { content, .. } = result else {
        panic!("{result:?}")
    };
    assert_eq!(content, "ok");

    // 根目录外：一律拒绝
    for escape in [
        "/etc/passwd".to_string(),
        format!("{}/../../../etc/passwd", dir.display()),
    ] {
        let id = session.send(Request::Read {
            path: escape.clone(),
            max_bytes: 1024,
        });
        let (result, _) = session.collect(id);
        assert!(
            matches!(result, OpResult::Error { .. }),
            "{escape} 应当被拒：{result:?}"
        );
    }

    // 写也一样
    let id = session.send(Request::Write {
        path: "/tmp/ai-task-should-not-exist.txt".into(),
        content: "x".into(),
    });
    let (result, _) = session.collect(id);
    assert!(matches!(result, OpResult::Error { .. }), "{result:?}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_malformed_request_gets_an_answer_instead_of_silence() {
    // 不回的话中心节点会一直等到超时
    let mut session = Session::start(&["/tmp"]);
    writeln!(session.stdin, "this is not json").expect("写入");
    session.stdin.flush().expect("flush");

    let (result, _) = session.collect(0);
    assert!(matches!(result, OpResult::Error { .. }), "{result:?}");

    // 而且会话还能继续用
    session.hello();
    let (result, _) = session.exec(exec_request("echo still-alive"));
    assert!(matches!(result, OpResult::Exec(_)));
}
