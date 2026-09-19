use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use serde_json::{json, Value};
use uuid::Uuid;

use crate::tools::context::ToolContext;
use crate::tools::workspace::{tool_ok, Workspace, WorkspaceError};

pub fn apply_patch(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    let ws = &ctx.workspace;
    let patch = args
        .get("patch")
        .and_then(Value::as_str)
        .ok_or_else(|| WorkspaceError::invalid_argument("patch is required"))?;
    let dry_run = args
        .get("dry_run")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let confirm = args
        .get("confirm")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let file_patches = parse_unified_diff(patch)?;
    if file_patches.is_empty() {
        return Err(patch_failed("No files were modified."));
    }
    if let Some(path) = file_patches
        .iter()
        .find(|file| is_protected_repository_asset(&file.path))
        .map(|file| file.path.as_str())
    {
        return Err(protected_repository_asset(format!(
            "禁止删除仓库保护资产: {path}"
        )));
    }
    if !confirm {
        if let Some(path) = file_patches
            .iter()
            .find(|file| file.is_deleted && is_critical_file(&file.path))
            .map(|file| file.path.as_str())
        {
            return Err(dangerous_operation(format!(
                "删除关键项目文件需要 confirm=true: {path}"
            )));
        }
    }

    // 一个文件在一批里可能被碰好几次（先删后加、两段 Update）。记的是
    // **每个文件最终发生了什么**，按第一次出现的顺序：`affected_files` 回答
    // 的是"这次改了哪些文件"，不是"应用了几段补丁"。
    let mut order: Vec<String> = Vec::new();
    let mut operations: HashMap<String, &'static str> = HashMap::new();
    let mut staged: HashMap<String, Option<String>> = HashMap::new();

    for fp in &file_patches {
        ws.reject_unsafe_text(&fp.path)?;
        // 这一批里已经排过队的改动，才是这个文件"现在"的样子：同一批里
        // 先 Add 再 Update、或者两次 Update 同一个文件，第二段要看见第一段
        // 的结果。以前每段都重新读磁盘，最后一段 insert 覆盖前面的，前面
        // 那次编辑就这么没了（审查 P03）。
        let staged_before = staged
            .get(&ws.resolve_for_write(&fp.path)?.display)
            .cloned();
        let resolved = if fp.is_new_file || matches!(staged_before, Some(Some(_))) {
            ws.resolve_for_write(&fp.path)?
        } else {
            ws.resolve_existing(&fp.path)?
        };
        ws.reject_write_symlink(&fp.path)?;

        // Add 是"新建"，不是"覆盖"。同一批里先 Delete 过它则另说：那是
        // 明写出来的整文件替换，有测试钉着。
        if fp.is_new_file
            && (resolved.existed || matches!(staged_before, Some(Some(_))))
            && !matches!(staged_before, Some(None))
        {
            return Err(patch_failed(format!(
                "{} already exists; use *** Update File: {} to change it, or delete it first",
                resolved.display, resolved.display
            )));
        }

        let original = match &staged_before {
            // 这一批里刚删过：Add 从空白开始，别的操作没有文件可改。
            Some(None) if fp.is_new_file => String::new(),
            Some(None) => {
                return Err(patch_failed(format!(
                    "{} was deleted earlier in this patch; add it again instead of editing it",
                    resolved.display
                )))
            }
            Some(Some(text)) if fp.is_new_file => {
                let _ = text;
                String::new()
            }
            Some(Some(text)) => text.clone(),
            None if fp.is_new_file => String::new(),
            None if resolved.existed => fs::read_to_string(&resolved.path)
                .map_err(|_| WorkspaceError::not_found(format!("File not found: {}", fp.path)))?,
            None if fp.is_deleted => String::new(),
            None => return Err(patch_failed(format!("File not found: {}", fp.path))),
        };

        if fp.is_deleted {
            staged.insert(resolved.display.clone(), None);
            if operations
                .insert(resolved.display.clone(), "delete")
                .is_none()
            {
                order.push(resolved.display.clone());
            }
            continue;
        }

        let updated = apply_hunks(&original, &fp.hunks)?;
        staged.insert(resolved.display.clone(), Some(updated));
        // 文件本来就在盘上就是改，不在就是新建——先删后加的净效果因此是
        // "改"，这一点有测试钉着。
        let op = if resolved.existed { "update" } else { "add" };
        if operations.insert(resolved.display.clone(), op).is_none() {
            order.push(resolved.display.clone());
        }
    }

    let affected: Vec<Value> = order
        .iter()
        .map(|path| json!({ "path": path, "operation": operations[path] }))
        .collect();
    let summaries: Vec<String> = order
        .iter()
        .map(|path| {
            let mark = match operations[path] {
                "add" => "A",
                "delete" => "D",
                _ => "M",
            };
            format!("{mark} {path}")
        })
        .collect();

    let files_created = affected_paths(&affected, "add");
    let files_modified = affected_paths(&affected, "update");
    let files_deleted = affected_paths(&affected, "delete");

    if !dry_run {
        let _transaction_backups = commit_staged(ws, &staged)?;
        let change_id = Uuid::new_v4().simple().to_string();
        return Ok(tool_ok(json!({
            "dry_run": false,
            "clean": true,
            "change_id": change_id,
            "summary": summaries.join("\n"),
            "affected_files": affected,
            "files_created": files_created,
            "files_modified": files_modified,
            "files_deleted": files_deleted,
            "recovery": "git",
            "warnings": []
        })));
    }

    Ok(tool_ok(json!({
        "dry_run": true,
        "preflight": true,
        "clean": true,
        "summary": summaries.join("\n"),
        "affected_files": affected,
        "would_create": files_created,
        "would_modify": files_modified,
        "would_delete": files_deleted,
        "warnings": []
    })))
}

pub fn patch_check(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    let mut check_args = args.clone();
    check_args["dry_run"] = Value::Bool(true);
    let mut result = apply_patch(ctx, &check_args)?;
    if let Some(object) = result.as_object_mut() {
        object.insert("preflight".into(), Value::Bool(true));
    }
    Ok(result)
}

#[derive(Debug)]
struct FilePatch {
    path: String,
    hunks: Vec<Hunk>,
    is_new_file: bool,
    is_deleted: bool,
}

#[derive(Debug, Default)]
struct Hunk {
    lines: Vec<HunkLine>,
    /// 0-based index in the original file where the old lines start, from a
    /// unified `@@ -a,b` header. For `-a,0` (pure insertion) it is the line
    /// after line `a`.
    old_start: Option<usize>,
    /// Codex `@@ <line>` header: the old lines are looked for after the
    /// first line equal to this one.
    anchor: Option<String>,
}

#[derive(Debug)]
enum HunkLine {
    Context(String),
    Add(String),
    Remove(String),
}

fn parse_unified_diff(patch: &str) -> Result<Vec<FilePatch>, WorkspaceError> {
    if patch
        .lines()
        .any(|line| line.trim_end_matches('\r') == "*** Begin Patch")
    {
        return parse_codex_patch(patch);
    }

    let mut files = Vec::new();
    let mut current: Option<FilePatch> = None;
    let mut current_hunk: Option<Hunk> = None;

    for line in patch.lines() {
        if line.starts_with("--- ") {
            if let Some(h) = current_hunk.take() {
                if let Some(ref mut f) = current {
                    f.hunks.push(h);
                }
            }
            if let Some(f) = current.take() {
                files.push(f);
            }
            let path = parse_diff_path(line.strip_prefix("--- ").unwrap_or(""));
            current = Some(FilePatch {
                path,
                hunks: Vec::new(),
                is_new_file: line.contains("/dev/null"),
                is_deleted: false,
            });
        } else if line.starts_with("+++ ") {
            if let Some(ref mut f) = current {
                let new_path = parse_diff_path(line.strip_prefix("+++ ").unwrap_or(""));
                if !new_path.is_empty() && new_path != "/dev/null" {
                    f.path = new_path;
                }
                if line.contains("/dev/null") {
                    f.is_deleted = true;
                }
            }
        } else if line.starts_with("@@") {
            if let Some(h) = current_hunk.take() {
                if let Some(ref mut f) = current {
                    f.hunks.push(h);
                }
            }
            current_hunk = Some(Hunk {
                old_start: unified_old_start(line),
                ..Hunk::default()
            });
        } else if let Some(ref mut hunk) = current_hunk {
            if let Some(rest) = line.strip_prefix('+') {
                hunk.lines.push(HunkLine::Add(rest.to_string()));
            } else if let Some(rest) = line.strip_prefix('-') {
                hunk.lines.push(HunkLine::Remove(rest.to_string()));
            } else if let Some(rest) = line.strip_prefix(' ') {
                hunk.lines.push(HunkLine::Context(rest.to_string()));
            } else if line.is_empty() {
                hunk.lines.push(HunkLine::Context(String::new()));
            }
        }
    }
    if let Some(h) = current_hunk.take() {
        if let Some(ref mut f) = current {
            f.hunks.push(h);
        }
    }
    if let Some(f) = current.take() {
        files.push(f);
    }
    Ok(files)
}

fn parse_codex_patch(patch: &str) -> Result<Vec<FilePatch>, WorkspaceError> {
    let mut files = Vec::new();
    let mut current: Option<FilePatch> = None;
    let mut current_hunk: Option<Hunk> = None;
    let mut ended = false;

    for raw_line in patch.lines() {
        let line = raw_line.trim_end_matches('\r');
        if line == "*** Begin Patch" {
            continue;
        }
        if line == "*** End Patch" {
            finish_codex_file(&mut files, &mut current, &mut current_hunk);
            ended = true;
            continue;
        }

        let header = line
            .strip_prefix("*** Add File: ")
            .map(|path| (path, true, false))
            .or_else(|| {
                line.strip_prefix("*** Update File: ")
                    .map(|path| (path, false, false))
            })
            .or_else(|| {
                line.strip_prefix("*** Delete File: ")
                    .map(|path| (path, false, true))
            });
        if let Some((path, is_new_file, is_deleted)) = header {
            finish_codex_file(&mut files, &mut current, &mut current_hunk);
            current = Some(FilePatch {
                path: parse_diff_path(path),
                hunks: Vec::new(),
                is_new_file,
                is_deleted,
            });
            if is_new_file {
                current_hunk = Some(Hunk::default());
            }
            continue;
        }

        // 认识的指令都在上面。剩下的 `*** …` 一律报错，**不跳过**：真 Codex
        // 有 `*** Move to:`，这里没实现；静默跳过的结果是模型以为文件挪了，
        // 而磁盘上没挪（审查 P04）。
        if let Some(directive) = line.strip_prefix("*** ") {
            let name = directive.split(':').next().unwrap_or(directive).trim();
            return Err(patch_failed(format!(
                "*** {name}: is not something this patch format supports here; supported directives are *** Add File:, *** Update File:, *** Delete File:"
            )));
        }

        if let Some(header) = line.strip_prefix("@@") {
            if let Some(hunk) = current_hunk.take() {
                if let Some(ref mut file) = current {
                    file.hunks.push(hunk);
                }
            }
            let anchor = header.trim();
            current_hunk = Some(Hunk {
                anchor: (!anchor.is_empty()).then(|| anchor.to_string()),
                ..Hunk::default()
            });
            continue;
        }

        let Some(file) = current.as_ref() else {
            continue;
        };
        if file.is_deleted {
            continue;
        }
        let hunk = current_hunk.get_or_insert_with(Hunk::default);
        if let Some(rest) = line.strip_prefix('+') {
            hunk.lines.push(HunkLine::Add(rest.to_string()));
        } else if let Some(rest) = line.strip_prefix('-') {
            hunk.lines.push(HunkLine::Remove(rest.to_string()));
        } else if let Some(rest) = line.strip_prefix(' ') {
            hunk.lines.push(HunkLine::Context(rest.to_string()));
        } else if line.is_empty() {
            hunk.lines.push(HunkLine::Context(String::new()));
        }
    }

    finish_codex_file(&mut files, &mut current, &mut current_hunk);
    // 补丁被截断（网络断了、上下文满了）和补丁写完了长得一模一样，而前者
    // 应用一半就是把文件改坏。只有信封闭合了才算数。
    if !ended {
        return Err(patch_failed(
            "the patch stops before *** End Patch, so it may have been cut short; nothing was applied. Send the whole envelope",
        ));
    }
    Ok(files)
}

fn finish_codex_file(
    files: &mut Vec<FilePatch>,
    current: &mut Option<FilePatch>,
    current_hunk: &mut Option<Hunk>,
) {
    if let Some(hunk) = current_hunk.take() {
        if let Some(file) = current.as_mut() {
            file.hunks.push(hunk);
        }
    }
    if let Some(file) = current.take() {
        files.push(file);
    }
}

fn affected_paths(affected: &[Value], operation: &str) -> Vec<String> {
    affected
        .iter()
        .filter(|file| file["operation"] == operation)
        .filter_map(|file| file["path"].as_str().map(str::to_string))
        .collect()
}

fn parse_diff_path(raw: &str) -> String {
    let trimmed = raw.trim();
    let path = trimmed
        .strip_prefix("a/")
        .or_else(|| trimmed.strip_prefix("b/"))
        .unwrap_or(trimmed);
    if path == "/dev/null" {
        return String::new();
    }
    path.replace('\\', "/")
}

fn apply_hunks(original: &str, hunks: &[Hunk]) -> Result<String, WorkspaceError> {
    let line_ending = if original.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let had_trailing_newline = original.ends_with('\n');
    let mut lines: Vec<String> = if original.is_empty() {
        Vec::new()
    } else {
        original
            .split_terminator('\n')
            .map(|line| line.trim_end_matches('\r').to_string())
            .collect()
    };
    // Hunks apply in order: each is looked for after the previous one ended,
    // so a later hunk cannot land on a repeat of the text above it.
    let mut search_from = 0usize;
    // Lines added minus lines removed so far. Header line numbers refer to
    // the original file, and earlier hunks have moved everything below them.
    let mut shift: isize = 0;

    for hunk in hunks {
        if let Some(anchor) = &hunk.anchor {
            let at = lines[search_from..]
                .iter()
                .position(|line| line.trim() == anchor)
                .ok_or_else(|| {
                    patch_failed(format!("Hunk header did not match any line: @@ {anchor}"))
                })?;
            search_from += at + 1;
        }
        let hunk_old: Vec<String> = hunk
            .lines
            .iter()
            .filter_map(|l| match l {
                HunkLine::Context(s) | HunkLine::Remove(s) => Some(s.clone()),
                HunkLine::Add(_) => None,
            })
            .collect();

        let expected = hunk
            .old_start
            .map(|start| start.saturating_add_signed(shift));
        // 会删或改行的 hunk，既没有行号也没有锚点，而上下文在剩下的文件里
        // 匹配到不止一处：**不猜**。挑错一处就是改错地方，而且结果看起来
        // 是成功的。纯插入不算——插哪一处都不动原有内容，而且现有补丁大量
        // 这么写（审查 P04）。
        if expected.is_none()
            && hunk.anchor.is_none()
            && hunk.lines.iter().any(|l| matches!(l, HunkLine::Remove(_)))
        {
            let candidates = match_positions(&lines, &hunk_old, search_from, 3);
            if candidates.len() > 1 {
                let places = candidates
                    .iter()
                    .map(|line| (line + 1).to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(patch_ambiguous(format!(
                    "this hunk's context matches more than one place (lines {places}); add surrounding lines or a @@ -line,count @@ header so it is clear which one to change"
                )));
            }
        }
        let pos = find_hunk_position(&lines, &hunk_old, search_from, expected)
            .ok_or_else(|| patch_failed("Hunk context did not match file content."))?;

        let mut idx = pos;
        for hl in &hunk.lines {
            match hl {
                HunkLine::Context(_) => idx += 1,
                HunkLine::Remove(_) => {
                    if idx < lines.len() {
                        lines.remove(idx);
                    }
                }
                HunkLine::Add(s) => {
                    lines.insert(idx, s.clone());
                    idx += 1;
                }
            }
        }
        search_from = idx;
        let added = hunk
            .lines
            .iter()
            .filter(|l| matches!(l, HunkLine::Add(_)))
            .count();
        let removed = hunk
            .lines
            .iter()
            .filter(|l| matches!(l, HunkLine::Remove(_)))
            .count();
        shift += added as isize - removed as isize;
    }
    let mut output = lines.join(line_ending);
    if !output.is_empty() && (had_trailing_newline || original.is_empty()) {
        output.push_str(line_ending);
    }
    Ok(output)
}

/// 上下文在 `start` 之后命中的位置，最多找 `limit` 个（报歧义只需要证明
/// "不止一个"，不必把整份文件找完）。
fn match_positions(lines: &[String], pattern: &[String], start: usize, limit: usize) -> Vec<usize> {
    if pattern.is_empty() || start > lines.len() || pattern.len() > lines.len() - start {
        return Vec::new();
    }
    (start..=lines.len() - pattern.len())
        .filter(|&i| lines[i..i + pattern.len()] == *pattern)
        .take(limit)
        .collect()
}

/// Where the old lines of a hunk are, at or after `start`. With a line
/// number from the header, the match nearest to it wins (the file may have
/// drifted a little since the diff was made); without one, the first.
fn find_hunk_position(
    lines: &[String],
    pattern: &[String],
    start: usize,
    expected: Option<usize>,
) -> Option<usize> {
    if pattern.is_empty() {
        return Some(
            expected
                .unwrap_or(start)
                .clamp(start, lines.len().max(start)),
        );
    }
    if start > lines.len() || pattern.len() > lines.len().saturating_sub(start) {
        return None;
    }
    let mut matches =
        (start..=lines.len() - pattern.len()).filter(|&i| lines[i..i + pattern.len()] == *pattern);
    match expected {
        Some(expected) => matches.min_by_key(|&i| i.abs_diff(expected)),
        None => matches.next(),
    }
}

/// The 0-based start of a unified hunk's old lines, from `@@ -a,b +c,d @@`.
/// `None` for a bare `@@`, which some tools and models write.
fn unified_old_start(header: &str) -> Option<usize> {
    let old = header.strip_prefix("@@ -")?.split_whitespace().next()?;
    let (start, count) = match old.split_once(',') {
        Some((start, count)) => (start.parse::<usize>().ok()?, count.parse::<usize>().ok()?),
        None => (old.parse::<usize>().ok()?, 1),
    };
    Some(if count == 0 {
        start
    } else {
        start.saturating_sub(1)
    })
}

/// 只在测试里用的故障注入：让备份、替换、回滚里的某一步真的失败。
///
/// 存在的理由是"失败之后说的话是不是真的"没法靠真实 I/O 稳定复现——
/// 权限、磁盘满、并发都做不成可重复的单测，而这恰恰是最需要有测试的地方
/// （审查 A10）。线程局部，不影响并行跑的别的测试。
#[cfg(test)]
pub(crate) mod faults {
    use std::cell::RefCell;
    use std::path::{Path, PathBuf};

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(crate) enum Fault {
        /// 读原文做备份时失败。
        BackupRead(PathBuf),
        /// 把暂存文件换上去时失败。
        Replace(PathBuf),
        /// 回滚写回原内容时失败。
        Restore(PathBuf),
    }

    thread_local! {
        static INJECTED: RefCell<Vec<Fault>> = const { RefCell::new(Vec::new()) };
    }

    /// 装一组故障，返回的守卫在测试结束时清掉它们。
    pub(crate) struct Injected;

    pub(crate) fn inject(faults: Vec<Fault>) -> Injected {
        INJECTED.with(|slot| *slot.borrow_mut() = faults);
        Injected
    }

    impl Drop for Injected {
        fn drop(&mut self) {
            INJECTED.with(|slot| slot.borrow_mut().clear());
        }
    }

    pub(crate) fn hits(fault: &Fault) -> bool {
        INJECTED.with(|slot| slot.borrow().contains(fault))
    }

    pub(crate) fn backup_read_fails(path: &Path) -> bool {
        hits(&Fault::BackupRead(path.to_path_buf()))
    }

    pub(crate) fn replace_fails(path: &Path) -> bool {
        hits(&Fault::Replace(path.to_path_buf()))
    }

    pub(crate) fn restore_fails(path: &Path) -> bool {
        hits(&Fault::Restore(path.to_path_buf()))
    }
}

#[cfg(not(test))]
mod faults {
    use std::path::Path;
    pub(crate) fn backup_read_fails(_path: &Path) -> bool {
        false
    }
    pub(crate) fn replace_fails(_path: &Path) -> bool {
        false
    }
    pub(crate) fn restore_fails(_path: &Path) -> bool {
        false
    }
}

fn commit_staged(
    ws: &Workspace,
    staged: &HashMap<String, Option<String>>,
) -> Result<HashMap<PathBuf, Option<Vec<u8>>>, WorkspaceError> {
    let staged_bytes = staged
        .iter()
        .map(|(path, content)| {
            (
                path.clone(),
                content.as_ref().map(|value| value.as_bytes().to_vec()),
            )
        })
        .collect::<HashMap<_, _>>();
    commit_staged_bytes(ws, &staged_bytes)
}

pub(crate) fn commit_staged_bytes(
    ws: &Workspace,
    staged: &HashMap<String, Option<Vec<u8>>>,
) -> Result<HashMap<PathBuf, Option<Vec<u8>>>, WorkspaceError> {
    let mut backups: HashMap<PathBuf, Option<Vec<u8>>> = HashMap::new();
    let mut temporary_files = HashMap::new();
    // **按路径排序后再落盘**，不按 HashMap 的随机顺序。一批补丁中途失败时，
    // 哪些文件已经换上去、哪些还没有，得是可复现的——否则同一个补丁失败两次
    // 留下的现场可能不一样，人没法照着查（审查 P05 的"错误后的真实状态"）。
    let mut entries: Vec<(&String, &Option<Vec<u8>>)> = staged.iter().collect();
    entries.sort_by(|left, right| left.0.cmp(right.0));
    for (rel, content) in &entries {
        ws.reject_protected_write_path(rel)?;
        let resolved = if content.is_none() {
            ws.resolve_existing(rel)?
        } else {
            ws.resolve_for_write(rel)?
        };
        let path = resolved.path.clone();
        // 读不出原内容就**不要动这个文件**。原来是 `unwrap_or_default()`：
        // 读失败被当成"原来是空的"，一旦后面要回滚，写回去的就是空文件——
        // 本来只是打补丁失败，结果把人家的文件清空了（审查 P05）。
        let backup = if path.exists() && path.is_file() {
            if faults::backup_read_fails(&path) {
                cleanup_temporary_files(temporary_files.values());
                return Err(patch_failed(format!(
                    "cannot read {} to back it up, so nothing was changed",
                    resolved.display
                )));
            }
            match fs::read(&path) {
                Ok(bytes) => Some(bytes),
                Err(err) => {
                    cleanup_temporary_files(temporary_files.values());
                    return Err(patch_failed(format!(
                        "cannot read {} to back it up ({err}), so nothing was changed",
                        resolved.display
                    )));
                }
            }
        } else {
            None
        };
        backups.insert(path.clone(), backup);
        if let Some(bytes) = content {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(|err| patch_failed(err.to_string()))?;
            }
            // 原文件的权限，好让打完补丁的脚本还是可执行的。文件不存在（新增）
            // 时是 None，临时文件就用新建文件的默认权限。
            let mode = fs::metadata(&path).ok().map(|meta| meta.permissions());
            let temp = path.with_file_name(format!(
                ".{}.harness-stage-{}",
                path.file_name().and_then(|v| v.to_str()).unwrap_or("file"),
                Uuid::new_v4().simple()
            ));
            // 共用 ccnm 的落盘写（toexec-fs）：内容 fsync 之后才返回。少了这一步，
            // 后面那次 rename 可能先持久化、内容还没有，断电后留下的是一个名字对、
            // 长度错的文件——看起来是成功的。
            if let Err(err) = toexec_fs::write_durable(&temp, bytes, mode.as_ref()) {
                // 还没有任何文件被换上去，所以没有要回滚的东西。
                cleanup_temporary_files(temporary_files.values());
                return Err(patch_failed(format!(
                    "Failed to stage file: {err}. Nothing was changed"
                )));
            }
            temporary_files.insert(path.clone(), temp);
        }
    }

    // 真正换上去的那些文件。回滚只回滚它们：还没动过的文件不需要"恢复"，
    // 把它们一起写一遍只会制造假的失败，也盖掉别人同时做的改动。
    let mut replaced: Vec<PathBuf> = Vec::new();
    for (rel, content) in &entries {
        let resolved = if content.is_none() {
            ws.resolve_existing(rel)?
        } else {
            ws.resolve_for_write(rel)?
        };
        let path = resolved.path;
        let result = if faults::replace_fails(&path) {
            Err(std::io::Error::other("injected replace failure"))
        } else if content.is_some() {
            let temp = temporary_files
                .get(&path)
                .cloned()
                .ok_or_else(|| patch_failed("Staged file is missing"));
            match temp {
                Ok(temp) => toexec_fs::replace(&temp, &path),
                Err(error) => Err(std::io::Error::other(error.to_string())),
            }
        } else if path.exists() && path.is_file() {
            fs::remove_file(&path)
        } else {
            Ok(())
        };
        match result {
            Ok(()) => replaced.push(path),
            Err(err) => {
                cleanup_temporary_files(temporary_files.values());
                let failed = restore_backups(&backups, &replaced);
                if failed.is_empty() {
                    return Err(patch_failed(format!(
                        "Failed to write file: {err}. Everything this patch had changed was rolled back"
                    )));
                }
                // 回滚也失败了：工作区现在是半新半旧，**不能**说已经回滚。
                // 人得知道去看哪几个文件。
                let names = failed
                    .iter()
                    .map(|path| crate::tools::workspace::relative_display(ws.root(), path))
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(rollback_incomplete(format!(
                    "Failed to write file: {err}, and rolling back did not finish: {names} may now hold this patch's content instead of the original. Check those files before retrying"
                )));
            }
        }
    }
    cleanup_temporary_files(temporary_files.values());
    Ok(backups)
}

/// 把已经换上去的文件恢复成原样，返回**没能恢复的**那些。
///
/// 原来所有错误都被 `let _ =` 吞掉，于是"回滚失败"和"回滚成功"给调用方的
/// 消息一模一样——而这两种情况下工作区完全不同（审查 P05）。
fn restore_backups(
    backups: &HashMap<PathBuf, Option<Vec<u8>>>,
    replaced: &[PathBuf],
) -> Vec<PathBuf> {
    let mut failed = Vec::new();
    for path in replaced {
        let Some(data) = backups.get(path) else {
            continue;
        };
        let result = match data {
            // 原来没有这个文件：回滚就是把新建的删掉。
            None => fs::remove_file(path).or_else(|err| {
                if err.kind() == std::io::ErrorKind::NotFound {
                    Ok(())
                } else {
                    Err(err)
                }
            }),
            Some(bytes) => {
                if faults::restore_fails(path) {
                    Err(std::io::Error::other("injected restore failure"))
                } else {
                    if let Some(parent) = path.parent() {
                        let _ = fs::create_dir_all(parent);
                    }
                    fs::write(path, bytes)
                }
            }
        };
        if result.is_err() {
            failed.push(path.clone());
        }
    }
    failed
}

fn cleanup_temporary_files<'a>(paths: impl Iterator<Item = &'a PathBuf>) {
    for path in paths {
        let _ = fs::remove_file(path);
    }
}

fn is_critical_file(path: &str) -> bool {
    let normalized = path.replace('\\', "/");
    let first = normalized.split('/').next().unwrap_or("");
    if matches!(first, ".git" | ".github") {
        return true;
    }
    let name = normalized.rsplit('/').next().unwrap_or(normalized.as_str());
    name == ".gitignore"
        || name == "Cargo.toml"
        || name == "Cargo.lock"
        || name == "package.json"
        || name == "package-lock.json"
        || name == "pnpm-lock.yaml"
        || name == "tauri.conf.json"
        || name.starts_with("README")
        || name.starts_with("LICENSE")
        || name.starts_with("vite.config.")
        || name == "pyproject.toml"
}

fn is_protected_repository_asset(path: &str) -> bool {
    let normalized = path.replace('\\', "/");
    let first = normalized.split('/').next().unwrap_or("");
    matches!(first, ".git" | ".github")
}

fn dangerous_operation(message: impl Into<String>) -> WorkspaceError {
    WorkspaceError::Tool {
        code: "DANGEROUS_OPERATION_REQUIRES_CONFIRMATION",
        message: message.into(),
        category: "permission",
        retryable: false,
    }
}

fn protected_repository_asset(message: impl Into<String>) -> WorkspaceError {
    WorkspaceError::Tool {
        code: "PROTECTED_REPOSITORY_ASSET",
        message: message.into(),
        category: "security",
        retryable: false,
    }
}

/// 上下文对得上好几处，谁也不能说了算。和 `PATCH_FAILED` 分开：那个是
/// "对不上，重读文件再来"，这个是"对得上太多处，把话说清楚再来"。
fn patch_ambiguous(message: impl Into<String>) -> WorkspaceError {
    WorkspaceError::Tool {
        code: "PATCH_AMBIGUOUS",
        message: message.into(),
        category: "validation",
        retryable: false,
    }
}

/// 打补丁失败**而且**回滚没做干净：工作区现在半新半旧。和 `PATCH_FAILED`
/// 分开，因为下一步完全不同——那个是"改一改再来"，这个是"先去看文件"。
fn rollback_incomplete(message: impl Into<String>) -> WorkspaceError {
    WorkspaceError::Tool {
        code: "PATCH_ROLLBACK_INCOMPLETE",
        message: message.into(),
        category: "runtime",
        retryable: false,
    }
}

fn patch_failed(message: impl Into<String>) -> WorkspaceError {
    WorkspaceError::Tool {
        code: "PATCH_FAILED",
        message: message.into(),
        category: "validation",
        retryable: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::context::ToolContext;
    use serde_json::json;
    use tempfile::tempdir;

    fn context_with_file() -> (tempfile::TempDir, tempfile::TempDir, ToolContext) {
        let workspace = tempdir().expect("workspace");
        let harness = tempdir().expect("harness");
        std::fs::write(workspace.path().join("main.rs"), "old\n").expect("file");
        let context =
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("context");
        (workspace, harness, context)
    }

    fn patch() -> Value {
        json!({
            "patch": "--- a/main.rs\n+++ b/main.rs\n@@\n-old\n+new\n"
        })
    }

    /// 打补丁不该让一个脚本丢掉可执行位。
    ///
    /// 以前会丢：暂存写的是一个全新的临时文件，用默认权限，rename 过去之后
    /// 原来的 0755 就没了，下一次 ./run.sh 直接 Permission denied。现在暂存
    /// 时把原文件的权限带过去（共用的 toexec-fs 负责设）。
    #[cfg(unix)]
    #[test]
    fn patching_a_script_keeps_it_executable() {
        use std::os::unix::fs::PermissionsExt;
        let workspace = tempdir().expect("workspace");
        let harness = tempdir().expect("harness");
        let script = workspace.path().join("run.sh");
        std::fs::write(&script, "#!/bin/sh\nold\n").expect("写脚本");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("设权限");
        let context =
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("context");

        apply_patch(
            &context,
            &json!({ "patch": "--- a/run.sh\n+++ b/run.sh\n@@\n-old\n+new\n" }),
        )
        .expect("apply");

        assert_eq!(
            std::fs::read_to_string(&script).expect("读"),
            "#!/bin/sh\nnew\n"
        );
        let mode = std::fs::metadata(&script)
            .expect("stat")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o755, "权限成了 {:o}", mode & 0o777);
    }

    /// 新增的文件没有"原来的权限"可继承，用默认的就行——尤其不能莫名其妙
    /// 带上可执行位。
    #[cfg(unix)]
    #[test]
    fn a_newly_added_file_gets_the_default_mode() {
        use std::os::unix::fs::PermissionsExt;
        let (workspace, _harness, context) = context_with_file();
        apply_patch(
            &context,
            &json!({ "patch": "--- /dev/null\n+++ b/added.txt\n@@\n+hello\n" }),
        )
        .expect("apply");

        let added = workspace.path().join("added.txt");
        let mode = std::fs::metadata(&added)
            .expect("stat")
            .permissions()
            .mode();
        assert_eq!(mode & 0o111, 0, "新文件不该可执行，实际 {:o}", mode & 0o777);
    }

    #[test]
    fn patch_check_does_not_modify_workspace() {
        let (_workspace, _harness, context) = context_with_file();
        let result = patch_check(&context, &patch()).expect("patch check");
        assert_eq!(result["preflight"], true);
        assert_eq!(
            std::fs::read_to_string(context.workspace.root().join("main.rs")).unwrap(),
            "old\n"
        );
    }

    #[test]
    fn preserves_crlf_when_inserting_multiple_lines() {
        let input = "one\r\ntwo\r\n";
        let hunk = Hunk {
            lines: vec![
                HunkLine::Context("one".into()),
                HunkLine::Add("insert-a".into()),
                HunkLine::Add("insert-b".into()),
                HunkLine::Context("two".into()),
            ],
            ..Hunk::default()
        };
        assert_eq!(
            apply_hunks(input, &[hunk]).expect("patch"),
            "one\r\ninsert-a\r\ninsert-b\r\ntwo\r\n"
        );
    }

    #[test]
    fn delete_then_add_same_path_replaces_instead_of_concatenating_old_content() {
        let (_workspace, _harness, context) = context_with_file();
        let result = apply_patch(
            &context,
            &json!({
                "patch": "*** Begin Patch\n*** Delete File: main.rs\n*** Add File: main.rs\n+fresh\n*** End Patch\n"
            }),
        )
        .expect("replace file");
        assert_eq!(result["files_modified"], json!(["main.rs"]));
        assert_eq!(
            std::fs::read_to_string(context.workspace.root().join("main.rs")).unwrap(),
            "fresh\n"
        );
    }

    #[test]
    fn validation_failure_in_later_file_keeps_all_files_unchanged() {
        let (_workspace, _harness, context) = context_with_file();
        let error = apply_patch(
            &context,
            &json!({
                "patch": "--- a/main.rs\n+++ b/main.rs\n@@\n-old\n+new\n--- a/missing.rs\n+++ b/missing.rs\n@@\n-old\n+new\n"
            }),
        )
        .expect_err("later file fails preflight");
        assert_eq!(error.to_error_value()["code"], "NOT_FOUND");
        assert_eq!(
            std::fs::read_to_string(context.workspace.root().join("main.rs")).unwrap(),
            "old\n"
        );
    }

    fn apply_to(original: &str, patch: &str) -> String {
        let files = parse_unified_diff(patch).expect("parse");
        apply_hunks(original, &files[0].hunks).expect("apply")
    }

    /// The same line twice; the header says which one.
    #[test]
    fn unified_hunk_goes_where_its_line_numbers_say() {
        let original = "fn a() {\n    x = 1;\n}\nfn b() {\n    x = 1;\n}\n";
        let patch = "--- a/m.rs\n+++ b/m.rs\n@@ -5,1 +5,1 @@\n-    x = 1;\n+    x = 2;\n";
        assert_eq!(
            apply_to(original, patch),
            "fn a() {\n    x = 1;\n}\nfn b() {\n    x = 2;\n}\n"
        );
    }

    /// The second hunk's context also matches text before the first hunk.
    #[test]
    fn a_later_hunk_is_never_applied_before_an_earlier_one() {
        let original = "a\nb\na\nb\n";
        let patch = "*** Begin Patch\n*** Update File: m.txt\n@@\n a\n b\n+tail1\n@@\n a\n b\n+tail2\n*** End Patch\n";
        assert_eq!(apply_to(original, patch), "a\nb\ntail1\na\nb\ntail2\n");
    }

    #[test]
    fn codex_context_header_picks_the_block_after_it() {
        let original = "fn a() {\n  x\n}\nfn b() {\n  x\n}\n";
        let patch =
            "*** Begin Patch\n*** Update File: m.rs\n@@ fn b() {\n-  x\n+  y\n*** End Patch\n";
        assert_eq!(
            apply_to(original, patch),
            "fn a() {\n  x\n}\nfn b() {\n  y\n}\n"
        );
    }

    /// `-2,0` means "after line 2"; without context there is nothing else
    /// to go by.
    #[test]
    fn a_pure_insertion_lands_after_the_line_its_header_names() {
        let patch = "--- a/m.txt\n+++ b/m.txt\n@@ -2,0 +3,1 @@\n+inserted\n";
        assert_eq!(apply_to("l1\nl2\nl3\n", patch), "l1\nl2\ninserted\nl3\n");
    }

    /// Line numbers in later hunks refer to the original file; earlier
    /// hunks that added lines shift where that is now.
    #[test]
    fn later_line_numbers_account_for_lines_added_above() {
        let original = "x\nk\nx\nk\nx\n";
        let patch =
            "--- a/m.txt\n+++ b/m.txt\n@@ -1,1 +1,3 @@\n x\n+p\n+q\n@@ -5,1 +7,1 @@\n-x\n+z\n";
        assert_eq!(apply_to(original, patch), "x\np\nq\nk\nx\nk\nz\n");
    }

    // ---- U1：不能静默改坏文件（2026-09-19 的工具审查 P02/P03/P04） ----

    /// `Add File` 指向一个已经存在的文件，今天会被当成整文件覆盖：原内容
    /// 一声不响地没了。应当报错，并告诉调用方该用 `Update File`。
    #[test]
    fn adding_a_file_that_already_exists_is_refused() {
        for patch in [
            "*** Begin Patch\n*** Add File: main.rs\n+fresh\n*** End Patch\n",
            "--- /dev/null\n+++ b/main.rs\n@@\n+fresh\n",
        ] {
            let (_workspace, _harness, context) = context_with_file();
            let error = apply_patch(&context, &json!({ "patch": patch }))
                .expect_err("Add 一个已存在的文件应当被拒");
            let value = error.to_error_value();
            assert_eq!(value["code"], "PATCH_FAILED", "{value}");
            let message = value["message"].as_str().unwrap_or_default();
            assert!(message.contains("main.rs"), "{message}");
            assert!(
                message.contains("Update File"),
                "得说清楚该用哪个：{message}"
            );
            assert_eq!(
                std::fs::read_to_string(context.workspace.root().join("main.rs")).unwrap(),
                "old\n",
                "被拒的补丁不能动文件"
            );
        }
    }

    /// 同一批里两次 Update 同一个文件：第二次今天从磁盘重新读原文，最后
    /// `staged.insert` 把第一次的结果盖掉——第一处改动就这么丢了。
    #[test]
    fn two_updates_to_the_same_file_apply_one_after_another() {
        let workspace = tempdir().expect("workspace");
        let harness = tempdir().expect("harness");
        std::fs::write(workspace.path().join("m.txt"), "a\nb\n").expect("file");
        let context =
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("context");

        apply_patch(
            &context,
            &json!({
                "patch": "*** Begin Patch\n*** Update File: m.txt\n@@\n-a\n+A\n*** Update File: m.txt\n@@\n-b\n+B\n*** End Patch\n"
            }),
        )
        .expect("两次编辑都该应用");

        assert_eq!(
            std::fs::read_to_string(workspace.path().join("m.txt")).unwrap(),
            "A\nB\n",
            "第一次的改动被第二次盖掉了"
        );
    }

    /// Codex 信封少了 `*** End Patch`：今天照样应用。补丁被截断过（网络、
    /// 上下文长度）和补丁写完了，长得一模一样，而前者应用一半就是改坏。
    #[test]
    fn a_codex_patch_without_its_end_marker_is_refused() {
        let (_workspace, _harness, context) = context_with_file();
        let error = apply_patch(
            &context,
            &json!({ "patch": "*** Begin Patch\n*** Update File: main.rs\n@@\n-old\n+new\n" }),
        )
        .expect_err("缺结束标记应当被拒");
        let value = error.to_error_value();
        assert_eq!(value["code"], "PATCH_FAILED", "{value}");
        assert!(
            value["message"]
                .as_str()
                .unwrap_or_default()
                .contains("*** End Patch"),
            "{value}"
        );
        assert_eq!(
            std::fs::read_to_string(context.workspace.root().join("main.rs")).unwrap(),
            "old\n"
        );
    }

    /// 不认识的 `*** ` 指令（比如真 Codex 有、这里没实现的 `*** Move to:`）
    /// 今天被静默跳过：模型以为文件挪了，实际没挪。
    #[test]
    fn an_unsupported_codex_directive_is_refused_instead_of_skipped() {
        let (_workspace, _harness, context) = context_with_file();
        let error = apply_patch(
            &context,
            &json!({
                "patch": "*** Begin Patch\n*** Update File: main.rs\n@@\n-old\n+new\n*** Move to: moved.rs\n*** End Patch\n"
            }),
        )
        .expect_err("不认识的指令应当被拒");
        let value = error.to_error_value();
        assert_eq!(value["code"], "PATCH_FAILED", "{value}");
        assert!(
            value["message"]
                .as_str()
                .unwrap_or_default()
                .contains("*** Move to:"),
            "{value}"
        );
        assert_eq!(
            std::fs::read_to_string(context.workspace.root().join("main.rs")).unwrap(),
            "old\n"
        );
        assert!(!context.workspace.root().join("moved.rs").exists());
    }

    /// 上下文在文件里出现多次、又没有行号也没有锚点时，今天挑第一处改。
    /// 挑错了就是改错地方，而且看起来是成功的。**只在这个 hunk 会删/改行时
    /// 才算歧义**：纯插入挑哪一处都不破坏原有内容，而且现有补丁大量这么写。
    #[test]
    fn an_ambiguous_change_names_the_candidates_instead_of_guessing() {
        let workspace = tempdir().expect("workspace");
        let harness = tempdir().expect("harness");
        std::fs::write(workspace.path().join("m.txt"), "x\ny\nx\n").expect("file");
        let context =
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("context");

        let error = apply_patch(
            &context,
            &json!({
                "patch": "*** Begin Patch\n*** Update File: m.txt\n@@\n-x\n+z\n*** End Patch\n"
            }),
        )
        .expect_err("两处都能匹配，应当报歧义");
        let value = error.to_error_value();
        assert_eq!(value["code"], "PATCH_AMBIGUOUS", "{value}");
        let message = value["message"].as_str().unwrap_or_default();
        assert!(
            message.contains("1") && message.contains("3"),
            "得给出候选行号：{message}"
        );
        assert_eq!(
            std::fs::read_to_string(workspace.path().join("m.txt")).unwrap(),
            "x\ny\nx\n"
        );

        // 给了行号就不再是歧义。
        apply_patch(
            &context,
            &json!({ "patch": "--- a/m.txt\n+++ b/m.txt\n@@ -3,1 +3,1 @@\n-x\n+z\n" }),
        )
        .expect("行号说了是第三行");
        assert_eq!(
            std::fs::read_to_string(workspace.path().join("m.txt")).unwrap(),
            "x\ny\nz\n"
        );
    }

    // ---- U1：失败之后说的话必须是真的（审查 P05、A10） ----

    /// 备份读不出来就别动这个文件。
    ///
    /// 原来是 `unwrap_or_default()`：读失败被当成"原来是空的"，一旦回滚就
    /// 把人家的文件写成空——本来只是打补丁失败。
    #[test]
    fn a_file_whose_backup_cannot_be_read_is_left_alone() {
        let (_workspace, _harness, context) = context_with_file();
        let target = context.workspace.root().join("main.rs");
        let _injected = faults::inject(vec![faults::Fault::BackupRead(
            target.canonicalize().unwrap_or(target.clone()),
        )]);

        let error = apply_patch(&context, &patch()).expect_err("备份读不了就该停下");
        let value = error.to_error_value();
        assert_eq!(value["code"], "PATCH_FAILED", "{value}");
        let message = value["message"].as_str().unwrap_or_default();
        assert!(message.contains("back it up"), "{message}");
        assert!(message.contains("nothing was changed"), "{message}");
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "old\n",
            "文件不该被动，更不该被清空"
        );
    }

    /// 一个文件换上去了、另一个换失败：回滚成功时要说清楚"都回滚了"。
    #[test]
    fn a_failed_write_rolls_the_earlier_files_back() {
        let workspace = tempdir().expect("workspace");
        let harness = tempdir().expect("harness");
        std::fs::write(workspace.path().join("a.txt"), "a-old\n").expect("a");
        std::fs::write(workspace.path().join("b.txt"), "b-old\n").expect("b");
        let context =
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("context");
        let b = workspace
            .path()
            .join("b.txt")
            .canonicalize()
            .expect("canonical b");
        let _injected = faults::inject(vec![faults::Fault::Replace(b)]);

        let error = apply_patch(
            &context,
            &json!({
                "patch": "--- a/a.txt\n+++ b/a.txt\n@@\n-a-old\n+a-new\n--- a/b.txt\n+++ b/b.txt\n@@\n-b-old\n+b-new\n"
            }),
        )
        .expect_err("第二个文件写失败");
        let value = error.to_error_value();
        assert_eq!(value["code"], "PATCH_FAILED", "{value}");
        assert!(
            value["message"]
                .as_str()
                .unwrap_or_default()
                .contains("rolled back"),
            "{value}"
        );
        assert_eq!(
            std::fs::read_to_string(workspace.path().join("a.txt")).unwrap(),
            "a-old\n",
            "第一个文件该回滚回去"
        );
        assert_eq!(
            std::fs::read_to_string(workspace.path().join("b.txt")).unwrap(),
            "b-old\n"
        );
    }

    /// 回滚**也**失败：绝不能说"已回滚"，要点名是哪些文件，让人去看。
    #[test]
    fn a_rollback_that_fails_says_which_files_are_in_doubt() {
        let workspace = tempdir().expect("workspace");
        let harness = tempdir().expect("harness");
        std::fs::write(workspace.path().join("a.txt"), "a-old\n").expect("a");
        std::fs::write(workspace.path().join("b.txt"), "b-old\n").expect("b");
        let context =
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("context");
        let a = workspace
            .path()
            .join("a.txt")
            .canonicalize()
            .expect("canonical a");
        let b = workspace
            .path()
            .join("b.txt")
            .canonicalize()
            .expect("canonical b");
        let _injected = faults::inject(vec![faults::Fault::Replace(b), faults::Fault::Restore(a)]);

        let error = apply_patch(
            &context,
            &json!({
                "patch": "--- a/a.txt\n+++ b/a.txt\n@@\n-a-old\n+a-new\n--- a/b.txt\n+++ b/b.txt\n@@\n-b-old\n+b-new\n"
            }),
        )
        .expect_err("写失败且回滚失败");
        let value = error.to_error_value();
        assert_eq!(value["code"], "PATCH_ROLLBACK_INCOMPLETE", "{value}");
        let message = value["message"].as_str().unwrap_or_default();
        assert!(message.contains("a.txt"), "得点名是哪个文件：{message}");
        assert!(!message.contains("was rolled back"), "{message}");
        // 现场就是半新半旧，这正是要告诉人的：a 已经是新的了。
        assert_eq!(
            std::fs::read_to_string(workspace.path().join("a.txt")).unwrap(),
            "a-new\n"
        );
    }

    #[test]
    fn a_context_header_that_is_not_in_the_file_fails() {
        let files = parse_unified_diff(
            "*** Begin Patch\n*** Update File: m.rs\n@@ fn missing() {\n-x\n+y\n*** End Patch\n",
        )
        .expect("parse");
        let error = apply_hunks("x\n", &files[0].hunks).expect_err("no such block");
        assert_eq!(error.to_error_value()["code"], "PATCH_FAILED");
    }
}
