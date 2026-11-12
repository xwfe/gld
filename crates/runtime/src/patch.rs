//! Apply text and notebook edits with workspace path checks and transactional rollback.
//!
//! The caller owns workspace-wide coordination. Request decoding can run before
//! acquiring the commit lock; file reads, validation, commit, and result sampling
//! remain inside the caller's lock for a real write.

use std::collections::{HashMap, HashSet};
use std::fs;

use gld_policy::write_class::WriteClass;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::notebook::NotebookEdits;
use crate::patch_diag::Diagnostic;
use crate::workspace::{tool_ok, Workspace, WorkspaceError};

mod commit;
mod parse;
mod report;
mod validation;

use commit::{commit_staged, Baseline};
use parse::{apply_hunks, parse_unified_diff};
use report::affected_paths;
use validation::{
    expected_versions, gate_write, operation_label, patch_diagnostics, patch_failed, state_version,
    version_precondition, version_value,
};

pub use report::{failure_log_fields, operation_summary, patch_format};

/// Validated argument shape, before workspace reads or commit-lock acquisition.
pub struct PatchRequest<'a> {
    patch: &'a str,
    notebook_edits: Option<Vec<NotebookEdits>>,
    dry_run: bool,
    confirm: bool,
    expected_versions: Option<HashMap<String, Option<String>>>,
}

impl<'a> PatchRequest<'a> {
    pub fn from_args(args: &'a Value) -> Result<Self, WorkspaceError> {
        // notebook 按 cell 改走另一个参数：补丁是文本信封，塞不进"第几个 cell 换
        // 成什么"这种结构。两者在**同一次事务**里——多文件原子提交、备份回滚、
        // 版本前置条件、进程内写锁全都照样管着它（RFC-0003 G3.2）。
        let notebook_edits = crate::notebook::notebook_edits(args)?;
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
        Ok(Self {
            patch,
            notebook_edits,
            dry_run,
            confirm,
            expected_versions,
        })
    }

    pub fn dry_run(&self) -> bool {
        self.dry_run
    }
}

/// The tool result and, after a commit, the versions needed for caller bookkeeping.
pub enum PatchOutcome {
    Checked(Value),
    Applied {
        result: Value,
        previous_versions: serde_json::Map<String, Value>,
    },
}

/// Apply a decoded request while the caller holds its workspace commit lock.
///
/// `wrote_since` answers only whether the caller owns the entire version chain.
/// `on_commit` runs exactly once after a successful write and before sampling
/// result versions, so running-command write counters retain their ordering.
/// Neither callback runs a second executor or changes the workspace transaction.
pub fn apply(
    ws: &Workspace,
    request: PatchRequest<'_>,
    wrote_since: impl Fn(&str, &str, &str) -> bool,
    on_commit: impl FnOnce(),
) -> Result<PatchOutcome, WorkspaceError> {
    let PatchRequest {
        patch,
        notebook_edits,
        dry_run,
        confirm,
        expected_versions,
    } = request;
    let file_patches = if patch.is_empty() {
        Vec::new()
    } else {
        parse_unified_diff(patch)?
    };
    if file_patches.is_empty() && notebook_edits.is_none() {
        return Err(patch_failed("no_file_headers", "No files were modified."));
    }
    // 每个目标先过写权限分类器：`.git/` 一律不写，`.github/` 按改的是什么分开
    // （workflow 和 CODEOWNERS 要 confirm，模板之类照常改），关键文件仍然只在
    // 删除时要 confirm。规则和理由都在 `gld_policy::write_class`，这里不再判第二遍。
    for file in &file_patches {
        gate_write(&file.path, file.is_deleted, confirm)?;
    }
    // notebook 的 cell 编辑和文本补丁在同一次事务里，写的也是工作区里的文件，
    // 所以走同一道门——否则 `.github/workflows/x.ipynb` 这种路径能从旁边绕过去。
    if let Some(edits) = notebook_edits.as_ref() {
        for edit in edits {
            gate_write(&edit.path, false, confirm)?;
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
    // expected_versions 落后、但中间全是这个调用方自己经 gld 写的：放行，在 warnings 里说一声。
    let mut version_notes: Vec<String> = Vec::new();

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
            let state = crate::workspace::file_state(&resolved.path);
            if let Some(diagnostic) = version_precondition(
                &resolved.display,
                operation_label(fp),
                &state,
                &expected_versions,
                &|then, now| wrote_since(&resolved.display, then, now),
                &mut version_notes,
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
                    *miss,
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

        let state = crate::workspace::file_state(&resolved.path);
        if let Some(diagnostic) = version_precondition(
            &resolved.display,
            "update",
            &state,
            &expected_versions,
            &|then, now| wrote_since(&resolved.display, then, now),
            &mut version_notes,
        ) {
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
        let updated = match crate::notebook::edit(&original, &resolved.display, &edits.cells) {
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
            patch_failed(
                "notebook_not_utf8",
                format!("{} did not serialize as UTF-8", resolved.display),
            )
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
        // 落盘成功才记这一笔：正在跑的命令拿它算"我起来之后工作区被改过几次"，
        // 记成失败的那次会让一条本来可信的命令结果显得不可信。
        on_commit();
        let change_id = Uuid::new_v4().simple().to_string();
        // 落盘之后的新版本：接着改同一个文件时原样传回 expected_versions，
        // 不用再 read_file 一遍。
        let new_versions = order
            .iter()
            .map(|path| {
                let version = ws
                    .resolve_for_write(path)
                    .ok()
                    .and_then(|resolved| crate::workspace::current_file_version(&resolved.path));
                (path.clone(), version_value(version))
            })
            .collect::<serde_json::Map<_, _>>();
        return Ok(PatchOutcome::Applied {
            result: tool_ok(json!({
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
                "warnings": version_notes
                    .into_iter()
                    .chain(sensitive_write_warnings(&order))
                    .collect::<Vec<_>>()
            })),
            previous_versions: observed,
        });
    }

    Ok(PatchOutcome::Checked(tool_ok(json!({
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
        "warnings": version_notes
            .into_iter()
            .chain(sensitive_write_warnings(&order))
            .collect::<Vec<_>>()
    }))))
}

/// 这批补丁里有没有"改的不只是工作树"的文件。
///
/// `confirm=true` 已经是一次明确的批准，但批准之后没人再提起这件事——落盘结果
/// 和预检结果都只说"改了几个文件"。CI 工作流和 CODEOWNERS 的效果发生在
/// GitHub 上，不在这棵树里，值得在结果里点名（方案 C4：不能把 `.github`
/// 标成无风险配置）。
fn sensitive_write_warnings(order: &[String]) -> Vec<String> {
    order
        .iter()
        .filter_map(|path| {
            let class = WriteClass::of(path);
            class.is_sensitive().then(|| {
                format!(
                    "{path} ({}): this changes what GitHub does — CI execution or review requirements — not just the working tree.",
                    class.slug()
                )
            })
        })
        .collect()
}

#[cfg(test)]
mod tests;
