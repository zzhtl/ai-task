//! Run 工作目录的准备。
//!
//! 每个 run 一个独立目录，里面放两样东西：
//!
//! - `.claude/skills/<name>/SKILL.md` —— 勾选的技能包。Claude Code 自己会做
//!   渐进式披露（先只读 frontmatter 的 description，用到了才读正文），
//!   所以这块我们零成本拿到。
//! - `.claude/hook-settings.json` —— `PreToolUse` hook 的配置。策略拦截靠它。
//!
//! 目录随 run 走：不复用、不共享。两个 run 共用工作目录会让它们互相看见
//! 对方的中间文件，那是最难查的一类污染。

use std::path::{Path, PathBuf};

use serde::Serialize;

/// 一个要落地的技能包。
#[derive(Debug, Clone)]
pub struct SkillFiles {
    pub name: String,
    /// frontmatter 里的一句话描述。渐进式披露时只有它进上下文。
    pub description: String,
    pub body: String,
    /// 附带文件：相对 skill 目录的路径 → 内容。
    pub files: Vec<(String, String)>,
}

/// `PreToolUse` hook 的配置。
#[derive(Debug, Clone)]
pub struct McpProxyConfig {
    /// 代理程序的绝对路径（一般是 ai-task 自己）。
    pub program: PathBuf,
    /// 中心的 `/internal/remote/call` 地址。
    pub endpoint: String,
    pub run_id: String,
    /// 目标主机由这个节点的任务定义决定，代理不能自己指定。
    pub node_key: String,
    pub token: String,
}

/// MCP server 在配置里的名字。工具会以 `mcp__<name>__<tool>` 暴露给模型，
/// `--tools` 白名单里要用同样的前缀。
pub const MCP_SERVER_NAME: &str = "ai_task_remote";

/// 远端工具的完整名字（`--tools` 用）。
#[must_use]
pub fn remote_tool_names() -> Vec<String> {
    ["remote_bash", "remote_read", "remote_write", "remote_glob"]
        .iter()
        .map(|tool| format!("mcp__{MCP_SERVER_NAME}__{tool}"))
        .collect()
}

pub struct HookConfig {
    /// hook 程序的绝对路径（一般是 ai-task 自己）。
    pub program: PathBuf,
    /// 判决接口地址。
    pub endpoint: String,
    pub run_id: String,
    /// 调用方身份令牌。没有它，同机上的任何进程都能替 run 批准工具调用。
    pub token: String,
}

#[derive(Debug, thiserror::Error)]
pub enum WorkdirError {
    #[error("准备工作目录 {path} 失败：{source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("技能名 `{0}` 含路径分隔符或上跳，拒绝落地")]
    UnsafeSkillName(String),

    #[error("技能 `{skill}` 的附带文件路径 `{path}` 会跳出技能目录，拒绝落地")]
    UnsafeSkillFile { skill: String, path: String },
}

/// 一个 run 的工作目录。
#[derive(Debug, Clone)]
pub struct RunWorkdir {
    root: PathBuf,
}

impl RunWorkdir {
    /// 创建（或复用）目录。
    pub async fn prepare(root: impl Into<PathBuf>) -> Result<Self, WorkdirError> {
        let root = root.into();
        tokio::fs::create_dir_all(&root)
            .await
            .map_err(|source| WorkdirError::Io {
                path: root.clone(),
                source,
            })?;
        Ok(Self { root })
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.root
    }

    /// 把技能包写进 `.claude/skills/`。返回落地成功的技能名。
    pub async fn write_skills(&self, skills: &[SkillFiles]) -> Result<Vec<String>, WorkdirError> {
        if skills.is_empty() {
            return Ok(Vec::new());
        }
        let skills_root = self.root.join(".claude").join("skills");
        let mut written = Vec::with_capacity(skills.len());

        for skill in skills {
            // 技能名来自数据库，但数据库内容也可能被改。名字要拼进路径，
            // 必须在这里挡住 `../` 和分隔符——不能假设上游一定干净。
            if !is_safe_segment(&skill.name) {
                return Err(WorkdirError::UnsafeSkillName(skill.name.clone()));
            }
            let dir = skills_root.join(&skill.name);
            create_dir(&dir).await?;

            write_file(&dir.join("SKILL.md"), &render_skill_md(skill)).await?;

            for (relative, content) in &skill.files {
                let target = dir.join(relative);
                // canonicalize 之前先做纯路径检查：文件还不存在，canonicalize 会失败
                if !is_within(&dir, &target) {
                    return Err(WorkdirError::UnsafeSkillFile {
                        skill: skill.name.clone(),
                        path: relative.clone(),
                    });
                }
                if let Some(parent) = target.parent() {
                    create_dir(parent).await?;
                }
                write_file(&target, content).await?;
            }
            written.push(skill.name.clone());
        }
        Ok(written)
    }

    /// 写 hook 配置，返回文件路径（传给 `--settings`）。
    /// 写 `--mcp-config` 用的配置，返回文件路径。
    ///
    /// 代理是 Claude Code 拉起的子进程，命令行对模型可见——所以这里只放
    /// 中心地址、run 级令牌和节点名。**主机凭据不在这条命令行上，
    /// 也不在代理进程里。**
    pub async fn write_mcp_config(
        &self,
        node_key: &str,
        proxy: &McpProxyConfig,
    ) -> Result<PathBuf, WorkdirError> {
        let dir = self.root.join(".claude");
        create_dir(&dir).await?;
        // 一个节点一个文件。node key 已经过 DAG 校验（只允许 [A-Za-z0-9_-]），
        // 拼进文件名不会跑出这个目录。
        let path = dir.join(format!("mcp-{node_key}.json"));

        let config = serde_json::json!({
            "mcpServers": {
                MCP_SERVER_NAME: {
                    "command": proxy.program.to_string_lossy(),
                    "args": [
                        "mcp-proxy",
                        "--endpoint", proxy.endpoint,
                        "--run-id", proxy.run_id,
                        "--node-key", proxy.node_key,
                        "--token", proxy.token,
                    ],
                }
            }
        });
        write_file(
            &path,
            &serde_json::to_string_pretty(&config).unwrap_or_default(),
        )
        .await?;
        Ok(path)
    }

    pub async fn write_hook_settings(&self, hook: &HookConfig) -> Result<PathBuf, WorkdirError> {
        let dir = self.root.join(".claude");
        create_dir(&dir).await?;
        let path = dir.join("hook-settings.json");

        let settings = Settings {
            hooks: Hooks {
                // 空 matcher = 匹配所有工具。策略层必须看见每一次调用，
                // 漏掉一个工具就等于给它开了后门。
                pre_tool_use: vec![HookMatcher {
                    matcher: String::new(),
                    hooks: vec![HookCommand {
                        kind: "command",
                        command: format!(
                            "{} policy-hook --endpoint {} --run-id {} --token {}",
                            shell_quote(&hook.program.to_string_lossy()),
                            shell_quote(&hook.endpoint),
                            shell_quote(&hook.run_id),
                            shell_quote(&hook.token),
                        ),
                    }],
                }],
            },
        };

        let json = serde_json::to_string_pretty(&settings).unwrap_or_else(|_| "{}".into());
        write_file(&path, &json).await?;
        Ok(path)
    }
}

/// 渲染成 Claude Code 认识的 SKILL.md（YAML frontmatter + 正文）。
fn render_skill_md(skill: &SkillFiles) -> String {
    format!(
        "---\nname: {}\ndescription: {}\n---\n\n{}\n",
        yaml_scalar(&skill.name),
        yaml_scalar(&skill.description),
        skill.body.trim_end()
    )
}

/// frontmatter 里的标量值。描述里出现冒号、引号、换行都不能把 YAML 撑坏。
fn yaml_scalar(value: &str) -> String {
    let flattened = value.replace(['\n', '\r'], " ");
    format!(
        "\"{}\"",
        flattened.replace('\\', r"\\").replace('"', "\\\"")
    )
}

/// 单个路径段是否安全：非空、不含分隔符、不是 `.` 或 `..`。
fn is_safe_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment != "."
        && segment != ".."
        && !segment.contains(['/', '\\'])
        && !segment.contains('\0')
}

/// `target` 是否落在 `base` 之内。纯路径运算，不碰文件系统——
/// 文件此时还不存在，`canonicalize` 用不了。
fn is_within(base: &Path, target: &Path) -> bool {
    let mut depth = 0i32;
    let Ok(relative) = target.strip_prefix(base) else {
        return false;
    };
    for part in relative.components() {
        match part {
            std::path::Component::ParentDir => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
            }
            std::path::Component::Normal(_) => depth += 1,
            std::path::Component::CurDir => {}
            // 绝对路径、盘符：直接拒
            _ => return false,
        }
    }
    true
}

/// hook 命令是拼成一整行字符串交给 shell 的，参数必须转义。
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

async fn create_dir(path: &Path) -> Result<(), WorkdirError> {
    tokio::fs::create_dir_all(path)
        .await
        .map_err(|source| WorkdirError::Io {
            path: path.to_path_buf(),
            source,
        })
}

async fn write_file(path: &Path, content: &str) -> Result<(), WorkdirError> {
    tokio::fs::write(path, content)
        .await
        .map_err(|source| WorkdirError::Io {
            path: path.to_path_buf(),
            source,
        })
}

#[derive(Serialize)]
struct Settings {
    hooks: Hooks,
}

#[derive(Serialize)]
struct Hooks {
    #[serde(rename = "PreToolUse")]
    pre_tool_use: Vec<HookMatcher>,
}

#[derive(Serialize)]
struct HookMatcher {
    matcher: String,
    hooks: Vec<HookCommand>,
}

#[derive(Serialize)]
struct HookCommand {
    #[serde(rename = "type")]
    kind: &'static str,
    command: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skill(name: &str) -> SkillFiles {
        SkillFiles {
            name: name.into(),
            description: "排查 Linux 性能问题".into(),
            body: "# 步骤\n\n1. 看负载".into(),
            files: vec![],
        }
    }

    #[tokio::test]
    async fn skills_land_where_claude_code_looks_for_them() {
        let dir = tempfile::tempdir().expect("临时目录");
        let wd = RunWorkdir::prepare(dir.path()).await.expect("准备");
        wd.write_skills(&[skill("linux-perf")]).await.expect("写入");

        let path = dir.path().join(".claude/skills/linux-perf/SKILL.md");
        let content = tokio::fs::read_to_string(&path).await.expect("读回");
        assert!(content.starts_with("---\n"), "必须是 frontmatter 开头");
        assert!(content.contains("name: \"linux-perf\""));
        assert!(content.contains("description: \"排查 Linux 性能问题\""));
        assert!(content.contains("# 步骤"));
    }

    #[tokio::test]
    async fn a_description_with_yaml_metacharacters_does_not_break_the_frontmatter() {
        let dir = tempfile::tempdir().expect("临时目录");
        let wd = RunWorkdir::prepare(dir.path()).await.expect("准备");
        let mut s = skill("tricky");
        s.description = "含: 冒号、\"引号\"\n和换行".into();
        wd.write_skills(&[s]).await.expect("写入");

        let content = tokio::fs::read_to_string(dir.path().join(".claude/skills/tricky/SKILL.md"))
            .await
            .expect("读回");
        // frontmatter 必须还是三行结构，描述被压成一行且转义
        let lines: Vec<_> = content.lines().take(4).collect();
        assert_eq!(lines[0], "---");
        assert!(lines[2].starts_with("description: \""), "{}", lines[2]);
        assert_eq!(lines[3], "---");
        assert!(!lines[2].contains('\n'));
    }

    #[tokio::test]
    async fn extra_files_are_written_alongside_the_skill() {
        let dir = tempfile::tempdir().expect("临时目录");
        let wd = RunWorkdir::prepare(dir.path()).await.expect("准备");
        let mut s = skill("with-refs");
        s.files = vec![("references/checklist.md".into(), "1. 看 CPU".into())];
        wd.write_skills(&[s]).await.expect("写入");

        let content = tokio::fs::read_to_string(
            dir.path()
                .join(".claude/skills/with-refs/references/checklist.md"),
        )
        .await
        .expect("读回");
        assert_eq!(content, "1. 看 CPU");
    }

    #[tokio::test]
    async fn a_skill_name_with_traversal_is_rejected() {
        // 技能名要拼进路径。数据库内容也可能被改，不能假设上游一定干净。
        let dir = tempfile::tempdir().expect("临时目录");
        let wd = RunWorkdir::prepare(dir.path()).await.expect("准备");
        for bad in ["../escape", "a/b", "..", ".", "", "a\\b"] {
            assert!(
                wd.write_skills(&[skill(bad)]).await.is_err(),
                "技能名 {bad:?} 应当被拒"
            );
        }
    }

    #[tokio::test]
    async fn a_file_path_escaping_the_skill_directory_is_rejected() {
        let dir = tempfile::tempdir().expect("临时目录");
        let wd = RunWorkdir::prepare(dir.path()).await.expect("准备");
        let mut s = skill("ok-name");
        s.files = vec![("../../../etc/cron.d/pwn".into(), "x".into())];
        let err = wd.write_skills(&[s]).await.expect_err("应当被拒");
        assert!(matches!(err, WorkdirError::UnsafeSkillFile { .. }));
    }

    #[test]
    fn path_containment_handles_dotdot_in_the_middle() {
        let base = Path::new("/runs/a/.claude/skills/x");
        assert!(is_within(base, &base.join("references/y.md")));
        // 先下后上，净深度仍在里面
        assert!(is_within(base, &base.join("a/../b.md")));
        // 跳出去了
        assert!(!is_within(base, &base.join("../../../etc/passwd")));
        assert!(!is_within(base, Path::new("/etc/passwd")));
    }

    #[tokio::test]
    async fn hook_settings_match_every_tool_not_just_some() {
        let dir = tempfile::tempdir().expect("临时目录");
        let wd = RunWorkdir::prepare(dir.path()).await.expect("准备");
        let path = wd
            .write_hook_settings(&HookConfig {
                program: PathBuf::from("/usr/local/bin/ai-task"),
                endpoint: "http://127.0.0.1:8930/internal/policy/decide".into(),
                run_id: "01a0-run".into(),
                token: "secret".into(),
            })
            .await
            .expect("写入");

        let json: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(&path).await.expect("读回"))
                .expect("解析");
        let matcher = &json["hooks"]["PreToolUse"][0];
        assert_eq!(
            matcher["matcher"], "",
            "空 matcher 才是匹配所有工具；漏掉一个工具就等于给它开后门"
        );
        let command = matcher["hooks"][0]["command"].as_str().expect("命令");
        assert!(command.contains("policy-hook"));
        assert!(command.contains("01a0-run"));
    }

    #[tokio::test]
    async fn hook_command_arguments_are_shell_quoted() {
        let dir = tempfile::tempdir().expect("临时目录");
        let wd = RunWorkdir::prepare(dir.path()).await.expect("准备");
        let path = wd
            .write_hook_settings(&HookConfig {
                program: PathBuf::from("/opt/my apps/ai-task"),
                endpoint: "http://x/y".into(),
                run_id: "r".into(),
                // 命令是拼成一行交给 shell 的，令牌里的引号必须转义
                token: "tok'; rm -rf / #".into(),
            })
            .await
            .expect("写入");

        let json: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(&path).await.expect("读回"))
                .expect("解析");
        let command = json["hooks"]["PreToolUse"][0]["hooks"][0]["command"]
            .as_str()
            .expect("命令");
        assert!(command.contains(r"'tok'\''; rm -rf / #'"), "{command}");
        assert!(command.contains("'/opt/my apps/ai-task'"), "{command}");
    }
}
