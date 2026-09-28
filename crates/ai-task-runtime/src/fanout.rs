//! shell 步骤在多台机器上各跑一次。
//!
//! 编排——并发上限、每台跑完就报一次、重试只重跑失败的那几台、汇总成一个节点结果——
//! 和"在一台机器上跑一条命令"分开：后者是 [`crate::host_exec::run_command`]，要真的连 SSH；
//! 这里是纯逻辑，拿一个假的执行函数就能测。
//!
//! 语义：
//! - **任一台失败，这一步就算失败**，但其余的照常跑完——批量操作停在一半，
//!   比全部跑完再看哪几台出错更难收拾。
//! - **重试只重跑上次没成功的机器。**命令很可能不是幂等的（重启服务、改配置），
//!   让已经成功的机器再跑一遍是在制造事故。

use std::collections::BTreeMap;
use std::future::Future;

use ai_task_proto::{HostId, NodeStatus, UsdMicros};
use futures_util::StreamExt;

use crate::dag::NodeResult;

/// 同一个步骤同时在几台机器上跑。每台一条 SSH 连接加一个 agent 进程，
/// 再多中心这边的连接数和目标机那边的瞬时压力都会上来。
pub const CONCURRENCY: usize = 8;

/// 每台机器的输出进事件和节点结果时保留的末尾长度。
/// 一百台乘两路输出，再大事件日志和下游步骤的输入都会被撑爆。
pub const OUTPUT_TAIL_BYTES: usize = 4 * 1024;

/// 要跑的一台机器。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub host_id: HostId,
    pub name: String,
    /// 解析时就发现不在了（勾选的主机后来被删了）：不去连，直接记失败。
    pub missing: bool,
}

/// 一台机器上的结局。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostOutcome {
    pub ok: bool,
    pub exit_code: Option<i32>,
    pub duration_ms: u64,
    pub error: Option<String>,
    pub stdout: String,
    pub stderr: String,
    pub cgroup_mode: Option<String>,
    /// 被资源上限（内存）杀掉。整个 run 因此落 `resource_exceeded`。
    pub resource_exceeded: bool,
}

impl HostOutcome {
    #[must_use]
    pub fn failed(error: impl Into<String>, duration_ms: u64) -> Self {
        Self {
            ok: false,
            exit_code: None,
            duration_ms,
            error: Some(error.into()),
            stdout: String::new(),
            stderr: String::new(),
            cgroup_mode: None,
            resource_exceeded: false,
        }
    }
}

/// 这一轮要跑的机器：上一轮已经成功的不再跑。
#[must_use]
pub fn pending(targets: &[Target], done: &BTreeMap<HostId, HostOutcome>) -> Vec<Target> {
    targets
        .iter()
        .filter(|t| !done.get(&t.host_id).is_some_and(|o| o.ok))
        .cloned()
        .collect()
}

/// 在并发上限内逐台执行，每台跑完就回调一次（发事件用），返回这一轮的全部结局。
pub async fn execute<F, Fut, C, CFut>(
    targets: Vec<Target>,
    limit: usize,
    exec: F,
    mut on_done: C,
) -> BTreeMap<HostId, HostOutcome>
where
    F: Fn(Target) -> Fut,
    Fut: Future<Output = HostOutcome>,
    C: FnMut(Target, HostOutcome) -> CFut,
    CFut: Future<Output = HostOutcome>,
{
    let mut results = BTreeMap::new();
    let mut running = futures_util::stream::iter(targets.into_iter().map(|target| {
        let fut = exec(target.clone());
        async move { (target, fut.await) }
    }))
    .buffer_unordered(limit.max(1));
    while let Some((target, outcome)) = running.next().await {
        let host_id = target.host_id;
        let outcome = on_done(target, outcome).await;
        results.insert(host_id, outcome);
    }
    results
}

/// 把所有机器的结局汇总成一个节点结果。
///
/// 输出不管成败都带上：下游步骤（断言、AI 汇总）和界面都要按台看结果，
/// 失败时尤其要知道是哪几台、为什么。
#[must_use]
pub fn summarize(targets: &[Target], outcomes: &BTreeMap<HostId, HostOutcome>) -> NodeResult {
    let hosts: Vec<serde_json::Value> = targets
        .iter()
        .map(|t| {
            let o = outcomes.get(&t.host_id);
            serde_json::json!({
                "host_id": t.host_id,
                "host": t.name,
                "ok": o.is_some_and(|o| o.ok),
                "exit_code": o.and_then(|o| o.exit_code),
                "stdout": o.map_or("", |o| o.stdout.as_str()),
                "stderr": o.map_or("", |o| o.stderr.as_str()),
                "error": o.map_or(Some("没有跑到"), |o| o.error.as_deref()),
            })
        })
        .collect();
    let failed: Vec<&Target> = targets
        .iter()
        .filter(|t| !outcomes.get(&t.host_id).is_some_and(|o| o.ok))
        .collect();
    let output = serde_json::json!({
        "total": targets.len(),
        "failed": failed.len(),
        "hosts": hosts,
    });

    if failed.is_empty() {
        return NodeResult::succeeded(Some(output));
    }
    // 错误里点名前三台：一眼能看出是个别机器的问题还是命令本身写错了
    let named: Vec<String> = failed
        .iter()
        .take(3)
        .map(|t| {
            let why = outcomes
                .get(&t.host_id)
                .and_then(|o| o.error.clone())
                .unwrap_or_else(|| "没有跑到".into());
            format!("{}（{why}）", t.name)
        })
        .collect();
    let more = if failed.len() > 3 {
        format!(" 等 {} 台", failed.len())
    } else {
        String::new()
    };
    NodeResult {
        status: NodeStatus::Failed,
        output: Some(output),
        error: Some(format!(
            "{} 台中有 {} 台失败：{}{more}",
            targets.len(),
            failed.len(),
            named.join("、")
        )),
        cost: UsdMicros::ZERO,
        cli_version: None,
    }
}

/// 保留末尾 `max_bytes` 字节。截了就在开头说明，不能让人以为看到的是全部。
#[must_use]
pub fn tail(text: &str, max_bytes: usize) -> String {
    let text = text.trim_end();
    if text.len() <= max_bytes {
        return text.to_owned();
    }
    let mut at = text.len() - max_bytes;
    // 中文输出很常见，不能从多字节字符中间切
    while !text.is_char_boundary(at) {
        at += 1;
    }
    format!("…（前面省略 {at} 字节）\n{}", &text[at..])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn target(name: &str) -> Target {
        Target {
            host_id: HostId::new(),
            name: name.into(),
            missing: false,
        }
    }

    fn ok() -> HostOutcome {
        HostOutcome {
            ok: true,
            exit_code: Some(0),
            duration_ms: 5,
            error: None,
            stdout: "fine".into(),
            stderr: String::new(),
            cgroup_mode: Some("proc".into()),
            resource_exceeded: false,
        }
    }

    #[test]
    fn a_retry_only_runs_the_hosts_that_did_not_succeed() {
        let targets = vec![target("a"), target("b"), target("c")];
        let mut done = BTreeMap::new();
        done.insert(targets[0].host_id, ok());
        done.insert(targets[1].host_id, HostOutcome::failed("退出码 1", 3));
        let names: Vec<String> = pending(&targets, &done)
            .into_iter()
            .map(|t| t.name)
            .collect();
        assert_eq!(names, ["b", "c"], "a 已经成功过，不能再跑一遍");
    }

    #[tokio::test]
    async fn execution_stays_within_the_concurrency_limit_and_reports_every_host() {
        let targets: Vec<Target> = (0..10).map(|i| target(&format!("h{i}"))).collect();
        let running = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        let reported = AtomicUsize::new(0);
        let outcomes = execute(
            targets.clone(),
            3,
            |_| async {
                let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                running.fetch_sub(1, Ordering::SeqCst);
                ok()
            },
            |_, outcome| {
                reported.fetch_add(1, Ordering::SeqCst);
                async move { outcome }
            },
        )
        .await;
        assert_eq!(outcomes.len(), 10);
        assert_eq!(reported.load(Ordering::SeqCst), 10, "每台跑完都要报一次");
        assert!(
            peak.load(Ordering::SeqCst) <= 3,
            "同时最多 3 台，实际 {}",
            peak.load(Ordering::SeqCst)
        );
    }

    #[test]
    fn one_failure_fails_the_step_but_keeps_every_result() {
        let targets = vec![target("web-1"), target("web-2")];
        let mut outcomes = BTreeMap::new();
        outcomes.insert(targets[0].host_id, ok());
        outcomes.insert(
            targets[1].host_id,
            HostOutcome::failed("命令退出码 1：no space", 7),
        );
        let result = summarize(&targets, &outcomes);
        assert_eq!(result.status, NodeStatus::Failed);
        assert_eq!(
            result.error.as_deref(),
            Some("2 台中有 1 台失败：web-2（命令退出码 1：no space）")
        );
        let output = result.output.expect("失败也要带每台的结果");
        assert_eq!(
            (output["total"].as_u64(), output["failed"].as_u64()),
            (Some(2), Some(1))
        );
        assert_eq!(output["hosts"][0]["stdout"], "fine");
        assert_eq!(output["hosts"][1]["ok"], false);
    }

    #[test]
    fn all_good_is_a_success_with_per_host_output() {
        let targets = vec![target("a")];
        let outcomes = BTreeMap::from([(targets[0].host_id, ok())]);
        let result = summarize(&targets, &outcomes);
        assert_eq!(result.status, NodeStatus::Succeeded);
        assert_eq!(result.output.expect("输出")["hosts"][0]["host"], "a");
    }

    #[test]
    fn many_failures_name_the_first_three() {
        let targets: Vec<Target> = (1..=5).map(|i| target(&format!("h{i}"))).collect();
        let outcomes: BTreeMap<_, _> = targets
            .iter()
            .map(|t| (t.host_id, HostOutcome::failed("连不上", 1)))
            .collect();
        let error = summarize(&targets, &outcomes).error.expect("错误");
        assert_eq!(
            error,
            "5 台中有 5 台失败：h1（连不上）、h2（连不上）、h3（连不上） 等 5 台"
        );
    }

    #[test]
    fn tail_keeps_the_end_and_says_what_it_dropped() {
        assert_eq!(tail("short\n", 10), "short");
        let long = "磁盘".repeat(10); // 60 字节
        let cut = tail(&long, 7);
        assert!(cut.starts_with("…（前面省略 "), "{cut}");
        assert!(cut.ends_with("磁盘"), "不能从多字节字符中间切：{cut}");
    }
}
