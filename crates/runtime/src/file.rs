//! 工作区文件工具：统一导出读取、目录列表和文本搜索入口。

use std::path::Path;

use serde_json::Value;

use crate::workspace::{relative_display, Workspace};

mod listing;
mod read;
mod search;

pub use listing::{list_dir, list_files};
pub use read::read_file;
pub use search::search_text;

/// 该不该走进这个目录。
///
/// 以前是走进去之后再逐个文件丢弃：`node_modules` 里几万个文件每个都要
/// `metadata()` 一次，一次搜索能因此多花几秒，而它们没有一个会进结果
/// （审查 F03、验收 A14）。现在整棵不进。
///
/// **只用来剪枝，不放宽任何检查**：文件那一层的 `is_safe_read_path` 和
/// `is_ignored_path` 照旧各判一遍，所以这里判错了顶多是多走一段路，不会让
/// 不该读的文件进结果。
fn keep_walking_into(
    ws: &Workspace,
    root: &Path,
    entry: &walkdir::DirEntry,
    include_hidden: bool,
) -> bool {
    // 起点自己永远要进：调用方明确指了它，哪怕它叫 .github。
    if entry.path() == root {
        return true;
    }
    if !entry.file_type().is_dir() {
        return true;
    }
    !ws.is_ignored_path(entry.path(), include_hidden, false)
}

/// 剪掉的目录里，有几棵是**因为点开头**才没进去的。
///
/// `.github` 默认不进，于是"这个仓库没有 workflow"和"workflow 在一棵没进去的
/// 目录里"长得一模一样——零结果的时候必须分得出来（验收 A14；U5 回放仓库维护
/// 流程时撞到的）。
///
/// 只在剪枝那一刻数：目录整棵不进，里面的文件根本不会出现在任何计数里。
#[derive(Default)]
struct HiddenSkips {
    count: usize,
    examples: Vec<String>,
}

impl HiddenSkips {
    /// `entry` 刚被剪掉。判据是"改成 include_hidden=true 就会进去"——
    /// `node_modules`、`target` 那一类另有开关（`include_ignored`），
    /// `.git` 两个开关都不开，都不算在这里。
    fn note(&mut self, ws: &Workspace, entry: &walkdir::DirEntry) {
        if !entry.file_type().is_dir() || ws.is_ignored_path(entry.path(), true, false) {
            return;
        }
        self.count += 1;
        if self.examples.len() < 3 {
            self.examples
                .push(relative_display(ws.root(), entry.path()));
        }
    }

    fn warning(&self, verb: &str) -> Option<String> {
        if self.count == 0 {
            return None;
        }
        Some(format!(
            "{} hidden directory(ies) were not {verb} ({}); dot-directories such as .github are skipped unless include_hidden=true",
            self.count,
            self.examples.join(", ")
        ))
    }
}

fn string_list_arg(args: &Value, key: &str) -> Vec<String> {
    args.get(key)
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn glob_match(pattern: &str, path: &str) -> bool {
    let pat = pattern.replace('\\', "/");
    let p = path.replace('\\', "/");
    if pat == "**/*" || pat == "*" {
        return true;
    }
    // `**` 交给 glob crate 自己解析。以前这里手工剥掉前导 `**/` 再拿剩下的
    // 部分去比，带子目录的模式（`**/tests/*.rs`）永远比不中任何嵌套文件，
    // list_files / search_text 整片返回空（审查 §85、§89）。
    let Ok(pattern) = glob::Pattern::new(&pat) else {
        return false;
    };
    // 目录名命中也算：`**/deps` 这种「以目录名结尾」的排除要能盖住目录下
    // 所有文件，而遍历只拿文件的相对路径来比，目录本身从不进匹配。依次比
    // `web`、`web/deps`、`web/deps/y/b.js` 这些前缀（PR #3 复核，和旧行为
    // 「任一路径段相同即命中」对齐；include / exclude 共用这一个函数）。
    let mut prefix = String::new();
    for segment in p.split('/') {
        if !prefix.is_empty() {
            prefix.push('/');
        }
        prefix.push_str(segment);
        if pattern.matches(&prefix) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::glob_match;

    /// `**/` 后面带子目录的模式要按工作区根的任意深度匹配（审查 §85、§89）。
    /// 以前手工剥掉前导 `**/` 后只比整条路径和单个路径段，`**/tests/*.rs`
    /// 对 `crates/core/tests/x.rs` 永远是 false。
    #[test]
    fn glob_match_recurses_through_directories() {
        assert!(glob_match("**/tests/*.rs", "crates/core/tests/x.rs"));
        assert!(glob_match("**/tests/*.rs", "tests/x.rs"));
        assert!(!glob_match("**/tests/*.rs", "crates/core/src/x.rs"));
        assert!(glob_match("**/exec.rs", "crates/core/exec.rs"));
        assert!(glob_match("**/exec.rs", "exec.rs"));
        assert!(glob_match("crates/**/exec.rs", "crates/core/exec.rs"));
        assert!(glob_match("assets/**", "assets/raw.bin"));
        assert!(glob_match("**\\tests\\*.rs", "crates\\core\\tests\\x.rs"));
    }

    /// 「以目录名结尾」的模式命中目录下所有文件：遍历只拿文件的工作区相对路径来比，
    /// 靠逐级路径前缀比出目录名命中，include / exclude 共用这一个函数。
    #[test]
    fn glob_match_directory_name_covers_files_below() {
        assert!(glob_match("**/foo", "a/foo/b.txt"));
        assert!(glob_match("**/foo", "a/b/foo"));
        assert!(glob_match("**/foo", "foo"));
        assert!(!glob_match("**/foo", "a/food/b.txt"));
        assert!(!glob_match("**/foo", "a/bar.txt"));
        assert!(!glob_match("exec.rs", "crates/core/exec.rs"));
        assert!(glob_match("exec.rs", "exec.rs"));
    }

    /// `*` 会跨 `/`：glob crate 默认 require_literal_separator=false，这是原有行为。
    #[test]
    fn glob_match_star_crosses_path_separators() {
        assert!(glob_match("tests/*.rs", "tests/a/b.rs"));
        assert!(glob_match("*.rs", "crates/core/x.rs"));
        assert!(!glob_match("*.rs", "crates/core/x.txt"));
    }
}
