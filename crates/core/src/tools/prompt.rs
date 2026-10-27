//! 用户与项目 .agents/prompts 下的只读 Markdown 模板；不注入系统说明，也不替换变量。

use std::fs;
use std::path::Path;

use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::tools::caller::Caller;
use crate::tools::context::ToolContext;
use crate::tools::workspace::{tool_ok, WorkspaceError};

const MAX_PROMPTS_PER_ROOT: usize = 256;
const MAX_PROMPT_BYTES: u64 = 128 * 1024;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PromptDescriptor {
    id: String,
    name: String,
    description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    argument_hint: Option<String>,
    path: String,
    scope: &'static str,
    content_sha256: String,
}

struct PromptEntry {
    descriptor: PromptDescriptor,
    content: String,
    notes: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
struct SkippedPrompt {
    path: String,
    scope: &'static str,
    reason: String,
}

#[derive(Default)]
struct PromptScan {
    prompts: Vec<PromptEntry>,
    skipped: Vec<SkippedPrompt>,
}

pub fn prompt_manage(
    ctx: &ToolContext,
    caller: &Caller,
    args: &Value,
) -> Result<Value, WorkspaceError> {
    manage_at(ctx, caller, args, dirs::home_dir().as_deref())
}

/// 允许测试把用户主目录换成临时目录，不碰真实用户的 Prompt 文件。
fn manage_at(
    ctx: &ToolContext,
    caller: &Caller,
    args: &Value,
    home: Option<&Path>,
) -> Result<Value, WorkspaceError> {
    let value = |key: &str| {
        args.get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
    };
    let scope = value("scope");
    if !matches!(scope, None | Some("workspace" | "global")) {
        return Err(WorkspaceError::invalid_argument(
            "scope must be workspace or global",
        ));
    }
    if scope == Some("global") && !caller.may_read_user_prompts() {
        return Err(WorkspaceError::Tool {
            code: "PROMPT_SCOPE_FORBIDDEN",
            message: "User-level prompts require full authenticated access".into(),
            category: "permission",
            retryable: false,
        });
    }
    let scan = discover(ctx.workspace.root(), home, caller.may_read_user_prompts());
    match value("action") {
        Some("list") => {
            let query = value("query").map(str::to_lowercase);
            let matched = scan
                .prompts
                .iter()
                .filter(|entry| scope.is_none_or(|scope| entry.descriptor.scope == scope))
                .filter(|entry| {
                    query.as_ref().is_none_or(|query| {
                        format!("{} {}", entry.descriptor.name, entry.descriptor.description)
                            .to_lowercase()
                            .contains(query)
                    })
                })
                .map(|entry| &entry.descriptor)
                .collect::<Vec<_>>();
            Ok(tool_ok(json!({
                "prompts": matched,
                "count": matched.len(),
                "skipped": scan.skipped,
            })))
        }
        Some("read") => {
            let id = value("id");
            let name = value("name");
            if id.is_none() && name.is_none() {
                return Err(WorkspaceError::invalid_argument(
                    "action=read requires id or name from action=list",
                ));
            }
            let matched =
                scan.prompts
                    .iter()
                    .filter(|entry| scope.is_none_or(|scope| entry.descriptor.scope == scope))
                    .filter(|entry| match id {
                        Some(id) => entry.descriptor.id == id,
                        None => name
                            .is_some_and(|name| entry.descriptor.name.eq_ignore_ascii_case(name)),
                    })
                    .collect::<Vec<_>>();
            match matched.as_slice() {
                [] => Err(WorkspaceError::Tool {
                    code: "PROMPT_NOT_FOUND",
                    message: "Prompt not found in permitted .agents/prompts sources".into(),
                    category: "runtime",
                    retryable: false,
                }),
                [entry] => Ok(tool_ok(json!({
                    "prompt": entry.descriptor,
                    "content": entry.content,
                    "notes": entry.notes,
                    "variablesExpanded": false,
                }))),
                _ => Err(WorkspaceError::ToolDetails {
                    code: "AMBIGUOUS_PROMPT",
                    message: "Multiple prompts use this name; select id or scope".into(),
                    category: "validation",
                    retryable: false,
                    details: json!({
                        "matches": matched.iter().map(|entry| &entry.descriptor).collect::<Vec<_>>(),
                    }),
                }),
            }
        }
        _ => Err(WorkspaceError::invalid_argument(
            "action must be list or read",
        )),
    }
}

fn discover(workspace: &Path, home: Option<&Path>, include_global: bool) -> PromptScan {
    let mut scan = PromptScan::default();
    collect(
        &mut scan,
        &workspace.join(".agents/prompts"),
        workspace,
        ".agents/prompts",
        "workspace",
    );
    if include_global {
        if let Some(home) = home {
            collect(
                &mut scan,
                &home.join(".agents/prompts"),
                home,
                "~/.agents/prompts",
                "global",
            );
        }
    }
    scan
}

/// 只读列举固定根目录第一层的 .md 文件；无任意目录遍历参数。
fn collect(
    scan: &mut PromptScan,
    root: &Path,
    allowed_parent: &Path,
    label: &str,
    scope: &'static str,
) {
    let meta = match fs::symlink_metadata(root) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(error) => {
            scan.skipped.push(SkippedPrompt {
                path: label.into(),
                scope,
                reason: format!("cannot inspect directory: {error}"),
            });
            return;
        }
    };
    // 整个根目录的符号链接不等于主动授权了链接另一端的用户文件。
    if !meta.is_dir() || meta.file_type().is_symlink() {
        scan.skipped.push(SkippedPrompt {
            path: label.into(),
            scope,
            reason: "prompt root is not a normal directory".into(),
        });
        return;
    }
    let (Ok(real_root), Ok(allowed)) = (root.canonicalize(), allowed_parent.canonicalize()) else {
        return;
    };
    if !real_root.starts_with(&allowed) || real_root == allowed {
        scan.skipped.push(SkippedPrompt {
            path: label.into(),
            scope,
            reason: "prompt root resolves outside its permitted directory".into(),
        });
        return;
    }
    let Ok(entries) = fs::read_dir(root) else {
        scan.skipped.push(SkippedPrompt {
            path: label.into(),
            scope,
            reason: "cannot list directory".into(),
        });
        return;
    };
    let mut entries = entries.filter_map(Result::ok).collect::<Vec<_>>();
    entries.sort_by_key(fs::DirEntry::file_name);
    let mut seen_md = 0;
    for entry in entries {
        let file_name = entry.file_name();
        let Some(file_name) = file_name.to_str() else {
            continue;
        };
        if file_name.starts_with('.') || !file_name.ends_with(".md") || file_name == ".md" {
            continue;
        }
        seen_md += 1;
        if seen_md > MAX_PROMPTS_PER_ROOT {
            scan.skipped.push(SkippedPrompt {
                path: label.into(),
                scope,
                reason: format!(
                    "more than {MAX_PROMPTS_PER_ROOT} Markdown prompts; later files not scanned"
                ),
            });
            break;
        }
        let display = format!("{label}/{file_name}");
        let bad = |reason: String, scan: &mut PromptScan| {
            scan.skipped.push(SkippedPrompt {
                path: display.clone(),
                scope,
                reason,
            });
        };
        let Ok(file_type) = entry.file_type() else {
            bad("cannot inspect file type".into(), scan);
            continue;
        };
        if !file_type.is_file() {
            bad(
                "not a regular file (directories and symlinks are not read)".into(),
                scan,
            );
            continue;
        }
        let Ok(real_path) = entry.path().canonicalize() else {
            bad("cannot resolve file".into(), scan);
            continue;
        };
        if !real_path.starts_with(&real_root) {
            bad("file resolves outside prompt directory".into(), scan);
            continue;
        }
        let Ok(metadata) = fs::metadata(&real_path) else {
            bad("cannot read file metadata".into(), scan);
            continue;
        };
        if metadata.len() > MAX_PROMPT_BYTES {
            bad(
                format!("file is larger than {MAX_PROMPT_BYTES} bytes"),
                scan,
            );
            continue;
        }
        let content = match fs::read_to_string(&real_path) {
            Ok(content) => content,
            Err(_) => {
                bad("file is not readable UTF-8 Markdown".into(), scan);
                continue;
            }
        };
        // 文件读取时再限制一次大小，覆盖 metadata 与 read 之间的普通变动。
        if content.len() as u64 > MAX_PROMPT_BYTES {
            bad(
                format!("file is larger than {MAX_PROMPT_BYTES} bytes"),
                scan,
            );
            continue;
        }
        if content.trim().is_empty() {
            bad("prompt is empty".into(), scan);
            continue;
        }
        let (header, _) = toexec_skill::frontmatter::split(&content);
        let hint_from_source = header.and_then(|header| {
            header.lines().find_map(|line| {
                line.strip_prefix("argument-hint:")
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string)
            })
        });
        let (frontmatter, notes) = match header {
            Some(header) => match toexec_skill::frontmatter::parse(header) {
                Ok(value) => (Some(value), Vec::new()),
                Err(_) => (None, vec!["invalid frontmatter; source returned unchanged"]),
            },
            None => (None, Vec::new()),
        };
        let description = frontmatter
            .as_ref()
            .and_then(|header| header.text("description"))
            .unwrap_or_default()
            .to_string();
        let argument_hint = frontmatter
            .as_ref()
            .and_then(|header| header.text("argument-hint"))
            .map(str::to_string)
            // Codex 示例常写 argument-hint: [FILES=<paths>]，YAML 把未引号的
            // 方括号解析为 sequence，text() 取不到；参数提示本来就是原文。
            .or(hint_from_source);
        let id = format!(
            "{:x}",
            Sha256::digest(real_path.to_string_lossy().as_bytes())
        );
        let content_sha256 = format!("{:x}", Sha256::digest(content.as_bytes()));
        scan.prompts.push(PromptEntry {
            descriptor: PromptDescriptor {
                id: id[..16].to_string(),
                name: file_name.trim_end_matches(".md").to_string(),
                description,
                argument_hint,
                path: display,
                scope,
                content_sha256,
            },
            content,
            notes,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{AuthContext, Grant, Principal};

    fn ctx(path: &Path, harness: &Path) -> ToolContext {
        ToolContext::for_test(path.to_path_buf(), harness.to_path_buf()).expect("context")
    }

    fn write(path: &Path, content: &str) {
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        fs::write(path, content).expect("write");
    }

    fn call(ctx: &ToolContext, caller: &Caller, args: Value, home: &Path) -> Value {
        manage_at(ctx, caller, &args, Some(home)).expect("prompt tool")
    }

    #[test]
    fn lists_and_reads_live_prompts_without_interpolating_arguments() {
        let workspace = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let harness = tempfile::tempdir().unwrap();
        let ctx = ctx(workspace.path(), harness.path());
        let caller = Caller::local();
        assert_eq!(
            call(&ctx, &caller, json!({"action":"list"}), home.path())["count"],
            0
        );

        let template = home.path().join(".agents/prompts/debug.md");
        write(
            &template,
            "---\ndescription: Diagnose a failure\nargument-hint: [SYMPTOM=<text>]\n---\nInvestigate $ARGUMENTS.\n",
        );
        let first = call(&ctx, &caller, json!({"action":"list"}), home.path());
        assert_eq!(first["count"], 1);
        assert_eq!(first["prompts"][0]["scope"], "global");
        assert_eq!(first["prompts"][0]["description"], "Diagnose a failure");
        assert_eq!(first["prompts"][0]["argumentHint"], "[SYMPTOM=<text>]");
        let id = first["prompts"][0]["id"].as_str().unwrap();
        let loaded = call(&ctx, &caller, json!({"action":"read","id":id}), home.path());
        assert!(loaded["content"].as_str().unwrap().contains("$ARGUMENTS"));
        assert_eq!(loaded["variablesExpanded"], false);

        write(&template, "Updated $ARGUMENTS");
        let updated = call(
            &ctx,
            &caller,
            json!({"action":"list","query":"DEBUG"}),
            home.path(),
        );
        assert_eq!(updated["count"], 1);
        assert_eq!(updated["prompts"][0]["id"], first["prompts"][0]["id"]);
        assert_ne!(
            updated["prompts"][0]["contentSha256"],
            first["prompts"][0]["contentSha256"]
        );
        assert_eq!(
            call(
                &ctx,
                &caller,
                json!({"action":"read","name":"debug"}),
                home.path()
            )["content"],
            "Updated $ARGUMENTS"
        );
    }

    #[test]
    fn duplicate_names_require_id_or_scope_and_workspace_takes_no_hidden_priority() {
        let workspace = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let harness = tempfile::tempdir().unwrap();
        let ctx = ctx(workspace.path(), harness.path());
        write(
            &workspace.path().join(".agents/prompts/review.md"),
            "workspace review",
        );
        write(
            &home.path().join(".agents/prompts/review.md"),
            "global review",
        );
        let listed = call(
            &ctx,
            &Caller::local(),
            json!({"action":"list"}),
            home.path(),
        );
        assert_eq!(listed["count"], 2);
        assert_eq!(listed["prompts"][0]["scope"], "workspace");
        let err = manage_at(
            &ctx,
            &Caller::local(),
            &json!({"action":"read","name":"review"}),
            Some(home.path()),
        )
        .expect_err("ambiguous");
        assert_eq!(err.code(), "AMBIGUOUS_PROMPT");
        let loaded = call(
            &ctx,
            &Caller::local(),
            json!({"action":"read","name":"review","scope":"workspace"}),
            home.path(),
        );
        assert_eq!(loaded["content"], "workspace review");
    }

    #[test]
    fn anon_and_grant_do_not_see_global_prompts_even_by_id() {
        let workspace = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let harness = tempfile::tempdir().unwrap();
        let ctx = ctx(workspace.path(), harness.path());
        write(
            &workspace.path().join(".agents/prompts/local.md"),
            "project",
        );
        write(
            &home.path().join(".agents/prompts/secret.md"),
            "user-level prompt",
        );
        let full = Caller::from_auth(&AuthContext::new(Principal::SharedSecret, "hub"));
        let listed = call(&ctx, &full, json!({"action":"list"}), home.path());
        assert_eq!(listed["count"], 2);
        let global_id = listed["prompts"][1]["id"].as_str().unwrap();
        let grant = Grant {
            id: "restricted".into(),
            name: "restricted".into(),
            workspaces: vec![],
            read_only: true,
            browser: false,
        };
        let restricted = Caller::from_auth(
            &AuthContext::new(Principal::SharedSecret, "hub").with_grant(Some(grant)),
        );
        for caller in [
            &Caller::from_auth(&AuthContext::anonymous("hub")),
            &restricted,
        ] {
            let scoped = call(&ctx, caller, json!({"action":"list"}), home.path());
            assert_eq!(scoped["count"], 1);
            assert_eq!(scoped["prompts"][0]["scope"], "workspace");
            let err = manage_at(
                &ctx,
                caller,
                &json!({"action":"read","id":global_id}),
                Some(home.path()),
            )
            .expect_err("global id must not resolve");
            assert_eq!(err.code(), "PROMPT_NOT_FOUND");
            let err = manage_at(
                &ctx,
                caller,
                &json!({"action":"list","scope":"global"}),
                Some(home.path()),
            )
            .expect_err("global must not be listed");
            assert_eq!(err.code(), "PROMPT_SCOPE_FORBIDDEN");
        }
    }

    #[test]
    fn ignores_hidden_and_non_markdown_and_reports_bad_files() {
        let workspace = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let harness = tempfile::tempdir().unwrap();
        let ctx = ctx(workspace.path(), harness.path());
        let root = workspace.path().join(".agents/prompts");
        write(&root.join(".token.md"), "not for models");
        write(&root.join("readme.txt"), "not a prompt");
        write(&root.join("okay.md"), "plain Markdown without frontmatter");
        write(
            &root.join("huge.md"),
            &"x".repeat(MAX_PROMPT_BYTES as usize + 1),
        );
        fs::write(root.join("bytes.md"), [0xff, 0xfe]).unwrap();
        fs::create_dir(root.join("directory.md")).unwrap();
        let listed = call(
            &ctx,
            &Caller::local(),
            json!({"action":"list"}),
            home.path(),
        );
        assert_eq!(listed["count"], 1);
        assert_eq!(listed["prompts"][0]["name"], "okay");
        assert_eq!(listed["skipped"].as_array().unwrap().len(), 3);
        let error = manage_at(
            &ctx,
            &Caller::local(),
            &json!({"action":"read","name":"huge"}),
            Some(home.path()),
        )
        .expect_err("large prompt not available");
        assert_eq!(error.code(), "PROMPT_NOT_FOUND");
    }

    #[cfg(unix)]
    #[test]
    fn refuses_symlinks_and_roots_that_escape_workspace() {
        use std::os::unix::fs::symlink;
        let workspace = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let harness = tempfile::tempdir().unwrap();
        let ctx = ctx(workspace.path(), harness.path());
        let outside = tempfile::tempdir().unwrap();
        write(&outside.path().join("secret.md"), "outside");
        let root = workspace.path().join(".agents/prompts");
        fs::create_dir_all(&root).unwrap();
        symlink(outside.path().join("secret.md"), root.join("linked.md")).unwrap();
        let listed = call(
            &ctx,
            &Caller::local(),
            json!({"action":"list"}),
            home.path(),
        );
        assert_eq!(listed["count"], 0);
        assert_eq!(listed["skipped"].as_array().unwrap().len(), 1);

        fs::remove_dir_all(workspace.path().join(".agents")).unwrap();
        symlink(outside.path(), workspace.path().join(".agents")).unwrap();
        write(&outside.path().join("prompts/escape.md"), "escaped");
        let listed = call(
            &ctx,
            &Caller::local(),
            json!({"action":"list"}),
            home.path(),
        );
        assert_eq!(listed["count"], 0);
        assert!(listed["skipped"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| { entry["reason"].as_str().unwrap().contains("outside") }));
    }
}
