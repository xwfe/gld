//! README 和 docs 下所有 Markdown（含 rfc/、reviews/）里的相对链接都要指得到。
//!
//! `docs_commands_exist` 只查命令，不查链接，也不进子目录（审查 D06）。改个标题
//! 或挪个文件，别处写着的 `concepts.md#某一节` 就悄悄断了，读者点过去只落在页首。
//! 这里查两件事：文件在不在；带 `#锚点` 的，那一页有没有这个标题。
//! 锚点按 GitHub 的规则算：小写，字母数字（含中文）、`-`、`_` 留下，空格变 `-`，
//! 其余标点去掉；同名标题从第二个起加 `-1`、`-2`。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

/// 按字面消掉 `.` 和 `..`，不碰文件系统：不存在的路径也要能判断出没出仓库。
fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

fn markdown_files(dir: &Path, into: &mut Vec<PathBuf>) {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .expect("read docs dir")
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            markdown_files(&path, into);
        } else if path.extension().is_some_and(|ext| ext == "md") {
            into.push(path);
        }
    }
}

/// 去掉围栏代码块：代码里的 `](...)` 和 `# ...` 不是链接和标题。
fn prose(text: &str) -> String {
    let mut out = String::new();
    let mut fenced = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if !fenced {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

fn slug(heading: &str) -> String {
    // 标题里的链接只留文字，反引号、星号不算。
    let mut text = String::new();
    let mut rest = heading;
    while let Some(start) = rest.find('[') {
        let Some(mid) = rest[start..].find("](") else {
            break;
        };
        let Some(end) = rest[start + mid..].find(')') else {
            break;
        };
        text.push_str(&rest[..start]);
        text.push_str(&rest[start + 1..start + mid]);
        rest = &rest[start + mid + end + 1..];
    }
    text.push_str(rest);
    text.trim()
        .to_lowercase()
        .chars()
        .filter_map(|ch| match ch {
            ' ' => Some('-'),
            '-' | '_' => Some(ch),
            ch if ch.is_alphanumeric() => Some(ch),
            _ => None,
        })
        .collect()
}

fn anchors(text: &str) -> HashSet<String> {
    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut anchors = HashSet::new();
    for line in prose(text).lines() {
        let hashes = line.chars().take_while(|ch| *ch == '#').count();
        if hashes == 0 || !line[hashes..].starts_with(' ') {
            continue;
        }
        let base = slug(&line[hashes..]);
        let count = seen.entry(base.clone()).or_insert(0);
        anchors.insert(if *count == 0 {
            base.clone()
        } else {
            format!("{base}-{count}")
        });
        *count += 1;
    }
    anchors
}

fn links(text: &str) -> Vec<String> {
    let prose = prose(text);
    let mut found = Vec::new();
    let mut rest = prose.as_str();
    while let Some(at) = rest.find("](") {
        let after = &rest[at + 2..];
        let end = after
            .find(|ch: char| ch == ')' || ch.is_whitespace())
            .unwrap_or(after.len());
        found.push(after[..end].to_string());
        rest = &after[end..];
    }
    found
}

#[test]
fn every_relative_link_in_the_docs_resolves() {
    let root = repo_root();
    let mut files = vec![root.join("README.md")];
    markdown_files(&root.join("docs"), &mut files);

    let mut checked = 0usize;
    let mut broken = Vec::new();
    let mut anchor_cache: HashMap<PathBuf, HashSet<String>> = HashMap::new();
    for file in &files {
        let text = std::fs::read_to_string(file).expect("read doc");
        for link in links(&text) {
            // 外部地址（https:、mailto: 之类）不在这里查。
            if link.split_once(':').is_some_and(|(scheme, _)| {
                !scheme.is_empty() && scheme.chars().all(|ch| ch.is_ascii_alphabetic())
            }) {
                continue;
            }
            let (path, anchor) = link.split_once('#').unwrap_or((link.as_str(), ""));
            let target = if path.is_empty() {
                file.clone()
            } else {
                lexical(&file.parent().expect("parent").join(path))
            };
            // 指出仓库之外的（README 里的 `../../releases`）是给 GitHub 网页用的。
            if !target.starts_with(&root) {
                continue;
            }
            if !target.exists() {
                broken.push(format!("{}: {link}（文件不存在）", file.display()));
                continue;
            }
            checked += 1;
            if anchor.is_empty() || target.extension().is_none_or(|ext| ext != "md") {
                continue;
            }
            let anchors = anchor_cache.entry(target.clone()).or_insert_with(|| {
                anchors(&std::fs::read_to_string(&target).expect("read target"))
            });
            if !anchors.contains(anchor) {
                broken.push(format!("{}: {link}（没有这一节）", file.display()));
            }
        }
    }
    assert!(checked > 100, "只查到 {checked} 个链接，扫描范围可能坏了");
    assert!(broken.is_empty(), "断了的链接：\n{}", broken.join("\n"));
}

#[test]
fn anchors_follow_the_github_rules() {
    assert_eq!(
        slug(" 任务怎么收尾：带证据才算 completed"),
        "任务怎么收尾带证据才算-completed"
    );
    assert_eq!(slug(" 7. 处理进展"), "7-处理进展");
    assert_eq!(slug(" `gld tool` 与 [RFC](x.md)"), "gld-tool-与-rfc");
    let text = "# A\n## A\n```\n# not a heading\n```\n";
    let found = anchors(text);
    assert!(found.contains("a") && found.contains("a-1"));
    assert!(!found.contains("not-a-heading"));
}
