//! 真跑一次 `claude` 的端到端测试。
//!
//! **会花钱**（一次约 $0.03），所以默认跳过。开：
//!
//! ```sh
//! AI_TASK_LIVE_CLAUDE=1 cargo test -p ai-task-exec --test live_claude -- --nocapture
//! ```
//!
//! CI 不开这个开关——防腐层的回归由 `decode_fixtures.rs` 用录制样本覆盖，
//! 这里验的是「进程真的能被拉起来、流真的能读回来、取消真的能杀掉它」。

use std::time::Duration;

use ai_task_exec::claude_code::ClaudeCodeExecutor;
use ai_task_exec::{ExecEvent, ExecOutcome, ExecRequest, Executor};
use ai_task_proto::{NodeKey, UsdMicros};

fn enabled() -> bool {
    matches!(
        std::env::var("AI_TASK_LIVE_CLAUDE").as_deref(),
        Ok("1") | Ok("true")
    )
}

fn request(dir: &std::path::Path, prompt: &str) -> ExecRequest {
    ExecRequest {
        node_key: NodeKey("probe".into()),
        prompt: prompt.into(),
        workdir: dir.to_path_buf(),
        model: Some("claude-haiku-4-5".into()),
        effort: None,
        tools: Some(vec!["Bash".into(), "Read".into(), "Glob".into()]),
        output_schema: None,
        // 兜底闸门：测试跑飞了也不至于烧掉一笔钱
        budget_usd: Some(UsdMicros(200_000)),
        system_append: None,
        session_id: uuid::Uuid::now_v7(),
        settings_path: None,
        mcp_config: None,
        has_skills: false,
    }
}

fn workdir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("临时目录");
    std::fs::write(dir.path().join("a.rs"), "fn main() {}\n").expect("写 a.rs");
    std::fs::write(dir.path().join("b.rs"), "pub fn x() {}\npub fn y() {}\n").expect("写 b.rs");
    dir
}

#[tokio::test]
async fn drives_a_real_claude_process_end_to_end() {
    if !enabled() {
        eprintln!("跳过：未设置 AI_TASK_LIVE_CLAUDE=1（这个测试会真的调用 API）");
        return;
    }

    let dir = workdir();
    let handle = ClaudeCodeExecutor::default()
        .spawn(request(
            dir.path(),
            "当前目录下所有 .rs 文件一共多少行？只回答数字。",
        ))
        .await
        .expect("拉起 claude");

    let mut events = handle.events;
    let mut collected = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(180);
    while let Ok(Some(event)) = tokio::time::timeout_at(deadline, events.recv()).await {
        let terminal = event.is_terminal();
        collected.push(event);
        if terminal {
            break;
        }
    }

    let first = collected.first().expect("至少要有一条事件");
    let ExecEvent::Started { tools, .. } = first else {
        panic!("第一条必须是 Started：{first:?}");
    };
    assert_eq!(
        tools.len(),
        3,
        "密闭执行没生效，实际拿到 {} 个工具：{tools:?}",
        tools.len()
    );

    let last = collected.last().expect("至少要有终态");
    let ExecEvent::Finished(outcome) = last else {
        panic!("最后一条必须是 Finished：{last:?}");
    };
    assert!(outcome.is_success(), "执行失败：{outcome:?}");

    let cost = collected
        .iter()
        .find_map(|e| match e {
            ExecEvent::Usage { cost_usd, .. } => Some(*cost_usd),
            _ => None,
        })
        .expect("必须报出成本");
    assert!(cost.0 > 0, "成本不该是 0");
    eprintln!("本次实跑成本 ${cost}，共 {} 条事件", collected.len());
}

#[tokio::test]
async fn cancellation_kills_the_child_process() {
    if !enabled() {
        eprintln!("跳过：未设置 AI_TASK_LIVE_CLAUDE=1");
        return;
    }

    let dir = workdir();
    let handle = ClaudeCodeExecutor::default()
        .spawn(request(
            dir.path(),
            "慢慢地、逐个文件地分析这个目录，越详细越好。",
        ))
        .await
        .expect("拉起 claude");

    let mut events = handle.events;
    // 等到真正开始跑再取消，否则测的是"还没起来就被杀"
    let started = tokio::time::timeout(Duration::from_secs(60), events.recv())
        .await
        .expect("等待 Started 超时");
    assert!(matches!(started, Some(ExecEvent::Started { .. })));

    handle.cancel.cancel();

    // 取消后必须很快收敛，否则 CLI 会在后台继续烧钱
    let mut last = None;
    while let Ok(Some(event)) = tokio::time::timeout(Duration::from_secs(20), events.recv()).await {
        let terminal = event.is_terminal();
        last = Some(event);
        if terminal {
            break;
        }
    }
    assert_eq!(
        last,
        Some(ExecEvent::Finished(ExecOutcome::Cancelled)),
        "取消后应当收到 Cancelled 终态"
    );
}

#[tokio::test]
async fn a_missing_binary_is_a_clear_error_not_a_hang() {
    let dir = workdir();
    let err = ClaudeCodeExecutor::new("definitely-not-a-real-binary-xyz")
        .spawn(request(dir.path(), "hi"))
        .await
        .expect_err("不存在的可执行文件必须立刻报错");
    let message = err.to_string();
    assert!(
        message.contains("PATH"),
        "错误信息要告诉人怎么办：{message}"
    );
}

#[tokio::test]
async fn a_missing_workdir_is_rejected_before_spawning() {
    let err = ClaudeCodeExecutor::default()
        .spawn(request(std::path::Path::new("/nope/does/not/exist"), "hi"))
        .await
        .expect_err("工作目录不存在必须提前拒绝");
    assert!(err.to_string().contains("不存在"));
}
