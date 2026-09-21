//! 把"这个二进制到底是什么"嵌进二进制：本仓的提交号，和链进来的共享 crate
//! 各自锁到了哪个提交。
//!
//! 为什么要有：排错的时候"我读的源码"和"正在跑的服务"是两件事，而它们经常
//! 对不上——装好的二进制是三天前编的，源码已经改过好几轮了。诊断里只报版本号
//! （`0.5.0`）没用，同一个版本号能对应几十个提交（跨仓评审 X10）。
//!
//! 共享 crate 同理，而且更隐蔽：`Cargo.toml` 里写的是 **git tag**
//! （`toexec-fs-v0.2.1`），而 tag 是可以被移动的——两次构建都说自己用的
//! "v0.2.1"，里面的代码可以不一样。`Cargo.lock` 里那个 `#<sha>` 才是实际链
//! 进来的那一份。
//!
//! **拿不到就什么都不做。**从 tarball 解出来编译、`.git` 不在、机器上没有 git、
//! 没有 `Cargo.lock`，这些都不是错误——诊断那边会如实报 `null` 或空表，
//! 而不是拿版本号顶替。构建脚本在任何情况下都不该让编译失败。

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    emit_shared_crate_revisions();
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

/// 从 `Cargo.lock` 里读共享 crate（`toexec-*`）锁到了哪个提交。
///
/// 不引第三方 TOML 解析器：构建脚本的依赖会进每个下游的构建，为读三行字段
/// 加一个不是必需的。`Cargo.lock` 的格式很稳，按行扫就够，扫不出来就空着。
fn emit_shared_crate_revisions() {
    let Some(lock) = find_upwards("Cargo.lock") else {
        return;
    };
    println!("cargo:rerun-if-changed={}", lock.display());
    let Ok(text) = std::fs::read_to_string(&lock) else {
        return;
    };

    let mut entries: Vec<String> = Vec::new();
    let mut name: Option<String> = None;
    let mut version: Option<String> = None;
    for line in text.lines() {
        let line = line.trim();
        if line == "[[package]]" {
            name = None;
            version = None;
        } else if let Some(value) = line.strip_prefix("name = ") {
            name = Some(value.trim_matches('"').to_string());
        } else if let Some(value) = line.strip_prefix("version = ") {
            version = Some(value.trim_matches('"').to_string());
        } else if let Some(value) = line.strip_prefix("source = ") {
            let Some(package) = name.as_deref() else {
                continue;
            };
            if !package.starts_with("toexec-") {
                continue;
            }
            // `git+https://…?tag=toexec-fs-v0.2.1#3f5b9f98…`
            let source = value.trim_matches('"');
            let revision = source
                .rsplit_once('#')
                .map(|(_, rev)| rev.chars().take(12).collect::<String>())
                .unwrap_or_default();
            entries.push(format!(
                "{package} {} {revision}",
                version.as_deref().unwrap_or("?")
            ));
        }
    }
    if !entries.is_empty() {
        println!("cargo:rustc-env=GLD_SHARED_CRATES={}", entries.join(","));
    }
}

/// 从本 crate 往上找一个文件（工作区根上的 `Cargo.lock`）。
fn find_upwards(file: &str) -> Option<PathBuf> {
    let mut dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    loop {
        let candidate = dir.join(file);
        if candidate.is_file() {
            return Some(candidate);
        }
        dir = dir.parent()?;
    }
}
