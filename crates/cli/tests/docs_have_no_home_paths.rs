//! 仓库是公开的，文档里不能出现真实的家目录路径。
//!
//! 写验证记录时顺手贴命令，最容易把 `/Users/<你的用户名>/…` 带进去：用户名、目录布局
//! 就这样公开了，读者照抄也用不了。写 `~/…`、仓库内相对路径，或者 `/Users/you` 这类占位。

use std::path::{Path, PathBuf};

/// 示例里常用的占位用户名。别的名字一律当成真实路径。
const PLACEHOLDERS: &[&str] = &["you", "me", "user", "username", "example", "runner"];

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

fn markdown_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read docs dir").flatten() {
        let path = entry.path();
        if path.is_dir() {
            markdown_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "md") {
            out.push(path);
        }
    }
}

/// `text` 里所有 `<prefix><名字><分隔符>` 中名字不是占位的。
fn home_paths(text: &str, prefix: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find(prefix) {
        let after = &rest[at + prefix.len()..];
        let name: String = after
            .chars()
            .take_while(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
            .collect();
        if !name.is_empty() && !PLACEHOLDERS.contains(&name.to_ascii_lowercase().as_str()) {
            found.push(format!("{prefix}{name}"));
        }
        rest = &after[name.len()..];
    }
    found
}

#[test]
fn docs_have_no_real_home_directory_paths() {
    let root = repo_root();
    let mut files: Vec<PathBuf> = ["README.md", "AGENTS.md", "task_plan.md"]
        .iter()
        .map(|name| root.join(name))
        .filter(|path| path.is_file())
        .collect();
    markdown_files(&root.join("docs"), &mut files);
    files.sort();
    assert!(files.len() > 10, "没找到文档：{files:?}");

    let mut problems = Vec::new();
    for file in &files {
        let text = std::fs::read_to_string(file).expect("read markdown");
        for (number, line) in text.lines().enumerate() {
            for prefix in ["/Users/", "/home/", "C:\\Users\\"] {
                for hit in home_paths(line, prefix) {
                    problems.push(format!(
                        "{}:{}: {hit}",
                        file.strip_prefix(&root).unwrap_or(file).display(),
                        number + 1
                    ));
                }
            }
        }
    }
    assert!(
        problems.is_empty(),
        "文档里有真实家目录路径，换成 ~/…、仓库内相对路径或 /Users/you：\n{}",
        problems.join("\n")
    );
}

#[test]
fn placeholders_pass_and_real_names_are_caught() {
    assert!(home_paths("见 /Users/you/.config/gld 和 /home/runner/work", "/Users/").is_empty());
    assert!(home_paths("/home/runner/work", "/home/").is_empty());
    assert_eq!(
        home_paths("构建在 /Users/alice/src/gld 里", "/Users/"),
        vec!["/Users/alice"]
    );
    assert_eq!(
        home_paths("C:\\Users\\bob\\gld", "C:\\Users\\"),
        vec!["C:\\Users\\bob"]
    );
}
