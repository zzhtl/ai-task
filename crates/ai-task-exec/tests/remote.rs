//! 针对真实 SSH 目标机的远端执行测试。
//!
//! 需要三个环境变量指向一台可 SSH 的机器，没设就跳过：
//!
//! ```sh
//! AI_TASK_TEST_SSH_HOST=127.0.0.1 \
//! AI_TASK_TEST_SSH_PORT=2222 \
//! AI_TASK_TEST_SSH_USER=runner \
//! AI_TASK_TEST_SSH_KEY=/path/to/id_ed25519 \
//!   cargo test -p ai-task-exec --test remote
//! ```
//!
//! 本地用 `deploy/test-target.sh` 起一个 Alpine 容器：它**没有 systemd**，
//! 正好覆盖资源归因降级到 `/proc` 的那条路径。

use std::path::PathBuf;

use ai_task_agent::protocol::{CgroupMode, ExecRequest, Limits};
use ai_task_exec::remote::{
    AgentEvent, HostKeyPolicy, RemoteAgent, SshConfig, SshSession, sha256_hex,
};

/// 推送用的 agent 二进制。跑测试前先编：
/// `cargo build -p ai-task-agent --bin ai-task-agent --profile agent-release
///  --target x86_64-unknown-linux-musl`
fn agent_binary() -> Option<Vec<u8>> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/x86_64-unknown-linux-musl/agent-release/ai-task-agent");
    std::fs::read(path).ok()
}

fn config() -> Option<SshConfig> {
    Some(SshConfig {
        host: std::env::var("AI_TASK_TEST_SSH_HOST").ok()?,
        port: std::env::var("AI_TASK_TEST_SSH_PORT").ok()?.parse().ok()?,
        user: std::env::var("AI_TASK_TEST_SSH_USER").ok()?,
        key_path: PathBuf::from(std::env::var("AI_TASK_TEST_SSH_KEY").ok()?),
        key_passphrase: None,
        known_hosts: std::env::temp_dir().join("ai-task-test-known-hosts"),
        policy: HostKeyPolicy::AcceptNew,
        connect_timeout: std::time::Duration::from_secs(10),
    })
}

/// 连上目标机并把 agent 投送好。
async fn connect() -> Option<(SshSession, String, Vec<u8>)> {
    let config = config()?;
    let binary = match agent_binary() {
        Some(binary) => binary,
        None => {
            eprintln!(
                "跳过：musl agent 二进制不存在，先 cargo build --target x86_64-unknown-linux-musl"
            );
            return None;
        }
    };
    let session = SshSession::connect(&config).await.expect("连接目标机");
    let sha = sha256_hex(&binary);
    let deployed = session
        .ensure_agent(&binary, &sha, "/tmp")
        .await
        .expect("投送 agent");
    Some((session, deployed.path, binary))
}

macro_rules! remote_test {
    ($name:ident, |$session:ident, $agent_path:ident| $body:block) => {
        #[tokio::test]
        async fn $name() {
            let Some(($session, $agent_path, _binary)) = connect().await else {
                eprintln!("跳过 {}：未配置 AI_TASK_TEST_SSH_*", stringify!($name));
                return;
            };
            $body
        }
    };
}

// ---------------------------------------------------------------- 测试

#[test]
fn the_agent_hash_matches_the_sha256_reference_vectors() {
    // 目标机用 sha256sum 校验，本地算错的话每次都会"哈希不匹配"
    assert_eq!(
        sha256_hex(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    // 跨块边界（> 64 字节）
    assert_eq!(
        sha256_hex(&b"a".repeat(1000)),
        "41edece42d63e8d9bf515a9ba6932e1c20cbc9f5a5d134645adb5db1b9737ea3"
    );
}

remote_test!(deploying_the_agent_is_idempotent, |session, agent_path| {
    // 已经在目标机上且哈希一致就不该重传——同一台机器跑第二个 run 时
    // 每次都传 500KB 是纯浪费
    let binary = agent_binary().expect("二进制");
    let sha = sha256_hex(&binary);
    let again = session
        .ensure_agent(&binary, &sha, "/tmp")
        .await
        .expect("再次投送");
    assert!(!again.uploaded, "第二次不该重传");
    assert_eq!(again.path, agent_path);
});

remote_test!(
    the_target_has_no_node_and_no_api_key,
    |session, _agent_path| {
        // 这是「远端形态」这个决策的核心承诺：目标机零依赖
        let probe = session
            .run("command -v node claude npm 2>/dev/null | wc -l; env | grep -ci ANTHROPIC || true")
            .await
            .expect("探测");
        let lines: Vec<&str> = probe.stdout.split_whitespace().collect();
        assert_eq!(lines.first(), Some(&"0"), "目标机上不该有 node/claude/npm");
        assert_eq!(
            lines.get(1),
            Some(&"0"),
            "目标机上不该有 ANTHROPIC 环境变量"
        );
    }
);

remote_test!(
    a_remote_command_streams_output_and_reports_attribution,
    |session, agent_path| {
        let mut agent = RemoteAgent::start(&session, &agent_path, &["/tmp".into()])
            .await
            .expect("启动 agent");

        let info = agent.info().clone();
        eprintln!(
            "远端归因档位：{:?}（{:?}）",
            info.cgroup_mode, info.cgroup_detail
        );
        // Alpine 容器没有 systemd，必须如实降级并给出原因
        if info.cgroup_mode != CgroupMode::Systemd {
            assert!(info.cgroup_detail.is_some(), "降级了却没说原因");
        }

        let mut stdout = String::new();
        let mut samples = Vec::new();
        let result = agent
        .exec(
            ExecRequest {
                command: "end=$(( $(date +%s) + 3 )); while [ $(date +%s) -lt $end ]; do :; done; echo burned".into(),
                cwd: "/tmp".into(),
                timeout_ms: 30_000,
                limits: None,
                max_output_bytes: 64 * 1024,
            },
            |event| match event {
                AgentEvent::Stdout(data) => stdout.push_str(&data),
                AgentEvent::Metrics(sample) => samples.push(sample),
                AgentEvent::Stderr(_) => {}
            },
        )
        .await
        .expect("远端执行");

        assert_eq!(result.exit_code, Some(0));
        assert!(stdout.contains("burned"), "{stdout}");
        assert!(
            samples.len() >= 2,
            "3 秒的命令至少该采到两个点：{}",
            samples.len()
        );
        assert!(
            result.resource.cpu_usec > 500_000,
            "远端 CPU 归因只有 {} 微秒",
            result.resource.cpu_usec
        );
        agent.shutdown().await.expect("关闭");
    }
);

remote_test!(
    remote_file_operations_stay_inside_the_declared_roots,
    |session, agent_path| {
        let mut agent = RemoteAgent::start(&session, &agent_path, &["/tmp".into()])
            .await
            .expect("启动 agent");

        agent
            .write("/tmp/ai-task-remote-test.txt", "hello from center")
            .await
            .expect("写入");
        let (content, truncated) = agent
            .read("/tmp/ai-task-remote-test.txt", 1024)
            .await
            .expect("读取");
        assert_eq!(content, "hello from center");
        assert!(!truncated);

        // 根目录外一律拒绝
        assert!(agent.read("/etc/passwd", 1024).await.is_err());
        assert!(agent.write("/etc/ai-task-pwn", "x").await.is_err());

        let paths = agent
            .glob("/tmp", "ai-task-remote-*.txt")
            .await
            .expect("glob");
        assert!(
            paths.iter().any(|p| p.ends_with("ai-task-remote-test.txt")),
            "{paths:?}"
        );
        agent.shutdown().await.expect("关闭");
    }
);

remote_test!(
    a_degraded_target_reports_that_limits_are_not_enforced,
    |session, agent_path| {
        // 谎称限额在生效比不限更糟：用户会以为自己配的 MemoryMax 起了作用
        let mut agent = RemoteAgent::start(&session, &agent_path, &["/tmp".into()])
            .await
            .expect("启动 agent");
        let enforces = agent.info().cgroup_mode.enforces_limits();

        let result = agent
            .exec(
                ExecRequest {
                    command: "echo ok".into(),
                    cwd: "/tmp".into(),
                    timeout_ms: 10_000,
                    limits: Some(Limits {
                        memory_max_bytes: Some(32 * 1024 * 1024),
                        cpu_quota_percent: Some(100),
                        pids_max: Some(32),
                    }),
                    max_output_bytes: 4096,
                },
                |_| {},
            )
            .await
            .expect("执行");

        assert_eq!(result.exit_code, Some(0));
        if !enforces {
            eprintln!(
                "目标机无法强制上限（档位 {:?}），已在 agent 信息里标明",
                agent.info().cgroup_mode
            );
        }
        agent.shutdown().await.expect("关闭");
    }
);

// ---------------------------------------------------------------- 本机 agent

/// 本机也走同一套协议和同一个二进制，所以本机节点同样有资源归因和上限。
/// 这台开发机有 systemd，正好覆盖 docker 容器覆盖不到的那一档。
#[tokio::test]
async fn the_local_agent_enforces_limits_when_systemd_is_available() {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/debug/ai-task-agent");
    if !path.exists() {
        eprintln!("跳过：本机 agent 未构建（cargo build -p ai-task-agent）");
        return;
    }

    let mut agent = ai_task_exec::remote::start_local_agent(&path, &["/tmp".into()])
        .await
        .expect("启动本机 agent");
    let mode = agent.info().cgroup_mode;
    eprintln!("本机归因档位：{mode:?}");

    if !mode.enforces_limits() {
        eprintln!("跳过限额断言：本机档位 {mode:?} 无法强制上限");
        agent.shutdown().await.expect("关闭");
        return;
    }

    let result = agent
        .exec(
            ExecRequest {
                command:
                    "python3 -c 'buf=[]\nwhile True: buf.append(bytearray(4*1024*1024))' 2>/dev/null"
                        .into(),
                cwd: "/tmp".into(),
                timeout_ms: 30_000,
                limits: Some(Limits {
                    memory_max_bytes: Some(32 * 1024 * 1024),
                    cpu_quota_percent: None,
                    pids_max: None,
                }),
                max_output_bytes: 4096,
            },
            |_| {},
        )
        .await
        .expect("执行");

    // 资源问题必须和"命令自己失败"区分开
    assert!(
        result.oom_killed,
        "命中内存上限却没被识别成 OOM：{result:?}"
    );
    agent.shutdown().await.expect("关闭");
}
