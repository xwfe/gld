//! `gld --version` 带构建提交：版本号只在发版时改，没发版的构建光看号分不清是哪份代码（审查 §35）。

use std::process::Command;

#[test]
fn version_names_the_build_commit_when_it_is_known() {
    let output = Command::new(env!("CARGO_BIN_EXE_gld"))
        .arg("--version")
        .output()
        .expect("run gld --version");
    assert!(output.status.success(), "{output:?}");
    let line = String::from_utf8(output.stdout).expect("utf-8");
    let line = line.trim();
    let expected = format!("gld {}", env!("CARGO_PKG_VERSION"));
    // scripts/package.sh 认的就是这两种：只有版本号（从 tarball 构建），或者后面带括号里的提交。
    let commit = match line.strip_prefix(&expected) {
        Some("") => None,
        Some(rest) => Some(
            rest.strip_prefix(" (")
                .and_then(|rest| rest.strip_suffix(')'))
                .unwrap_or_else(|| panic!("版本行格式不对：{line}")),
        ),
        None => panic!("版本行应以「{expected}」开头：{line}"),
    };

    // 在 git 检出里构建的，括号里就该是 HEAD（build.rs 盯着 HEAD，变了会重编）。
    let head = Command::new("git")
        .args(["rev-parse", "--short=12", "HEAD"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string());
    if let Some(head) = head {
        assert_eq!(commit, Some(head.as_str()), "{line}");
    }
}
