//! 把构建时的 git 提交号嵌进二进制。
//!
//! 为什么要有：排错的时候"我读的源码"和"正在跑的服务"是两件事，而它们经常
//! 对不上——装好的二进制是三天前编的，源码已经改过好几轮了。诊断里只报版本号
//! （`0.5.0`）没用，同一个版本号能对应几十个提交（跨仓评审 X10）。
//!
//! **拿不到就什么都不做。**从 tarball 解出来编译、`.git` 不在、机器上没有 git，
//! 这些都不是错误——`check_exec_environment` 那边会如实报 `build_commit: null`，
//! 而不是拿版本号顶替。构建脚本在任何情况下都不该让编译失败。

use std::path::Path;
use std::process::Command;

fn main() {
    let Some(git_dir) = find_git_dir() else {
        return;
    };
    // 换了提交、切了分支都要重新跑这个脚本，否则 cargo 会一直用缓存里那个
    // 旧的提交号——报一个过期的 SHA 比不报更糟。
    println!("cargo:rerun-if-changed={}", git_dir.join("HEAD").display());
    if let Ok(head) = std::fs::read_to_string(git_dir.join("HEAD")) {
        if let Some(reference) = head.trim().strip_prefix("ref: ") {
            println!(
                "cargo:rerun-if-changed={}",
                git_dir.join(reference).display()
            );
        }
    }

    let Ok(output) = Command::new("git")
        .args(["rev-parse", "--short=12", "HEAD"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
    else {
        return;
    };
    if !output.status.success() {
        return;
    }
    let commit = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !commit.is_empty() {
        println!("cargo:rustc-env=GLD_BUILD_COMMIT={commit}");
    }
}

/// 找到 `.git`。普通检出里它是目录，worktree 和 submodule 里它是一个
/// 写着 `gitdir: <路径>` 的文件。
fn find_git_dir() -> Option<std::path::PathBuf> {
    let mut dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    loop {
        let candidate = dir.join(".git");
        if candidate.is_dir() {
            return Some(candidate);
        }
        if candidate.is_file() {
            let pointer = std::fs::read_to_string(&candidate).ok()?;
            let path = pointer.trim().strip_prefix("gitdir: ")?;
            let path = Path::new(path);
            return Some(if path.is_absolute() {
                path.to_path_buf()
            } else {
                dir.join(path)
            });
        }
        dir = dir.parent()?;
    }
}
