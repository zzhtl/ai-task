//! 资源归因。
//!
//! 「监控对服务器的影响」的正确答案不是看机器级 CPU 曲线，是**归因**：
//! 这次执行在这台机器上花了多少 CPU、峰值内存多少、拉起了几个进程。
//! 唯一能做到这一点的机制是把子进程放进一个自己的 cgroup。
//!
//! 三个档位，能力依次下降，**降级必须如实上报**——谎称限额在生效比不限更糟。

use std::path::{Path, PathBuf};
use std::process::Command;

use ai_task_agent::protocol::{CgroupMode, Limits, Sample};

/// 探测这台机器能用哪一档。
#[must_use]
pub fn detect() -> (CgroupMode, Option<String>) {
    if !Path::new("/proc/self/stat").exists() {
        return (
            CgroupMode::None,
            Some("/proc 不可用，无法做任何资源归因".into()),
        );
    }

    if !Path::new("/sys/fs/cgroup/cgroup.controllers").exists() {
        return (
            CgroupMode::Proc,
            Some("内核未挂载 cgroup v2，只能按进程树记账，无法强制上限".into()),
        );
    }

    // 光有 systemd-run 不够，还得真能建出 scope 来：没有会话总线、
    // 或者控制器没委派给这个用户时，命令存在但会失败。
    match probe_scope() {
        Ok(()) => (CgroupMode::Systemd, None),
        Err(why) => (
            CgroupMode::Proc,
            Some(format!(
                "无法创建 systemd scope（{why}），只能按进程树记账，无法强制上限"
            )),
        ),
    }
}

/// 真建一个空 scope 试试。
fn probe_scope() -> Result<(), String> {
    let unit = format!("ai-task-probe-{}", std::process::id());
    let output = Command::new("systemd-run")
        .args([
            "--user",
            "--scope",
            "--quiet",
            "--collect",
            &format!("--unit={unit}"),
            "-p",
            "MemoryAccounting=yes",
            "--",
            "true",
        ])
        .output()
        .map_err(|e| format!("systemd-run 起不来：{e}"))?;

    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr)
            .lines()
            .next()
            .unwrap_or("未知原因")
            .trim()
            .to_string())
    }
}

/// 一次执行所处的资源容器。
pub struct Scope {
    /// systemd 档位下 scope 的 cgroup 目录。
    cgroup_path: Option<PathBuf>,
    /// systemd 档位下的 unit 名，用来精确停掉整个 scope。
    unit: Option<String>,
    /// proc 档位下的根进程 pid。
    root_pid: Option<u32>,
    peak_rss: u64,
}

impl Scope {
    /// 把命令包装成实际要执行的 argv。
    ///
    /// systemd 档位下外面套一层 `systemd-run --scope`；其它档位原样执行。
    ///
    /// 命令一律交给 `sh -c`——注意在 Debian/Ubuntu 上那是 **dash 不是 bash**，
    /// `$SECONDS`、`[[ ]]`、数组这些 bash 扩展都不可用。要用就显式写 `bash -c`。
    #[must_use]
    pub fn wrap(
        mode: CgroupMode,
        unit: &str,
        limits: Option<Limits>,
        command: &str,
    ) -> Vec<String> {
        if mode != CgroupMode::Systemd {
            return vec!["sh".into(), "-c".into(), command.into()];
        }

        let mut args: Vec<String> = vec![
            "systemd-run".into(),
            "--user".into(),
            "--scope".into(),
            "--quiet".into(),
            // 不加 --collect：进程死后 unit 要留一会儿，好读到 Result=oom-kill。
            // 读完由 reset_failed 清掉。
            format!("--unit={unit}"),
            "-p".into(),
            "CPUAccounting=yes".into(),
            "-p".into(),
            "MemoryAccounting=yes".into(),
            "-p".into(),
            "TasksAccounting=yes".into(),
        ];

        if let Some(limits) = limits {
            if let Some(bytes) = limits.memory_max_bytes {
                args.push("-p".into());
                args.push(format!("MemoryMax={bytes}"));
                // 不关掉 swap 的话，超限会先去换页，"限额"就成了"变慢"
                args.push("-p".into());
                args.push("MemorySwapMax=0".into());
            }
            if let Some(percent) = limits.cpu_quota_percent {
                args.push("-p".into());
                args.push(format!("CPUQuota={percent}%"));
            }
            if let Some(pids) = limits.pids_max {
                args.push("-p".into());
                args.push(format!("TasksMax={pids}"));
            }
        }

        args.push("--".into());
        args.push("sh".into());
        args.push("-c".into());
        args.push(command.into());
        args
    }

    /// 命令起来之后绑定到它的资源容器上。
    #[must_use]
    pub fn attach(mode: CgroupMode, unit: &str, child_pid: u32) -> Self {
        let (cgroup_path, unit_name) = if mode == CgroupMode::Systemd {
            (resolve_cgroup(unit), Some(format!("{unit}.scope")))
        } else {
            (None, None)
        };
        Self {
            cgroup_path,
            unit: unit_name,
            root_pid: (mode != CgroupMode::None).then_some(child_pid),
            peak_rss: 0,
        }
    }

    /// 采一次样。
    pub fn sample(&mut self) -> Sample {
        let mut sample = match (&self.cgroup_path, self.root_pid) {
            (Some(path), _) => sample_cgroup(path),
            (None, Some(pid)) => sample_proc_tree(pid),
            _ => Sample::default(),
        };
        sample.at_ms = now_ms();
        // cgroup 有 memory.peak，进程树没有——后者靠我们自己记高水位
        self.peak_rss = self
            .peak_rss
            .max(sample.rss_bytes)
            .max(sample.peak_rss_bytes);
        sample.peak_rss_bytes = self.peak_rss;
        sample
    }

    /// 命中内存上限被内核杀掉了吗。
    ///
    /// 两个来源：cgroup 的 `memory.events` 计数，以及 systemd 记的 unit 结果。
    /// 进程可能在两次采样之间就死了，那时 cgroup 已经没了，只有后者还能作数。
    #[must_use]
    pub fn was_oom_killed(&self) -> bool {
        if let Some(path) = &self.cgroup_path
            && read_kv(&path.join("memory.events"), "oom_kill").is_some_and(|n| n > 0)
        {
            return true;
        }
        let Some(unit) = &self.unit else {
            return false;
        };
        Command::new("systemctl")
            .args(["--user", "show", unit, "-p", "Result", "--value"])
            .output()
            .ok()
            .is_some_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "oom-kill")
    }

    /// 结束整个容器里的所有进程。
    ///
    /// 只 kill 直接子进程是不够的：`sh -c` 拉起来的孙子进程会活下来，
    /// 变成没人管的孤儿，还继续占着目标机的资源。
    pub fn kill_all(&self) {
        if let Some(unit) = &self.unit {
            let _ = Command::new("systemctl")
                .args(["--user", "stop", unit])
                .output();
            return;
        }
        if let Some(pid) = self.root_pid {
            // 没有 cgroup 时只能尽力而为：杀进程组
            let _ = Command::new("kill")
                .args(["-KILL", &format!("-{pid}"), &pid.to_string()])
                .output();
        }
    }

    /// 读完结果后清掉残留的 unit，免得越攒越多。
    pub fn cleanup(&self) {
        if let Some(unit) = &self.unit {
            let _ = Command::new("systemctl")
                .args(["--user", "reset-failed", unit])
                .output();
        }
    }
}

/// 问 systemd 要 scope 的 cgroup 路径。
///
/// 不自己拼路径：slice 布局取决于机器配置，拼出来的路径在别人的机器上会是错的。
fn resolve_cgroup(unit: &str) -> Option<PathBuf> {
    // scope 是异步创建的，起来之前问会拿到空值
    for _ in 0..50 {
        let output = Command::new("systemctl")
            .args([
                "--user",
                "show",
                &format!("{unit}.scope"),
                "-p",
                "ControlGroup",
                "--value",
            ])
            .output()
            .ok()?;
        let relative = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !relative.is_empty() && relative != "/" {
            let path = PathBuf::from("/sys/fs/cgroup").join(relative.trim_start_matches('/'));
            if path.exists() {
                return Some(path);
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    None
}

fn sample_cgroup(path: &Path) -> Sample {
    Sample {
        at_ms: 0,
        cpu_usec: read_kv(&path.join("cpu.stat"), "usage_usec").unwrap_or(0),
        rss_bytes: read_u64(&path.join("memory.current")).unwrap_or(0),
        peak_rss_bytes: read_u64(&path.join("memory.peak")).unwrap_or(0),
        pids: read_u64(&path.join("pids.current")).unwrap_or(0) as u32,
    }
}

/// 沿 ppid 走一遍进程树。
///
/// CPU 同时算两部分：树上还活着的进程的 `utime+stime`，加上它们已经**回收掉**
/// 的子进程的 `cutime+cstime`。只算前者会严重漏计——一个
/// `while ...; do $(date); done` 循环每轮都 fork 一个短命进程，它们在两次采样
/// 之间就死了，不算 cutime 的话这些 CPU 全部丢失（实测漏掉约一半）。
///
/// 不会重复计算：子进程活着时出现在树里，被回收后它的时间才滚进父进程的
/// cutime，两者互斥。
///
/// 比 cgroup 仍然差一点：进程一旦 reparent 到 init 就跟丢了，而且
/// **完全没法设限**。
fn sample_proc_tree(root: u32) -> Sample {
    let ticks = clock_ticks();
    let page = page_size();
    let mut pids = vec![root];
    let mut seen = vec![root];
    let mut cpu_ticks = 0u64;
    let mut rss_pages = 0u64;

    // 先扫一遍 /proc 建 ppid → 子进程 的映射，避免对每个 pid 重扫
    let mut children: Vec<(u32, u32)> = Vec::new();
    if let Ok(entries) = std::fs::read_dir("/proc") {
        for entry in entries.flatten() {
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|s| s.parse::<u32>().ok())
            else {
                continue;
            };
            if let Some(stat) = read_proc_stat(pid) {
                children.push((stat.ppid, pid));
            }
        }
    }

    while let Some(pid) = pids.pop() {
        if let Some(stat) = read_proc_stat(pid) {
            cpu_ticks += stat.utime + stat.stime + stat.cutime + stat.cstime;
            rss_pages += read_proc_rss(pid).unwrap_or(0);
        }
        for (ppid, child) in &children {
            if *ppid == pid && !seen.contains(child) {
                seen.push(*child);
                pids.push(*child);
            }
        }
    }

    let rss = rss_pages.saturating_mul(page);
    Sample {
        at_ms: 0,
        cpu_usec: cpu_ticks.saturating_mul(1_000_000) / ticks.max(1),
        rss_bytes: rss,
        peak_rss_bytes: rss,
        pids: u32::try_from(seen.len()).unwrap_or(u32::MAX),
    }
}

/// `/proc/<pid>/stat` 里我们关心的那几个字段。
struct ProcStat {
    ppid: u32,
    utime: u64,
    stime: u64,
    /// 已回收子进程的用户态时间。
    cutime: u64,
    /// 已回收子进程的内核态时间。
    cstime: u64,
}

fn read_proc_stat(pid: u32) -> Option<ProcStat> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // 进程名里可能有空格和括号，必须从最后一个 ')' 之后开始切
    let rest = text.rsplit_once(')')?.1;
    let fields: Vec<&str> = rest.split_whitespace().collect();
    // rest 的第一个字段是 state，后面的下标都相对它算
    Some(ProcStat {
        ppid: fields.get(1)?.parse().ok()?,
        utime: fields.get(11)?.parse().ok()?,
        stime: fields.get(12)?.parse().ok()?,
        cutime: fields.get(13)?.parse().ok()?,
        cstime: fields.get(14)?.parse().ok()?,
    })
}
fn read_proc_rss(pid: u32) -> Option<u64> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/statm")).ok()?;
    text.split_whitespace().nth(1)?.parse().ok()
}

fn read_u64(path: &Path) -> Option<u64> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// 从 `key value` 形式的文件里取一个值。
fn read_kv(path: &Path, key: &str) -> Option<u64> {
    let text = std::fs::read_to_string(path).ok()?;
    text.lines()
        .find_map(|line| line.strip_prefix(key)?.trim().parse().ok())
}

fn clock_ticks() -> u64 {
    // 所有主流 Linux 都是 100。拿不到就用它，误差只影响 proc 降级档的 CPU 读数。
    100
}

fn page_size() -> u64 {
    4096
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_systemd_wrapper_carries_every_limit_it_was_given() {
        let args = Scope::wrap(
            CgroupMode::Systemd,
            "ai-task-run-1",
            Some(Limits {
                memory_max_bytes: Some(512 * 1024 * 1024),
                cpu_quota_percent: Some(200),
                pids_max: Some(64),
            }),
            "stress --cpu 4",
        );
        let joined = args.join(" ");
        assert!(joined.contains("MemoryMax=536870912"), "{joined}");
        assert!(joined.contains("CPUQuota=200%"), "{joined}");
        assert!(joined.contains("TasksMax=64"), "{joined}");
        // 不关 swap 的话超限会先换页，"限额"就成了"变慢"
        assert!(joined.contains("MemorySwapMax=0"), "{joined}");
        // 命令原样作为一个参数交给 sh -c，不拼进 shell 字符串
        assert_eq!(args.last().map(String::as_str), Some("stress --cpu 4"));
    }

    #[test]
    fn accounting_is_on_even_without_limits() {
        // 不设限也要记账——归因是这层的主要目的，限额只是附带
        let args = Scope::wrap(CgroupMode::Systemd, "u", None, "ls");
        let joined = args.join(" ");
        assert!(joined.contains("CPUAccounting=yes"));
        assert!(joined.contains("MemoryAccounting=yes"));
        assert!(!joined.contains("MemoryMax"));
    }

    #[test]
    fn degraded_modes_run_the_command_directly() {
        for mode in [CgroupMode::Proc, CgroupMode::None] {
            let args = Scope::wrap(mode, "u", None, "ls -la");
            assert_eq!(args, vec!["sh", "-c", "ls -la"], "{mode:?}");
        }
    }

    #[test]
    fn a_command_with_shell_metacharacters_stays_one_argument() {
        // 命令是操作者写的，但仍然不该被二次解析——那会让引号和 $ 的行为
        // 取决于我们拼字符串的方式
        let command = r#"echo "a b" && ls '$HOME'"#;
        let args = Scope::wrap(CgroupMode::Systemd, "u", None, command);
        assert_eq!(args.last().map(String::as_str), Some(command));
        assert_eq!(args.iter().filter(|a| a.as_str() == command).count(), 1);
    }

    #[test]
    fn proc_stat_parsing_survives_a_process_name_with_spaces_and_parens() {
        // /proc/<pid>/stat 的进程名字段可以含空格和括号，
        // 按空格切会把后面的字段全错位
        let line = "1234 (my (weird) proc) S 42 1 1 0 -1 0 0 0 0 0 111 222 33 44";
        let rest = line.rsplit_once(')').expect("有右括号").1;
        let fields: Vec<&str> = rest.split_whitespace().collect();
        assert_eq!(fields.first(), Some(&"S"));
        assert_eq!(fields.get(1), Some(&"42"), "ppid");
        assert_eq!(fields.get(11), Some(&"111"), "utime");
        assert_eq!(fields.get(12), Some(&"222"), "stime");
        // 漏掉这两个的话，快进快出的子进程的 CPU 会整块丢失
        assert_eq!(fields.get(13), Some(&"33"), "cutime");
        assert_eq!(fields.get(14), Some(&"44"), "cstime");
    }

    #[test]
    fn reaped_children_cpu_is_counted_in_proc_mode() {
        // 起一堆快进快出的子进程，它们在采样间隙就死了。
        // 只算树上活着的进程会把这些 CPU 全丢掉。
        let mut scope = Scope::attach(CgroupMode::Proc, "unused", std::process::id());
        let before = scope.sample().cpu_usec;
        for _ in 0..30 {
            let _ = Command::new("true").status();
        }
        let after = scope.sample().cpu_usec;
        assert!(
            after > before,
            "回收掉的子进程也要计入 CPU（{before} → {after}）"
        );
    }

    #[test]
    fn sampling_this_process_yields_plausible_numbers() {
        let mut scope = Scope::attach(CgroupMode::Proc, "unused", std::process::id());
        let sample = scope.sample();
        assert!(sample.rss_bytes > 0, "自己的常驻内存不该是 0");
        assert!(sample.pids >= 1);
        assert!(sample.at_ms > 1_700_000_000_000, "时间戳应当是 Unix 毫秒");
    }

    #[test]
    fn peak_rss_is_a_high_water_mark() {
        let mut scope = Scope::attach(CgroupMode::Proc, "unused", std::process::id());
        let first = scope.sample();
        let second = scope.sample();
        assert!(second.peak_rss_bytes >= first.rss_bytes, "峰值只能涨不能落");
    }
}
