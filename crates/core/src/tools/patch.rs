use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;

use serde_json::{json, Value};
use uuid::Uuid;

use crate::tools::context::ToolContext;
use crate::tools::patch_diag::{Diagnostic, HunkMiss, MAX_CANDIDATES, MAX_DIAGNOSTICS};
use crate::tools::workspace::{tool_ok, FileState, Workspace, WorkspaceError};

pub fn apply_patch(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    let ws = &ctx.workspace;
    // notebook 按 cell 改走另一个参数：补丁是文本信封，塞不进"第几个 cell 换
    // 成什么"这种结构。两者在**同一次事务**里——多文件原子提交、备份回滚、
    // 版本前置条件、进程内写锁全都照样管着它（RFC-0003 G3.2）。
    let notebook_edits = crate::tools::notebook::notebook_edits(args)?;
    let patch = match args.get("patch").and_then(Value::as_str) {
        Some(patch) => patch,
        None if notebook_edits.is_some() => "",
        None => return Err(WorkspaceError::invalid_argument("patch is required")),
    };
    let dry_run = args
        .get("dry_run")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let confirm = args
        .get("confirm")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let expected_versions = expected_versions(args)?;

    // 真要落盘的那一路，从这里开始独占这个工作区，直到函数结束。
    //
    // 挡的是 gld 自己的并发写者：两个 MCP 会话同时打补丁，一个读原文、另一个
    // 正在落盘，第一个算出来的新内容就是基于已经过期的原文。dry_run 不写盘，
    // 不占这把锁——预检不该让真正的写操作排队。
    //
    // 锁是按**目录**取的，不是每个上下文一把，也不是整个进程一把：同一个目录
    // 经 hub、单工作区 listener、CLI 进来是三个 `ToolContext`，得排同一个队；
    // 两个不同的项目则各写各的，不该互相堵。这把锁管不到别的进程、管不到
    // `exec` 跑的命令、也管不到 Git 元数据——那几条边界写在
    // `crate::tools::workspace_runtime` 的模块文档里，那一侧靠的是下面的版本
    // 前置条件和落盘前复核，两者都不是强 CAS，见 `commit_staged_bytes`。
    let _commit_guard = (!dry_run).then(|| ctx.runtime.lock_commits());

    let file_patches = if patch.is_empty() {
        Vec::new()
    } else {
        parse_unified_diff(patch)?
    };
    if file_patches.is_empty() && notebook_edits.is_none() {
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
    // 对不上的地方一次报清楚，而不是报第一个就走。一个文件里第一段失败之后
    // 就不再检查这个文件剩下的段——它们要看见前一段的结果才知道对不对，接着
    // 检查只会连锁误报；那些段记进 `not_checked`，不能算作通过（审查 C2）。
    let mut diagnostics: Vec<Diagnostic> = Vec::new();
    let mut failed_files: HashSet<String> = HashSet::new();
    let mut not_checked: Vec<Value> = Vec::new();
    // 算这批补丁时，每个文件在磁盘上**是什么样**。落盘前再核一遍：不一样就
    // 说明算完之后有人动过它，这时候写下去就是把人家的改动盖掉（审查 C3）。
    let mut baselines: HashMap<String, Baseline> = HashMap::new();
    // 每个被碰到的文件当时的版本号，patch_check 会把它交回给模型。
    let mut observed: serde_json::Map<String, Value> = serde_json::Map::new();

    for fp in &file_patches {
        ws.reject_unsafe_text(&fp.path)?;
        // 这一批里已经排过队的改动，才是这个文件"现在"的样子：同一批里
        // 先 Add 再 Update、或者两次 Update 同一个文件，第二段要看见第一段
        // 的结果。以前每段都重新读磁盘，最后一段 insert 覆盖前面的，前面
        // 那次编辑就这么没了（审查 P03）。
        let write_display = ws.resolve_for_write(&fp.path)?.display;
        if failed_files.contains(&write_display) {
            not_checked.push(json!({
                "file": write_display,
                "reason_code": "earlier_failure_in_same_file"
            }));
            continue;
        }
        let staged_before = staged.get(&write_display).cloned();
        let resolved = if fp.is_new_file || matches!(staged_before, Some(Some(_))) {
            ws.resolve_for_write(&fp.path)?
        } else {
            match ws.resolve_existing(&fp.path) {
                Ok(resolved) => resolved,
                // 要改的文件根本不在：这是"改错了目标"，和权限、越界不是
                // 一回事，收进诊断，剩下的文件接着检查。
                Err(error) if error.code() == "NOT_FOUND" => {
                    diagnostics.push(Diagnostic::file_level(
                        // 错误码还是 NOT_FOUND：这条一直就是这么报的，改码等于
                        // 让照着旧码分支的客户端突然走进 else。带上的诊断是新的。
                        "NOT_FOUND",
                        "file_not_found",
                        format!(
                            "File not found: {}; use *** Add File: to create it",
                            fp.path
                        ),
                        fp.path.clone(),
                        if fp.is_deleted { "delete" } else { "update" },
                    ));
                    failed_files.insert(write_display);
                    continue;
                }
                Err(error) => return Err(error),
            }
        };
        ws.reject_write_symlink(&fp.path)?;

        // 这个文件现在是什么状态。第一次碰到它的时候记下来：给 patch_check
        // 交回给模型，也当作落盘前复核的基线。
        if !observed.contains_key(&resolved.display) {
            let state = crate::tools::workspace::file_state(&resolved.path);
            if let Some(diagnostic) = version_precondition(
                &resolved.display,
                operation_label(fp),
                &state,
                &expected_versions,
            ) {
                diagnostics.push(diagnostic);
                failed_files.insert(resolved.display.clone());
                continue;
            }
            observed.insert(
                resolved.display.clone(),
                version_value(state_version(&state)),
            );
            baselines.insert(resolved.display.clone(), Baseline::from_state(&state));
        }

        // Add 是"新建"，不是"覆盖"。同一批里先 Delete 过它则另说：那是
        // 明写出来的整文件替换，有测试钉着。
        if fp.is_new_file
            && (resolved.existed || matches!(staged_before, Some(Some(_))))
            && !matches!(staged_before, Some(None))
        {
            diagnostics.push(Diagnostic::file_level(
                "PATCH_FAILED",
                "file_already_exists",
                format!(
                    "{} already exists; use *** Update File: {} to change it, or delete it first",
                    resolved.display, resolved.display
                ),
                resolved.display.clone(),
                "add",
            ));
            failed_files.insert(resolved.display.clone());
            continue;
        }

        let original = match &staged_before {
            // 这一批里刚删过：Add 从空白开始，别的操作没有文件可改。
            Some(None) if fp.is_new_file => String::new(),
            Some(None) => {
                diagnostics.push(Diagnostic::file_level(
                    "PATCH_FAILED",
                    "deleted_earlier_in_patch",
                    format!(
                        "{} was deleted earlier in this patch; add it again instead of editing it",
                        resolved.display
                    ),
                    resolved.display.clone(),
                    "update",
                ));
                failed_files.insert(resolved.display.clone());
                continue;
            }
            Some(Some(text)) if fp.is_new_file => {
                let _ = text;
                String::new()
            }
            Some(Some(text)) => text.clone(),
            None if fp.is_new_file => String::new(),
            None if resolved.existed => match fs::read_to_string(&resolved.path) {
                Ok(text) => text,
                Err(error) => {
                    diagnostics.push(Diagnostic::file_level(
                        "PATCH_FAILED",
                        "file_unreadable",
                        format!("cannot read {} ({error})", resolved.display),
                        resolved.display.clone(),
                        "update",
                    ));
                    failed_files.insert(resolved.display.clone());
                    continue;
                }
            },
            None if fp.is_deleted => String::new(),
            None => {
                diagnostics.push(Diagnostic::file_level(
                    "NOT_FOUND",
                    "file_not_found",
                    format!("File not found: {}", fp.path),
                    fp.path.clone(),
                    "update",
                ));
                failed_files.insert(resolved.display.clone());
                continue;
            }
        };
        // 原文是刚从磁盘读上来的：拿它当基线比版本号更结实——版本号只是
        // 大小加修改时间，内容能一个字节一个字节地比。同一批里第二次碰这个
        // 文件时 `original` 是上一段的结果，不是磁盘内容，那时不能换。
        if staged_before.is_none() && !fp.is_new_file && resolved.existed {
            baselines.insert(
                resolved.display.clone(),
                Baseline::Content(original.as_bytes().to_vec()),
            );
        }

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

        // 文件本来就在盘上就是改，不在就是新建——先删后加的净效果因此是
        // "改"，这一点有测试钉着。
        let op = if resolved.existed { "update" } else { "add" };
        let updated = match apply_hunks(&original, &fp.hunks) {
            Ok(text) => text,
            Err(miss) => {
                // 拿什么当原文比的：同一批里前面改过这个文件，行号就和磁盘上
                // 那份对不上，得说清楚，否则模型会以为 read_file 读错了。
                let baseline = if staged_before.is_some() {
                    "earlier_in_this_patch"
                } else {
                    "file_on_disk"
                };
                diagnostics.push(Diagnostic::from_hunk_miss(
                    resolved.display.clone(),
                    op,
                    baseline,
                    &original,
                    miss,
                ));
                failed_files.insert(resolved.display.clone());
                continue;
            }
        };
        staged.insert(resolved.display.clone(), Some(updated));
        if operations.insert(resolved.display.clone(), op).is_none() {
            order.push(resolved.display.clone());
        }
    }

    // notebook 的 cell 编辑：和上面的补丁在同一批里排队，共用同一套版本核对、
    // 落盘和回滚。
    for edits in notebook_edits.iter().flatten() {
        ws.reject_unsafe_text(&edits.path)?;
        let resolved = match ws.resolve_existing(&edits.path) {
            Ok(resolved) => resolved,
            Err(error) if error.code() == "NOT_FOUND" => {
                diagnostics.push(Diagnostic::file_level(
                    "NOT_FOUND",
                    "file_not_found",
                    format!(
                        "File not found: {}; notebook_edits changes an existing notebook, it does not create one",
                        edits.path
                    ),
                    edits.path.clone(),
                    "update",
                ));
                continue;
            }
            Err(error) => return Err(error),
        };
        ws.reject_write_symlink(&edits.path)?;
        // 同一个文件既走补丁又走 cell 编辑：拒。两种改法对"原文是什么"的理解
        // 不一样，混在一起的结果没人说得清。
        if staged.contains_key(&resolved.display) {
            return Err(WorkspaceError::invalid_argument(format!(
                "{} is changed by both patch and notebook_edits in one call; send them separately",
                resolved.display
            )));
        }

        let state = crate::tools::workspace::file_state(&resolved.path);
        if let Some(diagnostic) =
            version_precondition(&resolved.display, "update", &state, &expected_versions)
        {
            diagnostics.push(diagnostic);
            continue;
        }
        observed.insert(
            resolved.display.clone(),
            version_value(state_version(&state)),
        );

        let original = match fs::read(&resolved.path) {
            Ok(bytes) => bytes,
            Err(error) => {
                diagnostics.push(Diagnostic::file_level(
                    "PATCH_FAILED",
                    "file_unreadable",
                    format!("cannot read {} ({error})", resolved.display),
                    resolved.display.clone(),
                    "update",
                ));
                continue;
            }
        };
        baselines.insert(
            resolved.display.clone(),
            Baseline::Content(original.clone()),
        );
        let updated = match crate::tools::notebook::edit(&original, &resolved.display, &edits.cells)
        {
            Ok(bytes) => bytes,
            Err(error) => {
                diagnostics.push(Diagnostic::file_level(
                    error.code(),
                    "notebook_edit_failed",
                    error.message(),
                    resolved.display.clone(),
                    "update",
                ));
                continue;
            }
        };
        // 写回去的是 serde_json 序列化的结果，一定是 UTF-8。
        let text = String::from_utf8(updated).map_err(|_| {
            patch_failed(format!("{} did not serialize as UTF-8", resolved.display))
        })?;
        staged.insert(resolved.display.clone(), Some(text));
        if operations
            .insert(resolved.display.clone(), "update")
            .is_none()
        {
            order.push(resolved.display.clone());
        }
    }

    if !diagnostics.is_empty() {
        return Err(patch_diagnostics(diagnostics, not_checked));
    }

    // 给了前置条件、补丁里却没有这个文件：报错，不是忽略。忽略的话模型以为
    // 自己保护住了 `a.rs`，而这次调用根本没碰它——它会把这当成"检查过了"。
    if let Some(expected) = expected_versions.as_ref() {
        let unknown = expected
            .keys()
            .filter(|path| !observed.contains_key(path.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        if !unknown.is_empty() {
            return Err(WorkspaceError::invalid_argument(format!(
                "expected_versions names files this patch does not touch: {}. Pass back what patch_check returned in observed_versions, with the same paths",
                unknown.join(", ")
            )));
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
        let _transaction_backups = commit_staged(ws, &staged, &baselines)?;
        let change_id = Uuid::new_v4().simple().to_string();
        // 落盘之后的新版本：接着改同一个文件时原样传回 expected_versions，
        // 不用再 read_file 一遍。
        let new_versions = order
            .iter()
            .map(|path| {
                let version = ws.resolve_for_write(path).ok().and_then(|resolved| {
                    crate::tools::workspace::current_file_version(&resolved.path)
                });
                (path.clone(), version_value(version))
            })
            .collect::<serde_json::Map<_, _>>();
        return Ok(tool_ok(json!({
            "dry_run": false,
            "clean": true,
            "change_id": change_id,
            "summary": summaries.join("\n"),
            "affected_files": affected,
            "files_created": files_created,
            "files_modified": files_modified,
            "files_deleted": files_deleted,
            "file_versions": new_versions,
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
        // 预检看到的版本。原样交给 apply_patch 的 expected_versions，中间
        // 有人动过这些文件就会被拒——**预检通过不是通行证**，它只说明"刚才
        // 这一刻能过"（审查 C3）。
        "observed_versions": observed,
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

/// 改后文件里的 0-based 行号，换算回**原文件**的 1-based 行号。
///
/// 前面几段 hunk 已经把下面的行推上推下了（`shift` 就是净增减），而模型
/// 拿到行号是要去 `read_file` 磁盘上那份文件的，不换算就对不上。
fn to_original_line(index: usize, shift: isize) -> usize {
    ((index as isize - shift).max(0) as usize) + 1
}

fn apply_hunks(original: &str, hunks: &[Hunk]) -> Result<String, HunkMiss> {
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

    for (hunk_index, hunk) in hunks.iter().enumerate() {
        if let Some(anchor) = &hunk.anchor {
            let Some(at) = lines[search_from..]
                .iter()
                .position(|line| line.trim() == anchor)
            else {
                return Err(HunkMiss {
                    code: "PATCH_FAILED",
                    reason_code: "anchor_not_found",
                    message: format!("Hunk header did not match any line: @@ {anchor}"),
                    hunk_index,
                    expected_range: None,
                    candidate_ranges: Vec::new(),
                    center_line: to_original_line(search_from, shift),
                });
            };
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
        //
        // 错误码和 `PATCH_FAILED` 分开，因为下一步不一样：那个是"对不上，
        // 重读文件再来"，这个是"对得上太多处，把话说清楚再来"。
        if expected.is_none()
            && hunk.anchor.is_none()
            && hunk.lines.iter().any(|l| matches!(l, HunkLine::Remove(_)))
        {
            let candidates = match_positions(&lines, &hunk_old, search_from, MAX_CANDIDATES);
            if candidates.len() > 1 {
                let places = candidates
                    .iter()
                    .map(|line| to_original_line(*line, shift).to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(HunkMiss {
                    code: "PATCH_AMBIGUOUS",
                    reason_code: "context_ambiguous",
                    message: format!(
                        "this hunk's context matches more than one place (lines {places}); add surrounding lines or a @@ -line,count @@ header so it is clear which one to change"
                    ),
                    hunk_index,
                    expected_range: None,
                    candidate_ranges: ranges_at(&candidates, hunk_old.len(), shift),
                    center_line: to_original_line(candidates[0], shift),
                });
            }
        }
        let Some(pos) = find_hunk_position(&lines, &hunk_old, search_from, expected) else {
            return Err(context_miss(
                &lines,
                &hunk_old,
                search_from,
                expected,
                shift,
                hunk_index,
            ));
        };

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

/// 一组命中位置（改后 0-based）变成原文件的 1-based 闭区间。
fn ranges_at(positions: &[usize], length: usize, shift: isize) -> Vec<(usize, usize)> {
    positions
        .iter()
        .map(|position| {
            let start = to_original_line(*position, shift);
            (start, start + length.saturating_sub(1))
        })
        .collect()
}

/// 上下文没对上时，把"它可能在哪儿"找出来。
///
/// 顺着找三层，越往后越松：
/// 1. 整段上下文在文件里别的地方——多半是前面的 hunk 顺序写反了；
/// 2. 上下文的第一行还在，只是漂走了——文件被人改过，行号过期；
/// 3. 什么都找不到——这段补丁和这个文件对不上，得整段重写。
///
/// 三种情况下模型该做的事不一样，所以 reason_code 要分开：原来一律
/// `Hunk context did not match file content.`，连是哪个文件都没有（审查 E03）。
fn context_miss(
    lines: &[String],
    hunk_old: &[String],
    search_from: usize,
    expected: Option<usize>,
    shift: isize,
    hunk_index: usize,
) -> HunkMiss {
    let expected_range = expected.map(|start| {
        let start = to_original_line(start, shift);
        (start, start + hunk_old.len().saturating_sub(1))
    });

    let elsewhere = match_positions(lines, hunk_old, 0, MAX_CANDIDATES);
    if !elsewhere.is_empty() {
        let ranges = ranges_at(&elsewhere, hunk_old.len(), shift);
        let places = ranges
            .iter()
            .map(|(start, _)| start.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        return HunkMiss {
            code: "PATCH_FAILED",
            reason_code: "context_out_of_order",
            message: format!(
                "this hunk's context is in the file (around line {places}) but not after the previous hunk; hunks must be in file order"
            ),
            hunk_index,
            expected_range,
            center_line: ranges[0].0,
            candidate_ranges: ranges,
        };
    }

    // 退一步：整段对不上，那第一行呢。找得到就说明文件被改过、行号过期，
    // 模型重读那一段就能修好；找不到才是真的对不上。
    let first_line = hunk_old.iter().find(|line| !line.trim().is_empty());
    let drifted = first_line
        .map(|needle| {
            lines
                .iter()
                .enumerate()
                .filter(|(_, line)| line.trim() == needle.trim())
                .map(|(index, _)| index)
                .take(MAX_CANDIDATES)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if !drifted.is_empty() {
        let ranges = ranges_at(&drifted, hunk_old.len(), shift);
        let places = ranges
            .iter()
            .map(|(start, _)| start.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        return HunkMiss {
            code: "PATCH_FAILED",
            reason_code: "context_drifted",
            message: format!(
                "this hunk's context did not match; its first line is still there (line {places}), so the rest has changed since the patch was written. Re-read that range and rebuild this hunk"
            ),
            hunk_index,
            expected_range,
            center_line: ranges[0].0,
            candidate_ranges: ranges,
        };
    }

    HunkMiss {
        code: "PATCH_FAILED",
        reason_code: "context_not_found",
        message: "this hunk's context is nowhere in the file; re-read the file and rebuild this hunk from what is actually there".into(),
        hunk_index,
        expected_range,
        candidate_ranges: Vec::new(),
        center_line: expected_range
            .map(|(start, _)| start)
            .unwrap_or_else(|| to_original_line(search_from, shift)),
    }
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

    thread_local! {
        static BEFORE_COMMIT: RefCell<Option<Box<dyn Fn()>>> = const { RefCell::new(None) };
    }

    /// 在"算完补丁"和"开始落盘"之间插一手。
    ///
    /// 专门用来复现那个最难测也最要紧的窗口：补丁算完之后、写下去之前，
    /// 别的进程（编辑器、另一个 gld、git checkout）改了同一个文件。靠真实
    /// 并发碰运气是测不稳的。
    pub(crate) struct CommitHook;

    pub(crate) fn before_commit_do(action: impl Fn() + 'static) -> CommitHook {
        BEFORE_COMMIT.with(|slot| *slot.borrow_mut() = Some(Box::new(action)));
        CommitHook
    }

    impl Drop for CommitHook {
        fn drop(&mut self) {
            BEFORE_COMMIT.with(|slot| *slot.borrow_mut() = None);
        }
    }

    pub(crate) fn run_before_commit() {
        let action = BEFORE_COMMIT.with(|slot| slot.borrow_mut().take());
        if let Some(action) = action {
            action();
            BEFORE_COMMIT.with(|slot| *slot.borrow_mut() = Some(action));
        }
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
    pub(crate) fn run_before_commit() {}
}

/// 算这批补丁的时候，一个文件在磁盘上是什么样。落盘前拿它再核一遍。
#[derive(Debug, Clone)]
pub(crate) enum Baseline {
    /// 内容就是这些字节（改文件时原文是现成的，比版本号结实）。
    Content(Vec<u8>),
    /// 只记了版本（删除：没必要为了校验把整个文件读进来）。
    Version(String),
    /// 那时这个路径上什么都没有（新建）。
    Absent,
}

impl Baseline {
    fn from_state(state: &FileState) -> Self {
        match state {
            FileState::Present(version) => Self::Version(version.clone()),
            FileState::Absent => Self::Absent,
            // 进不到这儿：状态问不出来的文件在前面就被拒了。
            FileState::Unknown(_) => Self::Absent,
        }
    }

    /// 磁盘上现在这一份，还是算补丁时的那一份吗。
    ///
    /// `on_disk` 是刚读出来的内容；路径上没有文件时是 `None`。
    fn check(
        &self,
        display: &str,
        path: &std::path::Path,
        on_disk: Option<&[u8]>,
    ) -> Result<(), WorkspaceError> {
        match self {
            Self::Content(expected) => match on_disk {
                Some(actual) if actual == expected.as_slice() => Ok(()),
                Some(_) => Err(version_conflict(format!(
                    "{display} changed while this patch was being prepared; nothing was written. Read it again and rebuild the patch"
                ))),
                None => Err(version_conflict(format!(
                    "{display} was removed while this patch was being prepared; nothing was written"
                ))),
            },
            Self::Version(expected) => {
                match crate::tools::workspace::file_state(path) {
                    // 删掉的东西已经不在了：结果一样，不算冲突。
                    FileState::Absent => Ok(()),
                    FileState::Present(actual) if &actual == expected => Ok(()),
                    FileState::Present(actual) => Err(version_conflict(format!(
                        "{display} changed while this patch was being prepared (version {expected} is now {actual}); nothing was written"
                    ))),
                    // 问不出来就不写：说不清的时候动手，正是这道门要拦的。
                    FileState::Unknown(reason) => Err(version_conflict(format!(
                        "cannot tell the state of {display} ({reason}) before writing it; nothing was written"
                    ))),
                }
            }
            Self::Absent => match on_disk {
                None if !path.exists() => Ok(()),
                // 新建的目标在这中间冒出来了：**不覆盖**。模型以为自己在建
                // 一个新文件，实际会盖掉别人刚放进来的东西（审查 A09）。
                _ => Err(version_conflict(format!(
                    "{display} appeared while this patch was being prepared; it would have been overwritten, so nothing was written. Read it and decide whether to update it instead"
                ))),
            },
        }
    }
}

fn commit_staged(
    ws: &Workspace,
    staged: &HashMap<String, Option<String>>,
    baselines: &HashMap<String, Baseline>,
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
    commit_staged_bytes(ws, &staged_bytes, baselines)
}

pub(crate) fn commit_staged_bytes(
    ws: &Workspace,
    staged: &HashMap<String, Option<Vec<u8>>>,
    baselines: &HashMap<String, Baseline>,
) -> Result<HashMap<PathBuf, Option<Vec<u8>>>, WorkspaceError> {
    faults::run_before_commit();
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
        // 落盘前最后一次核对：这个文件还是算补丁时的那一份吗。
        //
        // 备份刚刚把它读上来，比一比几乎不要钱，而它挡住的是最坏的一种失败——
        // 补丁算完之后有人动了文件，照写下去就是把那次改动无声盖掉。这里发现
        // 冲突时**还没有任何文件被换上去**（换上去是下面第二轮的事），所以
        // 一个字节都没改，也没有要回滚的东西（审查 C3、A09）。
        if let Some(baseline) = baselines.get(*rel) {
            if let Err(error) = baseline.check(&resolved.display, &path, backup.as_deref()) {
                cleanup_temporary_files(temporary_files.values());
                return Err(error);
            }
        }
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
        let result = if content.is_some() {
            let temp = temporary_files
                .get(&path)
                .cloned()
                .ok_or_else(|| patch_failed("Staged file is missing"));
            match temp {
                // 注入失败的办法是把暂存文件换成一个不存在的路径，让
                // `toexec_fs::replace` **真的**失败一次。以前是在调用之前直接
                // 返回 Err，那样共享库根本没被调到——它在 Windows 上曾经先
                // 删目标再 rename、失败就把旧文件弄丢（跨仓评审 X01），这条
                // 路径上的测试却照样是绿的。
                Ok(temp) => {
                    let staged = if faults::replace_fails(&path) {
                        temp.with_file_name("this-staged-file-does-not-exist")
                    } else {
                        temp
                    };
                    toexec_fs::replace(&staged, &path)
                }
                Err(error) => Err(std::io::Error::other(error.to_string())),
            }
        } else if faults::replace_fails(&path) {
            Err(std::io::Error::other("injected replace failure"))
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

/// 把收集到的诊断变成一个错误。
///
/// 整体错误码取第一条：`PATCH_AMBIGUOUS` 和 `PATCH_FAILED` 的下一步不一样，
/// 而多条失败时第一条才是模型最先要修的那个。消息里点名文件和第几段——
/// 原来只有一句 `Hunk context did not match file content.`，模型只能靠重读
/// 整个项目去猜（审查 P01、E03）。
fn patch_diagnostics(diagnostics: Vec<Diagnostic>, not_checked: Vec<Value>) -> WorkspaceError {
    let first = diagnostics.first().expect("at least one diagnostic");
    let code = first.code;
    let mut message = match first.hunk_index {
        Some(index) => format!("{}: hunk #{}: {}", first.file, index + 1, first.message),
        None => format!("{}: {}", first.file, first.message),
    };
    if diagnostics.len() > 1 {
        message.push_str(&format!(
            " ({} problems in this patch; see details.diagnostics)",
            diagnostics.len()
        ));
    }
    let truncated = diagnostics.len() > MAX_DIAGNOSTICS;
    let listed = diagnostics
        .iter()
        .take(MAX_DIAGNOSTICS)
        .map(Diagnostic::to_value)
        .collect::<Vec<_>>();
    WorkspaceError::ToolDetails {
        code,
        message,
        // 分类跟着错误码走：NOT_FOUND 一直是 not_found 类，别因为它现在从
        // 补丁诊断里出来就换一个类别。
        category: match code {
            "NOT_FOUND" => "not_found",
            "FILE_VERSION_CONFLICT" => "conflict",
            _ => "validation",
        },
        retryable: false,
        details: json!({
            "stage": "patch",
            // 校验失败一律整批不落盘。说出来，模型才不会先去"收拾现场"。
            "files_changed": false,
            "problem_count": diagnostics.len(),
            "diagnostics": listed,
            "diagnostics_truncated": truncated,
            "not_checked": not_checked,
            "suggestion": "按 diagnostics[].suggested_read_range 重读这些文件，照它们现在的样子重建对不上的那几段，再把整个补丁重新提交"
        }),
    }
}

/// 文件在这之间被写过。和 `PATCH_FAILED` 分开：那个是补丁自己写错了，
/// 这个是补丁没错而世界变了——下一步是重读文件，不是琢磨 hunk。
fn version_conflict(message: impl Into<String>) -> WorkspaceError {
    WorkspaceError::Tool {
        code: "FILE_VERSION_CONFLICT",
        message: message.into(),
        category: "conflict",
        retryable: false,
    }
}

fn version_value(version: Option<String>) -> Value {
    version.map(Value::String).unwrap_or(Value::Null)
}

fn state_version(state: &FileState) -> Option<String> {
    match state {
        FileState::Present(version) => Some(version.clone()),
        _ => None,
    }
}

/// 核对调用方给的版本前置条件；过不了就给一条诊断。
///
/// 三态各自的判法（跨仓评审 X02）：
///
/// | 磁盘上 | 说"应当是版本 v" | 说"应当什么都没有"（null） | 没给前置条件 |
/// | --- | --- | --- | --- |
/// | 在，版本一样 | 过 | 冲突：它在 | 过 |
/// | 在，版本不同 | 冲突：被写过 | 冲突：它在 | 过 |
/// | 确实不在 | 冲突：没了 | 过 | 过 |
/// | **问不出来** | **拒** | **拒** | **拒** |
///
/// 最后一行是这次收口的重点：读不出属性不等于文件不在。把它当成"不在"，
/// 一个"这路径应当是空的"前置条件就会在文件其实还在的时候放行。没给前置
/// 条件也照样拒——状态都问不出来，后面的落盘复核同样没法做。
fn version_precondition(
    display: &str,
    operation: &'static str,
    state: &FileState,
    expected_versions: &Option<HashMap<String, Option<String>>>,
) -> Option<Diagnostic> {
    if let FileState::Unknown(reason) = state {
        return Some(Diagnostic::version_conflict(
            format!(
                "cannot tell the state of {display} ({reason}), so this patch's preconditions cannot be checked; nothing was written"
            ),
            display.to_string(),
            operation,
            None,
            None,
        ));
    }
    let expected = expected_versions.as_ref()?.get(display)?;
    let actual = state_version(state);
    (expected != &actual).then(|| {
        Diagnostic::version_conflict(
            version_conflict_message(display, expected, &actual),
            display.to_string(),
            operation,
            expected.clone(),
            actual,
        )
    })
}

fn version_conflict_message(
    display: &str,
    expected: &Option<String>,
    actual: &Option<String>,
) -> String {
    match (expected, actual) {
        (Some(expected), Some(actual)) => format!(
            "{display} has changed since you read it (version {expected} is now {actual}); read it again before patching, or your edit would overwrite whatever changed"
        ),
        (Some(expected), None) => format!(
            "{display} is gone (you had version {expected}); it was deleted after you read it"
        ),
        (None, Some(actual)) => format!(
            "{display} already exists (version {actual}), but the precondition you sent says this path should be empty; read it and decide whether to update it instead"
        ),
        (None, None) => format!("{display}: version precondition failed"),
    }
}

fn operation_label(file: &FilePatch) -> &'static str {
    if file.is_deleted {
        "delete"
    } else if file.is_new_file {
        "add"
    } else {
        "update"
    }
}

/// `expected_versions`：路径 → 那时的版本号，`null` 表示"那时这里应当什么
/// 都没有"。形状和 `patch_check` 回的 `observed_versions` 一模一样，原样粘
/// 回来就行。
fn expected_versions(
    args: &Value,
) -> Result<Option<HashMap<String, Option<String>>>, WorkspaceError> {
    let Some(raw) = args.get("expected_versions") else {
        return Ok(None);
    };
    if raw.is_null() {
        return Ok(None);
    }
    let object = raw.as_object().ok_or_else(|| {
        WorkspaceError::invalid_argument(
            "expected_versions must be an object mapping paths to the version read_file or patch_check returned (null means the file should not exist)",
        )
    })?;
    let mut map = HashMap::with_capacity(object.len());
    for (path, value) in object {
        let version = match value {
            Value::Null => None,
            Value::String(version) => Some(version.clone()),
            _ => {
                return Err(WorkspaceError::invalid_argument(format!(
                    "expected_versions[{path}] must be a version string or null"
                )))
            }
        };
        map.insert(path.clone(), version);
    }
    Ok(Some(map))
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

    /// 上下文对不上的时候，得说清楚是哪个文件、第几段、文件现在长什么样、
    /// 该重读哪一段。原来只有一句 `Hunk context did not match file content.`
    /// 和一个空的 details，模型除了重读整个项目没有别的办法（审查 E03、P01）。
    #[test]
    fn a_hunk_that_does_not_match_says_where_to_look() {
        let workspace = tempdir().expect("workspace");
        let harness = tempdir().expect("harness");
        let body = (1..=60)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(workspace.path().join("m.txt"), format!("{body}\n")).expect("file");
        let context =
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("context");

        // 补丁以为第 40、41 行是 `line 40` / `line forty-one`，而文件里第 41 行
        // 是 `line 41`——整段对不上，只有第一行还在。
        let error = apply_patch(
            &context,
            &json!({
                "patch": "--- a/m.txt\n+++ b/m.txt\n@@ -40,2 +40,2 @@\n-line 40\n-line forty-one\n+line forty\n+line forty-one\n"
            }),
        )
        .expect_err("第二行对不上");
        let value = error.to_error_value();
        assert_eq!(value["code"], "PATCH_FAILED", "{value}");
        let message = value["message"].as_str().unwrap_or_default();
        assert!(
            message.contains("m.txt") && message.contains("hunk #1"),
            "消息得点名文件和第几段：{message}"
        );

        let details = &value["details"];
        assert_eq!(details["files_changed"], json!(false));
        assert_eq!(details["problem_count"], json!(1));
        let diagnostic = &details["diagnostics"][0];
        assert_eq!(diagnostic["file"], "m.txt");
        assert_eq!(diagnostic["operation"], "update");
        assert_eq!(diagnostic["baseline"], "file_on_disk");
        assert_eq!(diagnostic["hunk_index"], json!(0));
        // 第一行还在原处：这是"重读那一段"，不是"整个补丁都不对"。
        assert_eq!(diagnostic["reason_code"], "context_drifted", "{diagnostic}");
        assert_eq!(diagnostic["expected_range"]["start_line"], json!(40));
        assert_eq!(
            diagnostic["candidate_ranges"][0]["start_line"],
            json!(40),
            "{diagnostic}"
        );
        // 建议重读的范围里必须真的包含那一行，照着读就能重建这段补丁。
        let start = diagnostic["suggested_read_range"]["start_line"]
            .as_u64()
            .expect("start");
        let end = diagnostic["suggested_read_range"]["end_line"]
            .as_u64()
            .expect("end");
        assert!(start <= 40 && end >= 40, "{diagnostic}");
        let excerpt = &diagnostic["actual_excerpt"];
        assert!(
            excerpt["lines"]
                .as_array()
                .expect("lines")
                .iter()
                .any(|line| line == "line 40"),
            "摘录要给出文件现在的样子：{excerpt}"
        );
    }

    /// 两段的顺序写反了：上下文确实在文件里，只是在前一段之前。这跟"文件里
    /// 根本没有这段"是两码事——前者把两段调个个儿就能过。
    #[test]
    fn hunks_written_out_of_file_order_say_so() {
        let workspace = tempdir().expect("workspace");
        let harness = tempdir().expect("harness");
        std::fs::write(workspace.path().join("m.txt"), "alpha\nbeta\ngamma\n").expect("file");
        let context =
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("context");

        let error = apply_patch(
            &context,
            &json!({
                "patch": "--- a/m.txt\n+++ b/m.txt\n@@\n-gamma\n+GAMMA\n@@\n-alpha\n+ALPHA\n"
            }),
        )
        .expect_err("第二段在第一段前面");
        let diagnostic = &error.to_error_value()["details"]["diagnostics"][0];
        assert_eq!(
            diagnostic["reason_code"], "context_out_of_order",
            "{diagnostic}"
        );
        assert_eq!(diagnostic["hunk_index"], json!(1));
        assert_eq!(diagnostic["candidate_ranges"][0]["start_line"], json!(1));
        assert_eq!(
            std::fs::read_to_string(workspace.path().join("m.txt")).unwrap(),
            "alpha\nbeta\ngamma\n"
        );
    }

    /// 多个文件各自出问题：一次都报出来，而不是修一个报一个；并且一个字节
    /// 都不能落盘（审查 A08、C2）。
    #[test]
    fn every_file_that_fails_gets_its_own_diagnostic_and_nothing_is_written() {
        let workspace = tempdir().expect("workspace");
        let harness = tempdir().expect("harness");
        std::fs::write(workspace.path().join("a.txt"), "a-old\n").expect("a");
        std::fs::write(workspace.path().join("b.txt"), "b-old\n").expect("b");
        let context =
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("context");

        let error = apply_patch(
            &context,
            &json!({
                "patch": concat!(
                    "--- a/a.txt\n+++ b/a.txt\n@@\n-a-nope\n+a-new\n",
                    "--- a/b.txt\n+++ b/b.txt\n@@\n-b-nope\n+b-new\n",
                    "--- a/missing.txt\n+++ b/missing.txt\n@@\n-x\n+y\n"
                )
            }),
        )
        .expect_err("三个文件都有问题");
        let value = error.to_error_value();
        let details = &value["details"];
        assert_eq!(details["problem_count"], json!(3), "{details}");
        let files = details["diagnostics"]
            .as_array()
            .expect("diagnostics")
            .iter()
            .map(|item| item["file"].as_str().unwrap_or_default().to_string())
            .collect::<Vec<_>>();
        assert_eq!(files, vec!["a.txt", "b.txt", "missing.txt"]);
        assert_eq!(
            details["diagnostics"][2]["reason_code"], "file_not_found",
            "{details}"
        );
        assert_eq!(details["diagnostics_truncated"], json!(false));
        assert_eq!(
            std::fs::read_to_string(workspace.path().join("a.txt")).unwrap(),
            "a-old\n"
        );
        assert_eq!(
            std::fs::read_to_string(workspace.path().join("b.txt")).unwrap(),
            "b-old\n"
        );
    }

    /// 同一个文件里第一段就失败了，后面几段**不检查**——它们要看见前一段的
    /// 结果才知道对不对。不检查就得说没检查，不能让人以为剩下的都没问题。
    #[test]
    fn hunks_after_a_failure_in_the_same_file_are_reported_as_unchecked() {
        let workspace = tempdir().expect("workspace");
        let harness = tempdir().expect("harness");
        std::fs::write(workspace.path().join("m.txt"), "one\ntwo\n").expect("file");
        let context =
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("context");

        let error = apply_patch(
            &context,
            &json!({
                "patch": concat!(
                    "--- a/m.txt\n+++ b/m.txt\n@@\n-nope\n+changed\n",
                    "--- a/m.txt\n+++ b/m.txt\n@@\n-two\n+2\n"
                )
            }),
        )
        .expect_err("第一段就对不上");
        let details = &error.to_error_value()["details"];
        assert_eq!(details["problem_count"], json!(1), "{details}");
        assert_eq!(details["not_checked"][0]["file"], "m.txt");
        assert_eq!(
            details["not_checked"][0]["reason_code"],
            "earlier_failure_in_same_file"
        );
    }

    /// 算完补丁、还没写下去，这中间别人改了同一个文件。
    ///
    /// 这是最难测、也最要紧的那个窗口：照原样写下去，别人的改动就无声没了。
    /// 靠真实并发碰运气测不稳，所以用钩子把这一手插进确定的位置。
    #[test]
    fn a_file_changed_between_planning_and_writing_is_not_overwritten() {
        let (_workspace, _harness, context) = context_with_file();
        let main = context.workspace.root().join("main.rs");
        let target = main.clone();
        let _hook = faults::before_commit_do(move || {
            std::fs::write(&target, "someone else wrote this\n").expect("外部写入");
        });

        let error = apply_patch(&context, &patch()).expect_err("文件变了，应当拒绝");
        let value = error.to_error_value();
        assert_eq!(value["code"], "FILE_VERSION_CONFLICT", "{value}");
        assert_eq!(
            std::fs::read_to_string(&main).unwrap(),
            "someone else wrote this\n",
            "别人的改动被盖掉了"
        );
    }

    /// 新建的目标在这中间冒出来：也不覆盖。模型以为自己在建一个新文件，
    /// 实际会盖掉别人刚放进来的东西（审查 A09）。
    #[test]
    fn a_new_file_that_appeared_in_the_meantime_is_not_clobbered() {
        let (_workspace, _harness, context) = context_with_file();
        let fresh = context.workspace.root().join("fresh.txt");
        let target = fresh.clone();
        let _hook = faults::before_commit_do(move || {
            std::fs::write(&target, "someone else got here first\n").expect("外部写入");
        });

        let error = apply_patch(
            &context,
            &json!({
                "patch": "*** Begin Patch\n*** Add File: fresh.txt\n+mine\n*** End Patch\n"
            }),
        )
        .expect_err("目标已经有人了，应当拒绝");
        assert_eq!(
            error.to_error_value()["code"],
            "FILE_VERSION_CONFLICT",
            "{}",
            error.to_error_value()
        );
        assert_eq!(
            std::fs::read_to_string(&fresh).unwrap(),
            "someone else got here first\n"
        );
    }

    /// 删除也核对：模型是看着旧内容决定删它的，内容在这之后变了就不能照删。
    #[test]
    fn a_delete_of_a_file_that_changed_in_the_meantime_is_refused() {
        let (_workspace, _harness, context) = context_with_file();
        let main = context.workspace.root().join("main.rs");
        let target = main.clone();
        let _hook = faults::before_commit_do(move || {
            // 版本号是大小加修改时间，所以内容和长度都换一下。
            std::fs::write(&target, "changed and longer\n").expect("外部写入");
        });

        let error = apply_patch(
            &context,
            &json!({
                "patch": "*** Begin Patch\n*** Delete File: main.rs\n*** End Patch\n",
                "confirm": true
            }),
        )
        .expect_err("文件变了，不能照删");
        assert_eq!(
            error.to_error_value()["code"],
            "FILE_VERSION_CONFLICT",
            "{}",
            error.to_error_value()
        );
        assert!(main.exists(), "文件被删了");
    }

    /// 前一段加了几行之后，后一段报的行号必须还是**原文件**的行号——模型拿
    /// 这个行号去 read_file，读的是磁盘上那份，不是改到一半的中间结果。
    #[test]
    fn line_numbers_in_a_diagnostic_are_the_ones_on_disk() {
        let workspace = tempdir().expect("workspace");
        let harness = tempdir().expect("harness");
        let body = (1..=30)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(workspace.path().join("m.txt"), format!("{body}\n")).expect("file");
        let context =
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("context");

        // 第一段在第 2 行插进去 3 行；第二段以为第 25、26 行是
        // `line 25` / `line twenty-six`，而文件里第 26 行是 `line 26`。
        let error = apply_patch(
            &context,
            &json!({
                "patch": concat!(
                    "--- a/m.txt\n+++ b/m.txt\n@@ -2,0 +2,3 @@\n+extra a\n+extra b\n+extra c\n",
                    "@@ -25,2 +28,2 @@\n-line 25\n-line twenty-six\n+line 25!\n+line 26!\n"
                )
            }),
        )
        .expect_err("第二段对不上");
        let diagnostic = &error.to_error_value()["details"]["diagnostics"][0];
        assert_eq!(diagnostic["hunk_index"], json!(1), "{diagnostic}");
        // `line 25` 在磁盘上就是第 25 行；插入的 3 行不能算进去。
        assert_eq!(
            diagnostic["candidate_ranges"][0]["start_line"],
            json!(25),
            "{diagnostic}"
        );
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
    ///
    /// 注入的失败是"暂存文件不见了"，所以 `toexec_fs::replace` 真的被调了一
    /// 次并且真的失败——这条同时钉住共享库那条不可退让的约束（X01：替换失败
    /// 旧目标原样还在），下面对 `b.txt` 仍是 `b-old` 的断言才有意义。
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
        let miss = apply_hunks("x\n", &files[0].hunks).expect_err("no such block");
        assert_eq!(miss.code, "PATCH_FAILED");
        assert_eq!(miss.reason_code, "anchor_not_found");
    }
}
