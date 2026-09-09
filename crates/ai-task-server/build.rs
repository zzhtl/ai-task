//! 构建时调 bun 打前端，产物落在 `dist/`，由 `rust-embed` 内嵌进二进制。
//!
//! 刻意在缺 bun 时**硬失败**：静默产出一个只带占位页的空壳二进制，比构建失败
//! 难排查得多。

use std::path::Path;
use std::process::Command;

/// 占位页哨兵。CI 用它断言嵌进去的不是空壳。
const PLACEHOLDER_SENTINEL: &str = "<!--ai-task-placeholder-->";

fn main() -> anyhow::Result<()> {
    for path in [
        "../../web/src",
        "../../web/static",
        "../../web/package.json",
        "../../web/svelte.config.js",
        "../../web/vite.config.ts",
        "../../web/tsconfig.json",
    ] {
        println!("cargo:rerun-if-changed={path}");
    }
    println!("cargo:rerun-if-env-changed=AI_TASK_SKIP_WEB_BUILD");

    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let web = manifest.join("../../web");
    let dist = manifest.join("dist");

    // 发布包 / vendored 场景：没有 web/ 源码，只能用占位页。
    if !web.exists() {
        std::fs::create_dir_all(&dist)?;
        write_placeholder(&dist)?;
        println!("cargo:warning=web/ 不存在，跳过前端构建（使用占位页）");
        return Ok(());
    }

    // CI 场景：前端由独立 job 只构建一次，多个 target 复用同一份 dist/。
    if skip_web_build() {
        let index = dist.join("index.html");
        anyhow::ensure!(
            index.exists(),
            "AI_TASK_SKIP_WEB_BUILD 已设置，但 {} 不存在；请先在 web/ 下执行 bun run build",
            index.display()
        );
        anyhow::ensure!(
            !std::fs::read_to_string(&index)?.contains(PLACEHOLDER_SENTINEL),
            "AI_TASK_SKIP_WEB_BUILD 已设置，但 {} 是占位页而非真实前端产物",
            index.display()
        );
        println!("cargo:warning=AI_TASK_SKIP_WEB_BUILD=1，复用已有 dist/");
        return Ok(());
    }

    anyhow::ensure!(
        which("bun").is_some(),
        "未找到 bun，无法构建前端。安装（curl -fsSL https://bun.sh/install | bash），\
         或先自行构建 web/ 后用 AI_TASK_SKIP_WEB_BUILD=1 复用已有 dist/"
    );

    if !web.join("node_modules").exists() {
        run("bun", &["install"], &web)?;
    }
    run("bun", &["run", "build"], &web)?;

    let index = dist.join("index.html");
    anyhow::ensure!(
        index.exists(),
        "bun run build 结束后仍未生成 {}",
        index.display()
    );
    Ok(())
}

fn skip_web_build() -> bool {
    matches!(
        std::env::var("AI_TASK_SKIP_WEB_BUILD").as_deref(),
        Ok("1") | Ok("true")
    )
}

fn which(cmd: &str) -> Option<std::path::PathBuf> {
    let output = Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {cmd}"))
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!path.is_empty()).then(|| path.into())
}

fn run(cmd: &str, args: &[&str], cwd: &Path) -> anyhow::Result<()> {
    let status = Command::new(cmd).args(args).current_dir(cwd).status()?;
    anyhow::ensure!(status.success(), "{cmd} {args:?} 退出码 {status}");
    Ok(())
}

fn write_placeholder(dist: &Path) -> anyhow::Result<()> {
    std::fs::write(
        dist.join("index.html"),
        format!(
            "<!doctype html><meta charset=utf-8><title>ai-task</title>{PLACEHOLDER_SENTINEL}\
             <body><h1>ai-task 前端未构建</h1>\
             <p>请在 web/ 下执行 <code>bun install &amp;&amp; bun run build</code>，\
             或安装 bun 后重新 <code>cargo build</code>。</p></body>"
        ),
    )?;
    Ok(())
}
