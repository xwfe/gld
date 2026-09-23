//! `scripts/gen-cli-docs.sh` 失败时必须失败，而且不能动原来的 docs/cli.md。
//!
//! 以前它对字段表、密钥名表用 `|| true` 吞错，直接往目标文件里写：守护进程版本
//! 对不上时照样退出 0，留下两张空表（审查 D06）。这里用一个假 gld 分别造出
//! "某条命令失败"和"退出 0 但什么都没打印"两种情况。
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/gen-cli-docs.sh")
}

/// 一个假 gld：`first_arg` 那条命令按 `behaviour` 办，其余交给真的 gld。
fn fake_gld(dir: &Path, first_arg: &str, behaviour: &str) -> PathBuf {
    let path = dir.join("gld");
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\ncase \"$1\" in {first_arg}) {behaviour};; esac\nexec \"{}\" \"$@\"\n",
            env!("CARGO_BIN_EXE_gld")
        ),
    )
    .expect("write fake gld");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    path
}

/// 固定用 UTF-8 locale 跑：macOS 的 bash 3.2 只有在 UTF-8 下才会把紧跟变量的中文字节
/// 读进变量名。本机没设 LANG（C locale）时这类错误跑不出来，CI 的 runner 是 en_US.UTF-8，
/// 2026-09-23 就是只在 CI 上挂。
fn generate(gld: &Path, out: &Path) -> Output {
    Command::new("bash")
        .arg(script())
        .env("LC_ALL", "en_US.UTF-8")
        .env("GLD", gld)
        .env("GLD_CLI_DOC_OUT", out)
        .output()
        .expect("run gen-cli-docs.sh")
}

/// 断言失败时把脚本的退出码、stdout、stderr 都印出来：只说"退出 0 了"看不出脚本走的是哪条路
/// （2026-09-23 macOS CI 上这里失败过一次，本机和 Linux 都复现不了，当时的输出里什么都没有）。
fn describe(run: &Output) -> String {
    format!(
        "status={:?}\n--- stdout ---\n{}\n--- stderr ---\n{}",
        run.status.code(),
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    )
}

fn leftovers(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .expect("read dir")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("cli.md."))
        .collect()
}

#[test]
fn a_failing_subcommand_fails_the_run_and_keeps_the_old_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let out = dir.path().join("cli.md");
    std::fs::write(&out, "旧的\n").expect("old doc");
    let gld = fake_gld(dir.path(), "secret", "echo boom >&2; exit 4");

    let run = generate(&gld, &out);
    assert!(
        !run.status.success(),
        "命令失败了脚本还退出 0：{}",
        describe(&run)
    );
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(stderr.contains("gld secret"), "要说清是哪一条：{stderr}");
    assert_eq!(std::fs::read_to_string(&out).unwrap(), "旧的\n");
    assert!(
        leftovers(dir.path()).is_empty(),
        "{:?}",
        leftovers(dir.path())
    );
}

#[test]
fn an_empty_table_fails_the_run_even_when_every_command_exits_zero() {
    let dir = tempfile::tempdir().expect("tempdir");
    let out = dir.path().join("cli.md");
    std::fs::write(&out, "旧的\n").expect("old doc");
    // `gld fields --all` 什么都不打印、退出 0；`gld fields --help` 照常。
    let gld = fake_gld(dir.path(), "fields", "[ \"$2\" = --all ] && exit 0");

    let run = generate(&gld, &out);
    assert!(
        !run.status.success(),
        "空表也生成成功了：{}",
        describe(&run)
    );
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(stderr.contains("空表"), "{stderr}");
    assert_eq!(std::fs::read_to_string(&out).unwrap(), "旧的\n");
}

#[test]
fn a_good_run_writes_every_section() {
    let dir = tempfile::tempdir().expect("tempdir");
    let out = dir.path().join("cli.md");
    let run = generate(Path::new(env!("CARGO_BIN_EXE_gld")), &out);
    assert!(run.status.success(), "{}", describe(&run));
    let text = std::fs::read_to_string(&out).expect("generated");
    for needle in [
        "## gld tool call",
        "Usage: gld tool call",
        "## gld set 支持的字段",
        "tool-profile",
        "oauth_password",
    ] {
        assert!(text.contains(needle), "缺了 {needle}");
    }
    assert!(leftovers(dir.path()).is_empty());
}
