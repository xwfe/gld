//! 构建脚本报的提交号，是它这次被叫来编的那个检出的，不是它自己被编译时所在的那个。
//!
//! 两个检出共用一个 `target` 时，cargo 会拿另一个检出编出来的构建脚本直接跑。脚本里要是把
//! 自己编译时的目录写死，那个检出一删，版本号里就没有提交号了（审查 §119）。

#![cfg(unix)]

use std::path::Path;
use std::process::Command;

#[test]
fn the_build_script_reads_the_checkout_it_runs_for() {
    let here = Path::new(env!("CARGO_MANIFEST_DIR"));
    let head = Command::new("git")
        .args(["rev-parse", "--short=12", "HEAD"])
        .current_dir(here)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string());
    let Some(head) = head.filter(|head| !head.is_empty()) else {
        // 从 tarball 构建的没有提交号可比。
        return;
    };

    // 在一个之后会消失的目录名下编译构建脚本，再让它为本检出跑。
    let scratch = tempfile::tempdir().expect("tempdir");
    let gone = scratch.path().join("deleted-checkout").join("crates/core");
    let script = scratch.path().join("build-script");
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let compiled = Command::new(rustc)
        .args(["--edition", "2021", "-o"])
        .arg(&script)
        .arg(here.join("build.rs"))
        .env("CARGO_MANIFEST_DIR", &gone)
        .output()
        .expect("rustc");
    assert!(
        compiled.status.success(),
        "构建脚本编不过：{}",
        String::from_utf8_lossy(&compiled.stderr)
    );

    let ran = Command::new(&script)
        .env("CARGO_MANIFEST_DIR", here)
        .output()
        .expect("run build script");
    let stdout = String::from_utf8_lossy(&ran.stdout);
    assert!(
        stdout
            .lines()
            .any(|line| line == format!("cargo:rustc-env=GLD_BUILD_COMMIT={head}")),
        "构建脚本没报本检出的提交号 {head}：\n{stdout}"
    );
}
