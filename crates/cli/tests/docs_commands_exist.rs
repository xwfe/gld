//! README 和 docs 里写的命令必须真的能跑。
//!
//! 文档里有上百条 `gld ...` 示例。改个子命令名而忘了同步文档，读者照着做
//! 只会得到 "unrecognized subcommand"——而这类失效通常要等到有人抱怨才被发现。
//!
//! `docs/cli.md` 不在检查范围内：它整份由 `scripts/gen-cli-docs.sh` 从
//! `--help` 生成，CI 里另有一道比对。

mod common;

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR = crates/cli
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

fn documents() -> Vec<PathBuf> {
    let root = repo_root();
    let mut files = vec![root.join("README.md")];
    let docs = root.join("docs");
    let mut entries: Vec<PathBuf> = std::fs::read_dir(&docs)
        .expect("docs 目录")
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "md"))
        // cli.md 是生成物，由 CI 的另一道检查负责。
        .filter(|path| path.file_name().is_some_and(|name| name != "cli.md"))
        .collect();
    entries.sort();
    files.extend(entries);
    files
}

#[test]
fn every_documented_command_exists() {
    let mut total = 0usize;
    let mut files_with_commands = 0usize;

    for path in documents() {
        let text = std::fs::read_to_string(&path).expect("读取文档");
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("?")
            .to_string();
        let commands = common::docs::extract_from_docs(&text, &name);
        if !commands.is_empty() {
            files_with_commands += 1;
        }
        for command in &commands {
            common::docs::assert_valid(command);
            total += 1;
        }
    }

    assert!(
        files_with_commands >= 5,
        "只在 {files_with_commands} 个文档里找到命令，抽取逻辑可能失效了"
    );
    assert!(
        total >= 100,
        "只校验了 {total} 条命令（当前文档里有 140+ 条），抽取逻辑可能失效了"
    );
}

/// `scripts/gen-cli-docs.sh` 里那份命令清单也得是真命令。
///
/// 删掉一个子命令而忘了从清单里去掉，脚本会在 `gld xxx --help` 那一步
/// 因为 `set -e` 直接退出（exit 2，报 `unrecognized subcommand`），
/// docs/cli.md 于是停在旧版本。这个失效只有 CI 那道 `git diff --exit-code`
/// 会发现，本地 `cargo test` 一片绿——所以在这里也拦一道。
#[test]
fn the_cli_docs_generator_only_lists_real_commands() {
    let script = std::fs::read_to_string(repo_root().join("scripts/gen-cli-docs.sh"))
        .expect("读取 gen-cli-docs.sh");
    let body = script
        .split_once("COMMANDS=(")
        .and_then(|(_, rest)| rest.split_once("\n)"))
        .map(|(body, _)| body)
        .expect("找不到 COMMANDS 数组，脚本结构变了");

    let mut checked = 0usize;
    for raw in body.split('"').skip(1).step_by(2) {
        // 空串那项代表 `gld --help` 本身，没有子命令要校验。
        if raw.trim().is_empty() {
            continue;
        }
        let mut tokens = vec!["gld".to_string()];
        tokens.extend(raw.split_whitespace().map(str::to_string));
        common::docs::assert_valid(&common::docs::Extracted {
            tokens,
            source: "scripts/gen-cli-docs.sh".into(),
        });
        checked += 1;
    }
    assert!(
        checked >= 50,
        "只从脚本里读到 {checked} 条命令，解析逻辑可能失效了"
    );
}
