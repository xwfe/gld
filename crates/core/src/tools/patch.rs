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

    let mut affected = Vec::new();
    let mut summaries = Vec::new();
    let mut staged: HashMap<String, Option<String>> = HashMap::new();

    for fp in &file_patches {
        ws.reject_unsafe_text(&fp.path)?;
        let resolved = if fp.is_new_file {
            ws.resolve_for_write(&fp.path)?
        } else {
            ws.resolve_existing(&fp.path)?
        };
        ws.reject_write_symlink(&fp.path)?;

        let original = if fp.is_new_file {
            // An Add File envelope is replacement content even when an earlier
            // Delete File for the same path exists in this transaction.
            String::new()
        } else if resolved.existed {
            fs::read_to_string(&resolved.path)
                .map_err(|_| WorkspaceError::not_found(format!("File not found: {}", fp.path)))?
        } else if fp.is_new_file || fp.is_deleted {
            String::new()
        } else {
            return Err(patch_failed(format!("File not found: {}", fp.path)));
        };

        if fp.is_deleted {
            staged.insert(resolved.display.clone(), None);
            affected.push(json!({ "path": resolved.display, "operation": "delete" }));
            summaries.push(format!("D {}", resolved.display));
            continue;
        }

        let updated = apply_hunks(&original, &fp.hunks)?;
        let op = if resolved.existed { "update" } else { "add" };
        staged.insert(resolved.display.clone(), Some(updated));
        affected.push(json!({ "path": resolved.display, "operation": op }));
        summaries.push(format!(
            "{} {}",
            if op == "add" { "A" } else { "M" },
            resolved.display
        ));
    }

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

    for raw_line in patch.lines() {
        let line = raw_line.trim_end_matches('\r');
        if line == "*** Begin Patch" {
            continue;
        }
        if line == "*** End Patch" {
            finish_codex_file(&mut files, &mut current, &mut current_hunk);
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
    for (rel, content) in staged {
        ws.reject_protected_write_path(rel)?;
        let resolved = if content.is_none() {
            ws.resolve_existing(rel)?
        } else {
            ws.resolve_for_write(rel)?
        };
        let path = resolved.path.clone();
        backups.insert(
            path.clone(),
            if path.exists() && path.is_file() {
                Some(fs::read(&path).unwrap_or_default())
            } else {
                None
            },
        );
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
                cleanup_temporary_files(temporary_files.values());
                restore_backups(&backups);
                return Err(patch_failed(format!("Failed to stage file: {err}")));
            }
            temporary_files.insert(path.clone(), temp);
        }
    }

    for (rel, content) in staged {
        let resolved = if content.is_none() {
            ws.resolve_existing(rel)?
        } else {
            ws.resolve_for_write(rel)?
        };
        let path = resolved.path;
        let result = if content.is_some() {
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
        if let Err(err) = result {
            cleanup_temporary_files(temporary_files.values());
            restore_backups(&backups);
            return Err(patch_failed(format!("Failed to write file: {err}")));
        }
    }
    cleanup_temporary_files(temporary_files.values());
    Ok(backups)
}

fn restore_backups(backups: &HashMap<PathBuf, Option<Vec<u8>>>) {
    for (path, data) in backups {
        match data {
            None => {
                let _ = fs::remove_file(path);
            }
            Some(bytes) => {
                if let Some(parent) = path.parent() {
                    let _ = fs::create_dir_all(parent);
                }
                let _ = fs::write(path, bytes);
            }
        }
    }
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
