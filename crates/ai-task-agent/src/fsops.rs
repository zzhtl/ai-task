//! 受限的文件操作。
//!
//! 所有路径必须落在声明的根目录之内。这不是防君子的检查——远端 agent 执行的是
//! AI 决定要做的事，而上下文里可能有不可信内容（比如它刚读到的一个日志文件）。

use std::path::{Path, PathBuf};

/// 一次会话允许触碰的根目录。
#[derive(Debug, Clone)]
pub struct Roots {
    roots: Vec<PathBuf>,
}

#[derive(Debug)]
pub enum PathError {
    Outside { path: String },
    Io(std::io::Error),
}

impl std::fmt::Display for PathError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Outside { path } => write!(f, "路径 {path} 不在本次会话允许的目录内"),
            Self::Io(err) => write!(f, "{err}"),
        }
    }
}

impl Roots {
    /// 归一化给定的根目录。不存在的直接丢掉——留着只会在后面产生误导性的错误。
    #[must_use]
    pub fn new(roots: &[String]) -> Self {
        Self {
            roots: roots
                .iter()
                .filter_map(|r| std::fs::canonicalize(r).ok())
                .collect(),
        }
    }

    /// 校验一个**已存在**的路径。
    pub fn resolve(&self, path: &str) -> Result<PathBuf, PathError> {
        let canonical = std::fs::canonicalize(path).map_err(PathError::Io)?;
        self.check(&canonical, path)
    }

    /// 校验一个**将要创建**的路径：对父目录做 canonicalize，再拼上文件名。
    ///
    /// 不能直接 canonicalize 自身——文件还不存在，那会失败。
    pub fn resolve_for_create(&self, path: &str) -> Result<PathBuf, PathError> {
        let requested = Path::new(path);
        let parent = requested.parent().unwrap_or(Path::new("."));
        let name = requested.file_name().ok_or_else(|| PathError::Outside {
            path: path.to_string(),
        })?;
        let canonical_parent = std::fs::canonicalize(parent).map_err(PathError::Io)?;
        let target = canonical_parent.join(name);
        self.check(&target, path)
    }

    fn check(&self, canonical: &Path, original: &str) -> Result<PathBuf, PathError> {
        if self.roots.iter().any(|root| canonical.starts_with(root)) {
            Ok(canonical.to_path_buf())
        } else {
            Err(PathError::Outside {
                path: original.to_string(),
            })
        }
    }

    #[must_use]
    pub fn describe(&self) -> String {
        self.roots
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// 按 glob 列文件。只支持 `*` 和 `?`，够用且没有惊喜。
#[must_use]
pub fn glob(root: &Path, pattern: &str, limit: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                // 不跟符号链接：一个指回上级的链接会让遍历转不出来
                if entry.file_type().is_ok_and(|t| !t.is_symlink()) {
                    stack.push(path);
                }
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if matches(&name, pattern) {
                out.push(path.display().to_string());
                if out.len() >= limit {
                    out.sort();
                    return out;
                }
            }
        }
    }
    out.sort();
    out
}

/// 极简 glob 匹配：`*` 任意多个字符，`?` 单个字符。
fn matches(name: &str, pattern: &str) -> bool {
    let n: Vec<char> = name.chars().collect();
    let p: Vec<char> = pattern.chars().collect();
    let (mut i, mut j) = (0usize, 0usize);
    // star/mark 记住最后一个 `*` 的位置，失配时回溯到那里再吞一个字符
    let (mut star, mut mark) = (usize::MAX, 0usize);

    while i < n.len() {
        if j < p.len() && (p[j] == '?' || p[j] == n[i]) {
            i += 1;
            j += 1;
        } else if j < p.len() && p[j] == '*' {
            star = j;
            mark = i;
            j += 1;
        } else if star != usize::MAX {
            j = star + 1;
            mark += 1;
            i = mark;
        } else {
            return false;
        }
    }
    while j < p.len() && p[j] == '*' {
        j += 1;
    }
    j == p.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_matching_handles_stars_and_marks() {
        assert!(matches("app.log", "*.log"));
        assert!(matches("app.log", "app.???"));
        assert!(matches("a", "*"));
        assert!(matches("", "*"));
        assert!(!matches("app.log", "*.txt"));
        assert!(!matches("app.log", "app.??"));
        // 多个 * 要能回溯
        assert!(matches("a-b-c.log", "*-*-*.log"));
        assert!(!matches("abc", "a*d"));
    }

    #[test]
    fn paths_outside_the_roots_are_rejected() {
        let dir = std::env::temp_dir();
        let roots = Roots::new(&[dir.display().to_string()]);
        assert!(roots.resolve("/etc/passwd").is_err());
        assert!(roots.resolve(&dir.display().to_string()).is_ok());
    }

    #[test]
    fn traversal_out_of_a_root_is_rejected_after_canonicalization() {
        // canonicalize 会把 `..` 折叠掉，所以纯字符串前缀比较是不够的
        let dir = tempdir();
        let roots = Roots::new(&[dir.display().to_string()]);
        let escape = format!("{}/../../../etc/passwd", dir.display());
        assert!(roots.resolve(&escape).is_err(), "{escape}");
    }

    #[test]
    fn creating_a_new_file_inside_a_root_is_allowed() {
        let dir = tempdir();
        let roots = Roots::new(&[dir.display().to_string()]);
        // 文件还不存在，不能直接 canonicalize 自身
        let target = format!("{}/new.txt", dir.display());
        assert!(roots.resolve_for_create(&target).is_ok());

        let escape = format!("{}/../escaped.txt", dir.display());
        assert!(roots.resolve_for_create(&escape).is_err());
    }

    #[test]
    fn no_roots_means_nothing_is_allowed() {
        // 空根目录列表必须是"全拒"而不是"全放"
        let roots = Roots::new(&[]);
        assert!(roots.resolve("/etc/passwd").is_err());
        assert!(roots.resolve("/tmp").is_err());
    }

    #[test]
    fn glob_finds_files_and_respects_the_limit() {
        let dir = tempdir();
        for i in 0..5 {
            std::fs::write(dir.join(format!("f{i}.log")), "x").expect("写文件");
        }
        std::fs::write(dir.join("other.txt"), "x").expect("写文件");

        assert_eq!(glob(&dir, "*.log", 100).len(), 5);
        assert_eq!(glob(&dir, "*.log", 2).len(), 2);
        assert_eq!(glob(&dir, "*.txt", 100).len(), 1);
    }

    fn tempdir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ai-task-agent-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).expect("建目录");
        dir
    }
}
