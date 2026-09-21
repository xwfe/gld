mod common;

use std::fs;
use std::process::Command;

use common::*;
use gld_core::tools::list_tools_for_profile;
use serde_json::{json, Value};

#[cfg(windows)]
const TEST_PYTHON: &str = "python";
#[cfg(not(windows))]
const TEST_PYTHON: &str = "python3";

#[test]
fn server_info_returns_workspace_and_tools() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let out = invoke(&ctx, "server_info", json!({}));
    let payload = assert_ok(&out);
    // 命令行直连没有工作区名，回落到项目名。
    assert_eq!(payload["server"], "gld");
    assert_eq!(payload["version"], env!("CARGO_PKG_VERSION"));
    assert!(payload["tools"].is_array());
    assert!(payload["tool_count"].as_u64().unwrap_or(0) > 0);
}

/// 服务名跟着工作区走，别让每个项目都自报同一个名字。
///
/// AI 调 `server_info` 是为了知道"我现在在哪个项目里"。这里以前写死
/// `coding-tools-mcp`（上游项目的名字），同时接三个工作区时，
/// 无论问哪一个都回同一个答案。
#[test]
fn server_info_names_the_workspace() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root).with_workspace_name("api");
    let out = invoke(&ctx, "server_info", json!({}));
    let payload = assert_ok(&out);
    assert_eq!(payload["server"], "api");
    assert_eq!(payload["title"], "api · gld");
}

#[test]
fn read_file_happy_path() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let out = invoke(&ctx, "read_file", json!({"path": "src/math.js"}));
    let payload = assert_ok(&out);
    assert_eq!(payload["path"], "src/math.js");
    assert_eq!(payload["encoding"], "utf-8");
}

/// 照着 `next_start_line` 一页页往下翻，每一行都得完整地出现在某一页里。
///
/// 截断落在一行中间时，以前 `next_start_line` 给的是下一行，这一行的后半截就再也
/// 读不到了——AI 以为自己读完了整个文件，其实中间缺了好几段。
#[test]
fn paging_with_next_start_line_never_skips_part_of_a_line() {
    let dir = tempfile::tempdir().expect("workspace");
    let lines: Vec<String> = (1..=40)
        .map(|n| format!("{n}:{}\n", "x".repeat(n % 7)))
        .collect();
    fs::write(dir.path().join("paged.txt"), lines.concat()).expect("write");
    let ctx = ctx_for(dir.path());

    let mut seen = vec![false; lines.len()];
    let mut start = 1u64;
    for _ in 0..200 {
        let out = invoke(
            &ctx,
            "read_file",
            json!({"path": "paged.txt", "start_line": start, "max_bytes": 12}),
        );
        let page = assert_ok(&out);
        let content = page["content"].as_str().expect("content");
        for (offset, piece) in content.split_inclusive('\n').enumerate() {
            let index = start as usize - 1 + offset;
            if piece == lines[index] {
                seen[index] = true;
            }
        }
        match page["next_start_line"].as_u64() {
            Some(next) => {
                assert!(next > start, "翻页原地打转：{page:#}");
                start = next;
            }
            None => break,
        }
    }
    let missing: Vec<usize> = seen
        .iter()
        .enumerate()
        .filter(|(_, seen)| !**seen)
        .map(|(index, _)| index + 1)
        .collect();
    assert!(missing.is_empty(), "这些行从没完整读到过：{missing:?}");
}

/// 一行本身就比 max_bytes 长时只能跳过它的剩余部分，但得明说，不能让 AI 以为读全了。
#[test]
fn a_line_longer_than_max_bytes_is_skipped_out_loud() {
    let dir = tempfile::tempdir().expect("workspace");
    let long_line = "y".repeat(50);
    fs::write(
        dir.path().join("long.txt"),
        format!("short\n{long_line}\nend\n"),
    )
    .expect("write");
    let ctx = ctx_for(dir.path());

    let out = invoke(
        &ctx,
        "read_file",
        json!({"path": "long.txt", "start_line": 2, "max_bytes": 12}),
    );
    let page = assert_ok(&out);
    assert_eq!(page["content"], "y".repeat(12));
    assert_eq!(page["next_start_line"], 3);
    assert!(
        page["warnings"]
            .to_string()
            .contains("line 2 is longer than max_bytes"),
        "{page:#}"
    );
}

#[test]
fn unknown_tool_is_validation_error() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let out = invoke(&ctx, "definitely_not_a_tool", json!({}));
    let err = assert_err(&out);
    assert_eq!(err["error"]["code"], "INVALID_ARGUMENT");
    assert_eq!(err["error"]["category"], "validation");
}

/// 关掉 confine-reads 之后，`..` 路径能读到外面，但只能读。
#[test]
fn read_file_explicit_parent_path_is_read_only_when_confinement_is_off() {
    let fx = malicious_fixture();
    let ctx = common::ctx_for_unconfined_reads(&fx.root);
    let out = invoke(&ctx, "read_file", json!({"path": "../outside-secret.txt"}));
    let result = assert_ok(&out);
    assert!(result["content"]
        .as_str()
        .unwrap_or("")
        .contains("TOP_SECRET"));
}

/// 默认（confine-reads=true）下，同一条路径要被拒，并且告诉人怎么放开。
#[test]
fn read_file_rejects_parent_paths_by_default() {
    let fx = malicious_fixture();
    let out = invoke(
        &ctx_for(&fx.root),
        "read_file",
        json!({"path": "../outside-secret.txt"}),
    );
    let err = assert_err(&out);
    assert_eq!(err["error"]["code"], "READS_CONFINED_TO_WORKSPACE");
    assert!(err["error"]["message"]
        .as_str()
        .unwrap_or("")
        .contains("confine-reads=false"));
}

#[test]
fn request_permissions_is_unsupported_not_silent_grant() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let out = invoke(
        &ctx,
        "request_permissions",
        json!({
            "tool_name": "exec_command",
            "permission": "network",
            "reason": "verify compliance denial shape",
            "arguments": {"cmd": "curl https://example.com"}
        }),
    );
    assert_err(&out);
    assert_eq!(out["error"]["code"], "ELICITATION_UNSUPPORTED");
    assert_eq!(out["status"], "unsupported");
    assert_eq!(out["error"]["retryable"], false);
    assert!(out["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .contains("Do not retry request_permissions"));
}

#[test]
fn request_permissions_is_hidden_from_clients_but_keeps_legacy_dangerous_mode_compatibility() {
    for profile in ["core", "read-only", "advanced", "compat-readonly-all"] {
        let tools = list_tools_for_profile(profile);
        assert!(
            tools
                .iter()
                .all(|tool| tool["name"] != "request_permissions"),
            "request_permissions must not be advertised for profile {profile}"
        );
    }

    let fx = tiny_js_fixture();
    let mut ctx = ctx_for(&fx.root);
    ctx.permission_mode = "dangerous".into();
    ctx.policy.permission_mode = "dangerous".into();
    let args = json!({
        "tool_name": "exec_command",
        "permission": "network",
        "reason": "verify dangerous-mode compatibility",
        "arguments": {"cmd": "curl https://example.com"}
    });
    let out = invoke(&ctx, "request_permissions", args.clone());
    let payload = assert_ok(&out);
    assert_eq!(payload["status"], "granted");
    assert_eq!(payload["constraints"]["mode"], "dangerous");
    assert_eq!(payload["constraints"]["requested"], args);
}

#[test]
fn check_exec_environment_reports_policy_metadata() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let out = invoke(&ctx, "check_exec_environment", json!({}));
    let payload = assert_ok(&out);
    assert_eq!(payload["permission_mode"], "trusted");
    assert!(payload["allowed_commands"].is_array());
}

#[test]
fn default_cwd_is_used_by_file_and_native_exec_tools() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    assert_ok(&invoke(&ctx, "set_default_cwd", json!({"path": "src"})));

    let file_result = invoke(&ctx, "read_file", json!({"path": "math.js"}));
    let file = assert_ok(&file_result);
    assert_eq!(file["path"], "src/math.js");

    let pwd_result = invoke(&ctx, "exec_command", json!({"cmd": "pwd"}));
    let pwd = assert_ok(&pwd_result);
    assert!(pwd["stdout"].as_str().unwrap_or("").contains("src"));
}

#[test]
fn git_log_root_does_not_pass_empty_pathspec() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("repo");
    fs::create_dir_all(&workspace).expect("创建仓库目录");
    fs::write(workspace.join("README.md"), "初始内容\n").expect("写入文件");

    for args in [
        vec!["init", "-q"],
        vec!["config", "user.email", "test@example.com"],
        vec!["config", "user.name", "测试用户"],
        vec!["add", "README.md"],
        vec!["commit", "-q", "-m", "初始化"],
    ] {
        let output = Command::new("git")
            .current_dir(&workspace)
            .args(args)
            .output()
            .expect("执行 git");
        assert!(output.status.success(), "git 命令失败: {:?}", output);
    }

    let ctx = ctx_for(&workspace);
    let result = invoke(&ctx, "git_log", json!({"path": ".", "max_count": 3}));
    let payload = assert_ok(&result);
    assert_eq!(payload["is_repo"], true);
    assert_eq!(payload["commits"].as_array().unwrap().len(), 1);
    for commit in payload["commits"].as_array().unwrap() {
        for field in [
            "hash",
            "short_hash",
            "author_name",
            "author_email",
            "author_date",
            "subject",
        ] {
            assert_eq!(
                commit[field].as_str().unwrap(),
                commit[field].as_str().unwrap().trim()
            );
        }
    }
}

#[test]
fn advanced_profile_exposes_every_declared_tool() {
    let declared = gld_core::tools::registry::P0_TOOLS
        .iter()
        .map(|(name, ..)| *name)
        .filter(|name| *name != "request_permissions")
        .collect::<std::collections::HashSet<_>>();
    let tool_values = gld_core::tools::list_tools_for_profile("advanced");
    let exposed = tool_values
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect::<std::collections::HashSet<_>>();

    assert_eq!(declared, exposed);
    assert!(declared
        .iter()
        .all(|name| gld_core::tools::is_allowed_tool(name)));
}

#[test]
fn core_profile_keeps_the_default_capabilities_and_adds_history_tools() {
    let tools = gld_core::tools::list_tools_for_profile("core");
    let names = tools
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect::<std::collections::HashSet<_>>();
    let expected = gld_core::tools::registry::CORE_TOOLS
        .iter()
        .copied()
        .filter(|name| *name != "request_permissions")
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(names, expected);
    assert_eq!(names.len(), expected.len());
    assert!(!names.contains("request_permissions"));
    assert!(names.contains("grep_text"));
    assert!(names.contains("history_session_bootstrap"));
    assert!(names.contains("history_session_checkpoint"));
    assert!(names.contains("history_session_validate"));
    assert!(names.contains("history_session_search"));
    assert!(names.contains("history_session_read"));
    assert!(names.contains("planning_state"));
    assert!(names.contains("create_goal"));
    assert!(names.contains("update_goal"));
    assert!(names.contains("create_plan"));
    assert!(names.contains("update_plan"));
    assert!(!names.contains("set_planning_mode"));
    assert!(!names.contains("harness_status"));
    assert!(!names.contains("start_task"));
}

#[test]
fn exec_health_check_reports_worker_and_pipe_status() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let out = invoke(&ctx, "exec_health_check", json!({}));
    let payload = assert_ok(&out);
    assert_eq!(payload["worker"]["alive"], true);
    assert_eq!(payload["session_create"], true);
    assert_eq!(payload["command_run"], true);
    assert_eq!(payload["stdout_capture"], true);
    assert_eq!(payload["stderr_capture"], true);
}

#[test]
fn native_diagnostics_support_pwd_and_ls_without_a_shell() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);

    let pwd_result = invoke(&ctx, "exec_command", json!({"cmd": "pwd"}));
    let pwd = assert_ok(&pwd_result);
    assert_eq!(pwd["command"], "pwd");
    assert!(pwd["stdout"]
        .as_str()
        .unwrap_or("")
        .contains("tiny-js-project"));
    assert_eq!(pwd["execution_mode"], "native_builtin");
    assert_eq!(pwd["harness_mode"], "standalone");
    assert_eq!(pwd["task_required"], false);
    assert_eq!(pwd["command_runner"], "native_builtin");
    assert_eq!(pwd["status"], "exited");
    assert_eq!(pwd["exit_code"], 0);
    assert_eq!(pwd["transport_ok"], true);
    assert_eq!(pwd["command_ok"], true);
    assert_eq!(pwd["duration_ms"], 0);
    assert_eq!(pwd["elapsed_ms"], 0);
    assert!(pwd["stdout"].is_string());
    assert_eq!(pwd["stderr"], "");

    let ls_result = invoke(&ctx, "exec_command", json!({"cmd": "ls"}));
    let ls = assert_ok(&ls_result);
    assert!(ls["stdout"].as_str().unwrap_or("").contains("src"));
    assert_eq!(ls["exit_code"], 0);
}

#[test]
fn direct_exec_uses_the_same_result_contract() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let result = invoke(
        &ctx,
        "exec_command",
        json!({"cmd": format!("{TEST_PYTHON} --version"), "filesystem_scope": "workspace"}),
    );
    let payload = assert_ok(&result);

    assert_eq!(payload["command"], format!("{TEST_PYTHON} --version"));
    assert_eq!(payload["execution_mode"], "direct");
    assert_eq!(payload["harness_mode"], "standalone");
    assert_eq!(payload["task_required"], false);
    assert_eq!(payload["status"], "exited");
    assert_eq!(payload["exit_code"], 0);
    assert!(payload["stdout"].is_string());
    assert!(payload["stderr"].is_string());
    assert!(payload["duration_ms"].is_u64());
    assert_eq!(payload["duration_ms"], payload["elapsed_ms"]);
    assert_eq!(payload["transport_ok"], true);
    assert_eq!(payload["command_ok"], true);
}

#[test]
fn nonzero_command_exit_keeps_transport_ok_but_sets_command_ok_false() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let result = invoke(
        &ctx,
        "exec_command",
        json!({
            "cmd": format!("{TEST_PYTHON} -c \"import sys; sys.exit(1)\""),
            "filesystem_scope": "workspace"
        }),
    );
    let payload = assert_ok(&result);

    assert_eq!(payload["ok"], true);
    assert_eq!(payload["transport_ok"], true);
    assert_eq!(payload["command_ok"], false);
    assert_eq!(payload["status"], "exited");
    assert_eq!(payload["exit_code"], 1);
}

#[test]
fn retained_session_timeout_stops_the_process_after_deadline() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let result = invoke(
        &ctx,
        "exec_command",
        json!({
            "cmd": format!("{TEST_PYTHON} -c \"import time; time.sleep(2)\""),
            "filesystem_scope": "workspace",
            "timeout_ms": 100,
            "yield_time_ms": 0
        }),
    );
    let payload = assert_ok(&result);
    assert_eq!(payload["status"], "running");
    assert_eq!(payload["transport_ok"], true);
    assert_eq!(payload["command_ok"], Value::Null);
    // 没给 stdin = `stdin_mode: close`，起来就关掉它（U4 第二批）。以前这里
    // 是开着的，而那个"开着"永远不会有数据——读 stdin 的命令因此挂到超时。
    assert_eq!(payload["stdin_mode"], "close");
    assert_eq!(payload["stdin_open"], false);
    let session_id = payload["session_id"].as_str().expect("session id");

    std::thread::sleep(std::time::Duration::from_millis(250));
    let after = invoke(
        &ctx,
        "write_stdin",
        json!({"session_id": session_id, "chars": ""}),
    );
    assert_eq!(after["termination_reason"], "timeout");
    assert_eq!(after["status"], "exited");
    assert_eq!(after["transport_ok"], true);
    assert_eq!(after["command_ok"], false);
    assert_eq!(after["stdin_open"], false);
    #[cfg(unix)]
    assert_eq!(after["exit_code"], Value::Null);
}

#[test]
fn killed_session_reports_command_failure_even_when_transport_succeeds() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let result = invoke(
        &ctx,
        "exec_command",
        json!({
            "cmd": format!("{TEST_PYTHON} -c \"import time; time.sleep(2)\""),
            "filesystem_scope": "workspace",
            "timeout_ms": 10_000,
            "yield_time_ms": 0
        }),
    );
    let payload = assert_ok(&result);
    let session_id = payload["session_id"].as_str().expect("session id");

    let killed = invoke(
        &ctx,
        "kill_session",
        json!({"session_id": session_id, "wait_ms": 2_000}),
    );
    let killed = assert_ok(&killed);
    assert_eq!(killed["status"], "killed");
    assert_eq!(killed["killed"], true);
    assert_eq!(killed["transport_ok"], true);
    assert_eq!(killed["command_ok"], false);
    #[cfg(unix)]
    assert_eq!(killed["exit_code"], Value::Null);
}

#[test]
fn list_files_accepts_glob_alias() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let out = invoke(
        &ctx,
        "list_files",
        json!({"glob": "**/*.js", "max_results": 10}),
    );
    let payload = assert_ok(&out);
    let files = payload["files"].as_array().expect("files array");
    assert!(!files.is_empty());
    assert!(files
        .iter()
        .all(|f| f["path"].as_str().unwrap_or("").ends_with(".js")));
}

#[test]
fn search_text_filters_by_glob() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let hit = invoke(
        &ctx,
        "search_text",
        json!({"query": "function add", "glob": "**/*.js", "max_results": 10}),
    );
    let hit_payload = assert_ok(&hit);
    assert!(hit_payload["total_matches"].as_u64().unwrap_or(0) > 0);

    let miss = invoke(
        &ctx,
        "search_text",
        json!({"query": "function add", "glob": "**/*.py"}),
    );
    let miss_payload = assert_ok(&miss);
    assert_eq!(miss_payload["total_matches"].as_u64().unwrap_or(1), 0);
}

#[test]
fn search_text_skips_binary_and_oversized_files() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);

    // Fixture materializes assets/raw.bin with NUL bytes; must not match or thrash.
    let binary = invoke(
        &ctx,
        "search_text",
        json!({"query": "binary", "glob": "assets/**", "max_results": 10}),
    );
    let binary_payload = assert_ok(&binary);
    assert_eq!(binary_payload["total_matches"].as_u64().unwrap_or(1), 0);
    assert!(binary_payload["skipped_binary_files"].as_u64().unwrap_or(0) >= 1);

    // Oversized text file is skipped by max_file_bytes without being fully loaded.
    let large_path = fx.root.join("search/huge.txt");
    fs::write(&large_path, format!("needle {}\n", "x".repeat(4096))).expect("write huge");
    let oversized = invoke(
        &ctx,
        "search_text",
        json!({
            "query": "needle",
            "glob": "search/huge.txt",
            "max_file_bytes": 64,
            "max_results": 10
        }),
    );
    let oversized_payload = assert_ok(&oversized);
    assert_eq!(oversized_payload["total_matches"].as_u64().unwrap_or(1), 0);
    assert!(
        oversized_payload["skipped_large_files"]
            .as_u64()
            .unwrap_or(0)
            >= 1
    );
    assert!(oversized_payload["warnings"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|w| w.as_str().unwrap_or("").contains("max_file_bytes")));
}

#[test]
fn search_text_stops_after_max_results() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let out = invoke(
        &ctx,
        "search_text",
        json!({"query": "common-token", "glob": "search/**", "max_results": 2}),
    );
    let payload = assert_ok(&out);
    assert_eq!(
        payload["matches"].as_array().map(|a| a.len()).unwrap_or(0),
        2
    );
    assert!(payload["truncated"].as_bool().unwrap_or(false));
    assert!(payload["warnings"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|w| w.as_str().unwrap_or("").contains("result limit reached")));
}

#[test]
fn grep_reuses_search_text_schema_and_behavior() {
    let schema = gld_core::tools::registry::input_schema("grep");
    assert_eq!(
        schema,
        gld_core::tools::registry::input_schema("search_text")
    );

    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    assert_ok(&invoke(&ctx, "set_default_cwd", json!({"path": "src"})));
    let output = invoke(
        &ctx,
        "grep",
        json!({
            "query": "function\\s+add",
            "path": ".",
            "glob": "**/*.js",
            "regex": true,
            "case_sensitive": true,
            "max_results": 10
        }),
    );
    let payload = assert_ok(&output);
    let matches = payload["matches"].as_array().expect("matches array");
    assert!(!matches.is_empty());
    assert!(matches
        .iter()
        .all(|item| item["path"].as_str().unwrap_or("").starts_with("src/")));
}

/// `check_exec_environment` 报出去的能力必须和实际行为一致。
///
/// 这个工具是模型用来"问自己能干什么"的。它以前在 permission-mode=dangerous
/// 下报 `global_tmp_write: allowed`，而写入实际永远只允许在 Workspace 内
/// （resolve_for_write 一律 join 到工作区根目录，它根本拿不到 permission_mode）。
/// 模型照着这句话去写 /tmp，只会拿到 ABSOLUTE_PATH_DENIED，然后反复重试。
///
/// 报少了顶多是模型不去尝试；报多了是让它撞墙。所以这里把"报告"和"实际"
/// 绑在一条测试里：两边同时改才可能过。
#[test]
fn the_reported_capabilities_match_what_writes_actually_do() {
    let fx = tiny_js_fixture();

    let contexts = [
        ("trusted", ctx_for(&fx.root)),
        ("dangerous", common::ctx_for_dangerous_mode(&fx.root)),
    ];
    for (label, ctx) in &contexts {
        let environment = invoke(ctx, "check_exec_environment", json!({}));
        let reported = assert_ok(&environment);
        assert_eq!(
            reported["global_tmp_write"], "denied",
            "{label} 模式报了一个做不到的能力"
        );

        let attempted = invoke(
            ctx,
            "apply_patch",
            json!({"patch": "*** Begin Patch\n*** Add File: /tmp/gld-capability-probe.txt\n+x\n*** End Patch"}),
        );
        let error = assert_err(&attempted);
        assert_eq!(
            error["error"]["code"], "ABSOLUTE_PATH_DENIED",
            "{label} 模式下工作区外写入居然成功了，报告要跟着改"
        );
    }
}

/// 预检和真跑必须给同一个答案。
///
/// 这是 `check_command` 存在的前提：如果预检说能跑、真跑被拒（或者反过来），
/// 它就不是"先问一句"，而是多一个误导来源。所以判定不是各写一份，而是同一个
/// `validate_command_for_workspace` + 同一个 `resolve_program`；这条测试钉住
/// 这件事（审查 A01）。
///
/// 被拒的三条都不会启动子进程——策略在起进程之前就拦下了。
#[test]
fn check_command_and_exec_command_agree() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);

    // (命令, 预期决定, 预期错误码)
    let cases: &[(&str, &str, &str)] = &[
        // 服务端自己就能答，不起进程。
        ("pwd", "allow", ""),
        ("rg --version", "deny", "POLICY_REJECTED"),
        ("ls | wc -l", "deny", "POLICY_REJECTED"),
        (
            "rm -rf build",
            "needs_approval",
            "DANGEROUS_OPERATION_REQUIRES_CONFIRMATION",
        ),
    ];

    for (cmd, decision, code) in cases {
        let checked = invoke(&ctx, "check_command", json!({ "cmd": cmd }));
        assert_ok(&checked);
        assert_eq!(checked["decision"], *decision, "{cmd}: {checked}");
        assert_eq!(checked["side_effects"], "none");

        let executed = invoke(&ctx, "exec_command", json!({ "cmd": cmd }));
        if *decision == "allow" {
            assert_eq!(executed["ok"], json!(true), "{cmd}: {executed}");
        } else {
            // 预检说的码，就是真跑会拿到的码。
            assert_eq!(checked["code"], *code, "{cmd}: {checked}");
            assert_eq!(executed["error"]["code"], *code, "{cmd}: {executed}");
            assert_eq!(
                executed["error"]["details"]["reason"], checked["rule"],
                "{cmd}: 预检和执行的原因得一致"
            );
        }
    }
}

/// 策略拒绝 ≠ 程序没装。这两件事下一步完全不同：一个是请用户改配置，另一个
/// 是换个工具或装东西。原来拒绝信息只有一句 `Command is not allowlisted: rg`，
/// 模型分不出来，于是换着花样重试（审查 C02）。
#[test]
fn a_denied_command_still_says_whether_the_program_is_installed() {
    let fx = tiny_js_fixture();
    // 只允许 pwd：python3 因此被拒，但它在这台机器上确实装着（别的测试在跑它）。
    let ctx = common::ctx_with_allowed_commands(&fx.root, "only:pwd");

    let checked = invoke(
        &ctx,
        "check_command",
        json!({ "cmd": format!("{TEST_PYTHON} --version") }),
    );
    assert_ok(&checked);
    assert_eq!(checked["decision"], "deny");
    assert_eq!(checked["rule"], "command_not_allowlisted");
    assert_eq!(checked["denied_stage"], "policy");
    assert_eq!(
        checked["program"]["found"],
        json!(true),
        "程序是装着的，别把策略拒绝说成没装：{checked}"
    );
    assert_eq!(checked["program"]["source"], "path");
    assert_eq!(checked["policy"]["allowlist_mode"], "only");
    assert_eq!(checked["needs_user_authorization"], json!(true));

    // 反过来：名字根本不存在的程序，才是 found=false。
    let missing = invoke(
        &ctx,
        "check_command",
        json!({ "cmd": "gld-no-such-program-xyz" }),
    );
    assert_ok(&missing);
    assert_eq!(missing["program"]["found"], json!(false), "{missing}");
}

/// 预检不跑东西。
///
/// 光看代码"没有 spawn"不算证据——这里让预检的对象是一个**真的会写文件**的
/// 命令：预检之后文件不能出现，真跑之后必须出现。少了后半句，这条测试在脚本
/// 根本不工作时也会通过。
#[test]
fn check_command_does_not_run_anything() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    fs::write(
        fx.root.join("marker.py"),
        "open('marker.txt', 'w').write('ran')\n",
    )
    .expect("write probe script");
    let marker = fx.root.join("marker.txt");

    let checked = invoke(
        &ctx,
        "check_command",
        json!({ "cmd": format!("{TEST_PYTHON} marker.py") }),
    );
    assert_ok(&checked);
    assert_eq!(checked["decision"], "allow", "{checked}");
    assert!(!marker.exists(), "预检把命令跑了");

    let executed = invoke(
        &ctx,
        "exec_command",
        json!({ "cmd": format!("{TEST_PYTHON} marker.py") }),
    );
    assert_eq!(executed["ok"], json!(true), "{executed}");
    assert!(marker.exists(), "探针脚本本身没工作，上面那句断言不算数");
}

/// `argv` 形式：参数一格一格地到达进程，中间没人解释。
///
/// 这是 C04 的核心——以前只有 `cmd` 一个入口，接口长得像 shell、执行却不是，
/// 参数里带 `|` 或空格就得自己琢磨引号。这条测试证明的不是"没报错"，而是
/// **进程真的收到了原样的那个字符串**：脚本把 argv[1] 原样打回来。
#[test]
fn argv_passes_arguments_through_untouched() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let tricky = "a b|c;d\ne";

    let executed = invoke(
        &ctx,
        "exec_command",
        json!({
            "argv": [TEST_PYTHON, "-c", "import sys; sys.stdout.write(sys.argv[1])", tricky]
        }),
    );
    assert_eq!(executed["ok"], json!(true), "{executed}");
    assert_eq!(executed["stdout"], tricky, "参数被谁动过：{executed}");
}

/// 同一个参数，写成 `cmd` 会被 shell 语法检测拒掉，写成 `argv` 放行。
///
/// 两边都不经过 shell；区别在于 `cmd` 那一行**看起来**像 shell，放行等于让
/// 调用方以为 `|` 真的接上了管道。
#[test]
fn a_pipe_character_is_data_in_argv_and_an_operator_in_cmd() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);

    let as_line = invoke(
        &ctx,
        "check_command",
        json!({ "cmd": format!("{TEST_PYTHON} -c print(1|2)") }),
    );
    assert_eq!(as_line["decision"], "deny", "{as_line}");
    assert_eq!(as_line["rule"], "shell_syntax_rejected");

    let as_argv = invoke(
        &ctx,
        "check_command",
        json!({ "argv": [TEST_PYTHON, "-c", "print(1|2)"] }),
    );
    assert_eq!(as_argv["decision"], "allow", "{as_argv}");
}

/// 结构化形式不是权限后门：白名单、危险命令、受保护路径照样管着它。
#[test]
fn argv_goes_through_exactly_the_same_gates_as_cmd() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);

    // 白名单：rg 不在默认名单里，两种形式都拒。
    let not_allowed = invoke(
        &ctx,
        "check_command",
        json!({ "argv": ["rg", "--version"] }),
    );
    assert_eq!(not_allowed["decision"], "deny", "{not_allowed}");
    assert_eq!(not_allowed["rule"], "command_not_allowlisted");
    // 预检和真跑给同一个答案——A01 对 argv 这条新入口的复核，不是只测预检。
    let executed = invoke(&ctx, "exec_command", json!({ "argv": ["rg", "--version"] }));
    assert_eq!(executed["error"]["code"], "POLICY_REJECTED", "{executed}");
    assert_eq!(
        executed["error"]["details"]["reason"], not_allowed["rule"],
        "预检和执行的原因得一致：{executed}"
    );

    // 受保护资产：解释器删 .git，拆成 argv 也一样拦。
    let deletes_git = invoke(
        &ctx,
        "check_command",
        json!({
            "argv": [TEST_PYTHON, "-c", "import shutil; shutil.rmtree('.git')"],
            "confirm": true
        }),
    );
    assert_eq!(deletes_git["decision"], "deny", "{deletes_git}");
    assert_eq!(deletes_git["rule"], "protected_repository_asset");

    // 危险命令：需要确认这件事不因为换了形式就消失。
    let destructive = invoke(
        &ctx,
        "check_command",
        json!({ "argv": ["rm", "-rf", "build"] }),
    );
    assert_eq!(destructive["decision"], "needs_approval", "{destructive}");
}

/// 白名单批准的是系统命令名，跑的就得是系统那个。
///
/// 工作区里放一个同名文件曾经能把它顶替掉：`python --version` 跑的是
/// `<工作区>/python`。批的和跑的不是同一个东西（方案 B、审查 A04）。现在裸名
/// 先查 PATH；工作区里的脚本要么写 `./名字`，要么是 PATH 上根本没有的名字。
#[test]
fn a_bare_command_name_resolves_on_path_not_in_the_workspace() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let impostor = fx.root.join(TEST_PYTHON);
    fs::write(&impostor, "#!/bin/sh\necho impostor\n").expect("写同名文件");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&impostor, fs::Permissions::from_mode(0o755)).expect("加可执行位");
    }

    let checked = invoke(
        &ctx,
        "check_command",
        json!({ "argv": [TEST_PYTHON, "--version"] }),
    );
    assert_eq!(checked["decision"], "allow", "{checked}");
    assert_eq!(
        checked["program"]["source"], "path",
        "白名单批的是系统 python，解析结果却指向工作区：{checked}"
    );

    let executed = invoke(
        &ctx,
        "exec_command",
        json!({ "argv": [TEST_PYTHON, "--version"] }),
    );
    assert_eq!(executed["ok"], json!(true), "{executed}");
    let stdout = executed["stdout"].as_str().unwrap_or_default();
    assert!(
        !stdout.contains("impostor"),
        "跑的是工作区里那个冒名文件：{executed}"
    );

    // 明确指路的形式仍然跑工作区里的东西——这条能力没被收掉。
    let explicit = invoke(
        &ctx,
        "check_command",
        json!({ "argv": [format!("./{TEST_PYTHON}")] }),
    );
    assert_eq!(
        explicit["program"]["source"], "workspace_entry",
        "{explicit}"
    );
}

/// 两个都给不猜哪个算数，也不维护两套权限逻辑。
#[test]
fn giving_cmd_and_argv_together_is_rejected() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let checked = invoke(
        &ctx,
        "check_command",
        json!({ "cmd": "pwd", "argv": ["pwd"] }),
    );
    assert_eq!(checked["decision"], "deny", "{checked}");
    assert_eq!(checked["rule"], "conflicting_command_forms");

    let executed = invoke(
        &ctx,
        "exec_command",
        json!({ "cmd": "pwd", "argv": ["pwd"] }),
    );
    assert_eq!(executed["error"]["code"], "POLICY_REJECTED", "{executed}");
    assert_eq!(
        executed["error"]["details"]["reason"],
        "conflicting_command_forms"
    );
}

/// 内建的 `ls` 不冒充系统 ls：带 flag 直接说清它是什么。
///
/// 以前 `ls -la` 会把 `-la` 当成目录名，报"路径不存在"——看着像目录没了
/// （方案 B 的原生 ls 一条）。
#[test]
fn the_builtin_ls_says_it_is_not_the_system_ls() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let out = invoke(&ctx, "exec_command", json!({ "cmd": "ls -la" }));
    let message = out["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("list_dir"),
        "要指向真能给这些信息的工具：{out}"
    );
}

/// 内建 `ls` 的相对参数按 `workdir` 解析，不是工作区根。
#[test]
fn the_builtin_ls_resolves_relative_paths_against_workdir() {
    let fx = tiny_js_fixture();
    fs::create_dir_all(fx.root.join("pkg/inner")).expect("建子目录");
    fs::write(fx.root.join("pkg/inner/only-here.txt"), "x\n").expect("写文件");
    let ctx = ctx_for(&fx.root);

    let out = invoke(
        &ctx,
        "exec_command",
        json!({ "cmd": "ls inner", "workdir": "pkg" }),
    );
    assert_eq!(out["ok"], json!(true), "{out}");
    assert!(
        out["stdout"]
            .as_str()
            .unwrap_or_default()
            .contains("only-here.txt"),
        "从工作区根解析的话这里会是另一个目录：{out}"
    );
}

/// `gh` 进白名单只等于开了只读诊断。
///
/// 一个字段名就能把 `gh run view` 变成 `gh run rerun`，而"首个单词是 gh"看不出
/// 区别（方案 B 的 GitHub 两行、审查 A05）。名单是允许制，没列的一律拒。
#[test]
fn enabling_gh_opens_read_only_diagnostics_only() {
    let fx = tiny_js_fixture();
    let ctx = common::ctx_with_allowed_commands(&fx.root, "gh");

    for allowed in [
        "gh run list",
        "gh run view 42 --log",
        "gh workflow view ci.yml",
        "gh pr checks",
        "gh version",
    ] {
        let checked = invoke(&ctx, "check_command", json!({ "cmd": allowed }));
        assert_eq!(checked["decision"], "allow", "{allowed}: {checked}");
    }

    for denied in [
        "gh run rerun 42",
        "gh run cancel 42",
        "gh pr merge 7",
        "gh release create v1",
        "gh secret set TOKEN",
        "gh workflow run deploy.yml",
        "gh auth token",
        "gh api /repos/o/r/actions/runs",
        "gh repo clone o/r",
    ] {
        let checked = invoke(&ctx, "check_command", json!({ "cmd": denied }));
        assert_eq!(checked["decision"], "deny", "{denied}: {checked}");
        assert_eq!(
            checked["rule"], "github_command_not_read_only",
            "{denied}: {checked}"
        );
    }
}

/// 没把 gh 加进白名单时，它就是一条普通的未获准命令——不会因为有只读规则
/// 就自己开着。
#[test]
fn gh_is_not_enabled_by_default() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let checked = invoke(&ctx, "check_command", json!({ "cmd": "gh run list" }));
    assert_eq!(checked["decision"], "deny", "{checked}");
    assert_eq!(checked["rule"], "command_not_allowlisted");
    assert_eq!(checked["needs_user_authorization"], json!(true));
}

/// 直连远端的命令有自己的说法，不跟"不在白名单"混成一句。
#[test]
fn ssh_points_at_the_hub_instead_of_a_generic_denial() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    for cmd in [
        "ssh host uptime",
        "scp a.txt host:/tmp/",
        "rsync -a . host:/tmp/",
    ] {
        let checked = invoke(&ctx, "check_command", json!({ "cmd": cmd }));
        assert_eq!(checked["decision"], "deny", "{cmd}: {checked}");
        assert_eq!(
            checked["rule"], "remote_shell_not_allowed",
            "{cmd}: {checked}"
        );
        let suggestion = checked["suggestion"].as_str().unwrap_or_default();
        assert!(suggestion.contains("hub"), "{cmd}: {checked}");
    }
}

/// 被拒之后给的是**已经获准**的替代工具，不是"换个解释器再试"这种绕过办法。
#[test]
fn a_denied_command_points_at_an_allowed_tool() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let checked = invoke(&ctx, "check_command", json!({ "cmd": "rg --version" }));
    assert_ok(&checked);
    let alternatives = checked["alternatives"].as_array().expect("alternatives");
    assert!(
        alternatives
            .iter()
            .any(|item| item["tool"] == "search_text"),
        "{checked}"
    );
    // 白名单不是沙箱，这句话得说出来，别让人以为放行的命令被关着。
    assert_eq!(checked["policy"]["sandbox_enforced"], json!(false));
    assert_eq!(checked["server"]["build_commit"], json!(null));
}

/// 补丁失败不该提示"检查 stderr、exit_code"——那次没有 stderr，也没有
/// exit_code，更不该"重试"：同一个补丁再发一遍还是对不上（审查 D01、A18）。
#[test]
fn a_failed_patch_is_not_told_to_check_stderr() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let out = invoke(
        &ctx,
        "apply_patch",
        json!({"patch": "--- a/TODO.md\n+++ b/TODO.md\n@@\n-no such line in this file\n+x\n"}),
    );
    let error = assert_err(&out);
    assert_eq!(error["error"]["code"], "PATCH_FAILED", "{error}");
    let hint = out["recovery_hint"].as_str().unwrap_or_default();
    assert!(!hint.contains("stderr"), "{hint}");
    assert!(hint.contains("suggested_read_range"), "{hint}");
    assert_eq!(error["error"]["details"]["files_changed"], json!(false));
    assert_eq!(
        error["error"]["details"]["diagnostics"][0]["file"], "TODO.md",
        "{error}"
    );
}

/// 三种输出模式在同一份内容上必须自洽：只列文件的那份就是有匹配的那些文件，
/// 计数加起来就是匹配行数。对不上的话，模型用哪一种得到的结论会不一样。
#[test]
fn the_three_output_modes_describe_the_same_matches() {
    let dir = tempfile::tempdir().expect("workspace");
    fs::write(dir.path().join("a.rs"), "needle\nother\nneedle\n").expect("a");
    fs::write(dir.path().join("b.rs"), "needle\n").expect("b");
    fs::write(dir.path().join("c.rs"), "nothing here\n").expect("c");
    let ctx = ctx_for(dir.path());

    let content = invoke(&ctx, "search_text", json!({"query": "needle"}));
    assert_ok(&content);
    assert_eq!(content["output_mode"], "content");
    assert_eq!(content["total_matches"], json!(3));

    let files = invoke(
        &ctx,
        "search_text",
        json!({"query": "needle", "output_mode": "files_with_matches"}),
    );
    assert_ok(&files);
    let mut listed = files["files"]
        .as_array()
        .expect("files")
        .iter()
        .map(|item| item.as_str().unwrap_or_default().to_string())
        .collect::<Vec<_>>();
    listed.sort();
    assert_eq!(listed, vec!["a.rs", "b.rs"], "{files}");
    assert!(files["matches"].as_array().expect("matches").is_empty());

    let counts = invoke(
        &ctx,
        "search_text",
        json!({"query": "needle", "output_mode": "count"}),
    );
    assert_ok(&counts);
    let total: u64 = counts["counts"]
        .as_array()
        .expect("counts")
        .iter()
        .map(|item| item["count"].as_u64().unwrap_or(0))
        .sum();
    assert_eq!(total, 3, "{counts}");

    // 认不出来的模式是错误，不是悄悄按默认来——那会让模型以为它要的过滤生效了。
    let bad = invoke(
        &ctx,
        "search_text",
        json!({"query": "needle", "output_mode": "lines"}),
    );
    assert_eq!(bad["error"]["code"], "INVALID_ARGUMENT", "{bad}");
}

/// 跨行匹配：一处匹配跨了几行也只报一条，行号是**起点**那一行。
#[test]
fn a_multiline_match_is_reported_once_at_its_first_line() {
    let dir = tempfile::tempdir().expect("workspace");
    fs::write(
        dir.path().join("a.rs"),
        "fn one() {}\n\nfn two() {\n    body();\n}\n",
    )
    .expect("a");
    let ctx = ctx_for(dir.path());

    // 不开 multiline 时，`.` 不跨行，这条正则什么都匹配不到。
    let single = invoke(
        &ctx,
        "search_text",
        json!({"query": r"fn two\(\) \{.*body", "regex": true}),
    );
    assert_ok(&single);
    assert_eq!(single["total_matches"], json!(0), "{single}");

    let multi = invoke(
        &ctx,
        "search_text",
        json!({"query": r"fn two\(\) \{.*body", "regex": true, "multiline": true}),
    );
    assert_ok(&multi);
    assert_eq!(multi["total_matches"], json!(1), "{multi}");
    assert_eq!(multi["matches"][0]["line"], json!(3), "{multi}");
    assert_eq!(multi["matches"][0]["preview"], "fn two() {");
}

/// 类型过滤和 `.github` 的显式搜索：这两件事以前一个没有、一个做不到
/// ——`.github/workflows` 要改的时候连"现在写的是什么"都搜不出来（审查 F03）。
#[test]
fn searching_by_type_and_inside_dot_directories() {
    let dir = tempfile::tempdir().expect("workspace");
    fs::write(dir.path().join("keep.rs"), "target-token\n").expect("rs");
    fs::write(dir.path().join("keep.md"), "target-token\n").expect("md");
    fs::create_dir_all(dir.path().join(".github/workflows")).expect("workflows");
    fs::write(
        dir.path().join(".github/workflows/ci.yml"),
        "name: target-token\n",
    )
    .expect("workflow");
    let ctx = ctx_for(dir.path());

    let rust_only = invoke(
        &ctx,
        "search_text",
        json!({"query": "target-token", "type": "rust"}),
    );
    assert_ok(&rust_only);
    let paths = rust_only["matches"]
        .as_array()
        .expect("matches")
        .iter()
        .map(|item| item["path"].as_str().unwrap_or_default().to_string())
        .collect::<Vec<_>>();
    assert_eq!(paths, vec!["keep.rs"], "{rust_only}");

    // 类型名不认识要报错并列出支持的，不能当成"这个类型里没有匹配"。
    let unknown = invoke(
        &ctx,
        "search_text",
        json!({"query": "target-token", "type": "brainfuck"}),
    );
    assert_eq!(unknown["error"]["code"], "INVALID_ARGUMENT", "{unknown}");
    assert!(unknown["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .contains("rust"));

    // 默认不搜点开头的目录。
    let hidden_off = invoke(
        &ctx,
        "search_text",
        json!({"query": "target-token", "glob": "**/*.yml"}),
    );
    assert_ok(&hidden_off);
    assert_eq!(hidden_off["total_matches"], json!(0), "{hidden_off}");

    let hidden_on = invoke(
        &ctx,
        "search_text",
        json!({"query": "target-token", "glob": "**/*.yml", "include_hidden": true}),
    );
    assert_ok(&hidden_on);
    assert_eq!(hidden_on["total_matches"], json!(1), "{hidden_on}");
    assert_eq!(
        hidden_on["matches"][0]["path"], ".github/workflows/ci.yml",
        "{hidden_on}"
    );
}

/// 版本前置条件的完整来回：read_file 给版本 → 中间有人改了文件 →
/// apply_patch 带着旧版本来，被拒，文件不动（审查 C3、A09）。
#[test]
fn a_patch_built_on_a_stale_read_is_refused() {
    let dir = tempfile::tempdir().expect("workspace");
    let file = dir.path().join("notes.md");
    fs::write(&file, "one\n").expect("write");
    let ctx = ctx_for(dir.path());

    let read = invoke(&ctx, "read_file", json!({"path": "notes.md"}));
    assert_ok(&read);
    let version = read["version"].as_str().expect("version").to_string();

    // 别人改了它（编辑器、另一个 gld、git checkout……）。
    // 版本号是大小加修改时间，所以内容长度也换一下。
    fs::write(&file, "someone else edited this\n").expect("rewrite");

    let patch = "--- a/notes.md\n+++ b/notes.md\n@@\n-one\n+two\n";
    let refused = invoke(
        &ctx,
        "apply_patch",
        json!({"patch": patch, "expected_versions": {"notes.md": version}}),
    );
    let error = assert_err(&refused);
    assert_eq!(error["error"]["code"], "FILE_VERSION_CONFLICT", "{error}");
    let diagnostic = &error["error"]["details"]["diagnostics"][0];
    assert_eq!(diagnostic["reason_code"], "file_changed_since_read");
    assert!(diagnostic["actual_version"].is_string(), "{diagnostic}");
    assert_eq!(error["error"]["details"]["files_changed"], json!(false));
    assert_eq!(
        fs::read_to_string(&file).unwrap(),
        "someone else edited this\n",
        "别人的改动被盖掉了"
    );

    // 恢复提示不能是"检查 stderr"，得说清楚下一步是重读。
    let hint = refused["recovery_hint"].as_str().unwrap_or_default();
    assert!(
        hint.contains("read_file") && !hint.contains("stderr"),
        "{hint}"
    );
}

/// stdin 有三种状态，不是两种（审查 X03、验收 A17）。
///
/// 最要命的是第三条：`yield_time_ms=0` 以前在写 stdin **之前**就返回了，
/// 初始输入整个丢掉，命令拿着一个永远没数据的 stdin 挂到超时。
#[cfg(unix)]
#[test]
fn stdin_has_three_states_and_none_of_them_loses_the_input() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);

    // 一、不给输入 = 起来就关。读 stdin 的命令拿到 EOF 正常结束，不会挂到超时。
    let closed = invoke(
        &ctx,
        "exec_command",
        json!({ "argv": ["cat"], "timeout_ms": 5000 }),
    );
    assert_eq!(closed["ok"], json!(true), "{closed}");
    assert_eq!(closed["stdin_mode"], "close", "{closed}");
    assert_eq!(closed["exit_code"], 0, "cat 该读到 EOF 就退出：{closed}");
    assert_eq!(closed["pty"], json!(false));

    // 二、给了输入 = 写进去再关。
    let once = invoke(
        &ctx,
        "exec_command",
        json!({ "argv": ["cat"], "stdin": "hello\n", "timeout_ms": 5000 }),
    );
    assert_eq!(once["stdin_mode"], "once", "{once}");
    assert_eq!(once["stdout"], "hello\n", "{once}");

    // 三、yield_time_ms=0 直接转后台，初始输入也不能丢。
    let backgrounded = invoke(
        &ctx,
        "exec_command",
        json!({
            "argv": ["cat"],
            "stdin": "kept\n",
            "yield_time_ms": 0,
            "timeout_ms": 5000
        }),
    );
    assert_eq!(backgrounded["stdin_mode"], "once", "{backgrounded}");
    let session_id = backgrounded["session_id"]
        .as_str()
        .expect("转后台要给 session_id")
        .to_string();
    // 命令拿到输入之后就会结束；读回来的必须是那段输入。
    let mut seen = String::new();
    for _ in 0..50 {
        let out = invoke(
            &ctx,
            "read_output",
            json!({ "output_ref": format!("session:{session_id}:stdout") }),
        );
        seen = out["content"].as_str().unwrap_or_default().to_string();
        if !seen.is_empty() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_eq!(seen, "kept\n", "yield_time_ms=0 把初始输入丢了");
}

/// 零结果有好几种，得分得出来是哪一种。
///
/// 以前三种都只回一个空数组：glob 基准写错、文件全被跳过、真的没匹配，看起来
/// 一模一样，而下一步完全不同（审查 F02、复现 E07、验收 A13）。
#[test]
fn an_empty_search_result_says_which_kind_of_empty_it_is() {
    let dir = tempfile::tempdir().expect("workspace");
    fs::create_dir_all(dir.path().join("crates/core")).expect("建目录");
    fs::write(dir.path().join("crates/core/exec.rs"), "fn run() {}\n").expect("写文件");
    let ctx = ctx_for(dir.path());

    // 一、glob 基准写错：在子目录里搜 "exec.rs"，它匹配的是工作区相对全路径。
    let wrong_base = invoke(
        &ctx,
        "search_text",
        json!({ "query": "run", "path": "crates", "glob": "exec.rs" }),
    );
    assert_ok(&wrong_base);
    assert_eq!(wrong_base["total_matches"], 0, "{wrong_base}");
    assert_eq!(wrong_base["scanned_files"], 0, "{wrong_base}");
    assert!(
        wrong_base["rejected_by_glob"].as_u64().unwrap_or(0) > 0,
        "{wrong_base}"
    );
    assert_eq!(wrong_base["glob_base"], "workspace");
    assert_eq!(wrong_base["search_root"], "crates");
    let warning = wrong_base["warnings"][0].as_str().unwrap_or_default();
    assert!(
        warning.contains("**/exec.rs"),
        "要给出能用的写法：{wrong_base}"
    );

    // 二、同一个意图，按工作区基准写就有结果。
    let right_base = invoke(
        &ctx,
        "search_text",
        json!({ "query": "run", "path": "crates", "glob": "**/exec.rs" }),
    );
    assert_eq!(right_base["total_matches"], 1, "{right_base}");
    assert_eq!(right_base["scanned_files"], 1, "{right_base}");

    // 三、真的搜过但没匹配：和上面两种不是一回事。
    let no_match = invoke(
        &ctx,
        "search_text",
        json!({ "query": "gld-no-such-symbol", "path": "crates" }),
    );
    assert_eq!(no_match["total_matches"], 0, "{no_match}");
    assert_eq!(no_match["scanned_files"], 1, "{no_match}");
    let warning = no_match["warnings"][0].as_str().unwrap_or_default();
    assert!(warning.contains("none contained the query"), "{no_match}");
}

/// 被忽略的目录在**遍历阶段**就不进，不是进去之后再逐个文件丢。
///
/// 验收 A14。判据不能是"结果里没有它"——那在剪枝之前也成立；这里让
/// `node_modules` 里躺着一个必然匹配的文件，再确认扫描计数没把它算进去。
#[test]
fn ignored_directories_are_pruned_before_their_files_are_touched() {
    let dir = tempfile::tempdir().expect("workspace");
    fs::create_dir_all(dir.path().join("node_modules/pkg")).expect("建目录");
    fs::write(dir.path().join("node_modules/pkg/index.js"), "needle\n").expect("写文件");
    fs::write(dir.path().join("src.js"), "needle\n").expect("写文件");
    let ctx = ctx_for(dir.path());

    let out = invoke(&ctx, "search_text", json!({ "query": "needle" }));
    assert_ok(&out);
    assert_eq!(out["total_matches"], 1, "node_modules 不该被搜：{out}");
    assert_eq!(
        out["scanned_files"], 1,
        "被忽略目录里的文件连读都不该读：{out}"
    );

    // list_files 同一条规矩。
    let listed = invoke(&ctx, "list_files", json!({ "patterns": ["**/*.js"] }));
    let paths: Vec<&str> = listed["files"]
        .as_array()
        .expect("files")
        .iter()
        .filter_map(|f| f["path"].as_str())
        .collect();
    assert_eq!(paths, vec!["src.js"], "{listed}");
}

/// 工具吐出来的路径，原样喂回工具就得能用。
///
/// 根目录的 `list_dir` 以前给的是 `/Cargo.toml`——前面那个斜杠让 `read_file`
/// 把它当成系统根下的绝对路径，报 `NOT_FOUND`，模型得自己悟出要去掉它
/// （审查 F01、复现 E06）。这条测试串的是 `list_dir → read_file`
/// 和 `list_dir → patch_check`，A12 要的就是这个。
#[test]
fn paths_from_list_dir_can_be_fed_straight_back_in() {
    let dir = tempfile::tempdir().expect("workspace");
    fs::write(dir.path().join("top.md"), "one\n").expect("写根目录文件");
    fs::create_dir_all(dir.path().join("pkg")).expect("建子目录");
    fs::write(dir.path().join("pkg/inner.md"), "two\n").expect("写子目录文件");
    let ctx = ctx_for(dir.path());

    for (listed_at, expected_path, expected_body) in
        [(".", "top.md", "one\n"), ("pkg", "pkg/inner.md", "two\n")]
    {
        let listed = invoke(&ctx, "list_dir", json!({ "path": listed_at }));
        assert_ok(&listed);
        let entry_path = listed["entries"]
            .as_array()
            .expect("entries")
            .iter()
            .find(|entry| entry["type"] == "file")
            .and_then(|entry| entry["path"].as_str())
            .unwrap_or_default()
            .to_string();
        assert_eq!(entry_path, expected_path, "{listed}");
        assert!(
            !entry_path.starts_with('/'),
            "工作区内的路径不带前导斜杠：{listed}"
        );

        let read = invoke(&ctx, "read_file", json!({ "path": entry_path.clone() }));
        assert_ok(&read);
        assert_eq!(read["content"], expected_body, "{read}");

        let checked = invoke(
            &ctx,
            "patch_check",
            json!({
                "patch": format!(
                    "--- a/{entry_path}\n+++ b/{entry_path}\n@@\n-{}+changed\n",
                    expected_body
                )
            }),
        );
        assert_ok(&checked);
    }

    // 根目录自己也要是能往回传的东西，不是空串。
    let root = invoke(&ctx, "list_dir", json!({ "path": "." }));
    assert_eq!(root["path"], ".", "{root}");
}

/// patch_check 回的 `observed_versions` 原样交给 apply_patch 就能过；
/// apply_patch 回的 `file_versions` 又能接着用于下一次改动。
#[test]
fn versions_round_trip_from_patch_check_through_apply_patch() {
    let dir = tempfile::tempdir().expect("workspace");
    fs::write(dir.path().join("notes.md"), "one\n").expect("write");
    let ctx = ctx_for(dir.path());

    let checked = invoke(
        &ctx,
        "patch_check",
        json!({"patch": "--- a/notes.md\n+++ b/notes.md\n@@\n-one\n+two\n"}),
    );
    assert_ok(&checked);
    let observed = checked["observed_versions"].clone();
    assert!(observed["notes.md"].is_string(), "{checked}");

    let applied = invoke(
        &ctx,
        "apply_patch",
        json!({
            "patch": "--- a/notes.md\n+++ b/notes.md\n@@\n-one\n+two\n",
            "expected_versions": observed
        }),
    );
    assert_ok(&applied);
    assert_eq!(
        fs::read_to_string(dir.path().join("notes.md")).unwrap(),
        "two\n"
    );

    // 写完给的是新版本：接着改同一个文件不用再 read_file 一遍。
    let after = applied["file_versions"]["notes.md"]
        .as_str()
        .expect("new version")
        .to_string();
    assert_ne!(Some(after.as_str()), observed["notes.md"].as_str());

    let again = invoke(
        &ctx,
        "apply_patch",
        json!({
            "patch": "--- a/notes.md\n+++ b/notes.md\n@@\n-two\n+three\n",
            "expected_versions": {"notes.md": after}
        }),
    );
    assert_ok(&again);
    assert_eq!(
        fs::read_to_string(dir.path().join("notes.md")).unwrap(),
        "three\n"
    );
}

/// 新建文件的前置条件是"这个路径应当什么都没有"，写成 null。
#[test]
fn a_new_file_precondition_is_null_and_it_is_enforced() {
    let dir = tempfile::tempdir().expect("workspace");
    let ctx = ctx_for(dir.path());
    let patch = "*** Begin Patch\n*** Add File: fresh.txt\n+mine\n*** End Patch\n";

    let created = invoke(
        &ctx,
        "apply_patch",
        json!({"patch": patch, "expected_versions": {"fresh.txt": null}}),
    );
    assert_ok(&created);

    // 同一条补丁再来一次：这回路径上已经有东西了，前置条件不成立。
    let refused = invoke(
        &ctx,
        "apply_patch",
        json!({"patch": patch, "expected_versions": {"fresh.txt": null}}),
    );
    let error = assert_err(&refused);
    assert_eq!(error["error"]["code"], "FILE_VERSION_CONFLICT", "{error}");
    assert_eq!(
        fs::read_to_string(dir.path().join("fresh.txt")).unwrap(),
        "mine\n"
    );
}

/// 前置条件里写了补丁根本不碰的文件：报错。忽略的话，模型会以为自己保护住了
/// 那个文件，而这次调用压根没检查它。
#[test]
fn a_precondition_for_an_untouched_file_is_an_error() {
    let dir = tempfile::tempdir().expect("workspace");
    fs::write(dir.path().join("notes.md"), "one\n").expect("write");
    let ctx = ctx_for(dir.path());

    let out = invoke(
        &ctx,
        "apply_patch",
        json!({
            "patch": "--- a/notes.md\n+++ b/notes.md\n@@\n-one\n+two\n",
            "expected_versions": {"other.md": "1-2"}
        }),
    );
    let error = assert_err(&out);
    assert_eq!(error["error"]["code"], "INVALID_ARGUMENT", "{error}");
    assert!(error["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .contains("other.md"));
    assert_eq!(
        fs::read_to_string(dir.path().join("notes.md")).unwrap(),
        "one\n",
        "参数错了却把文件改了"
    );
}

/// 每一条返回路径都带 operation_id——**包括被拒的那些**。
///
/// 以前 id 是执行到一半由 record_operation 生成的，策略拒绝、Planning 拒绝
/// 这些提前返回的响应根本没有；人拿着模型给的报错在日志里对不上号
///（审查 D02、F）。
#[test]
fn every_response_carries_an_operation_id() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);

    let cases = [
        // 成功、命令被策略拒、补丁对不上、参数不对、工具名不存在
        ("read_file", json!({"path": "package.json"})),
        ("exec_command", json!({"cmd": "rg --version"})),
        (
            "apply_patch",
            json!({"patch": "--- a/TODO.md\n+++ b/TODO.md\n@@\n-nope\n+x\n"}),
        ),
        ("read_file", json!({})),
        ("no_such_tool", json!({})),
    ];
    let mut seen = std::collections::HashSet::new();
    for (name, args) in cases {
        let out = invoke(&ctx, name, args.clone());
        let id = out["operation_id"]
            .as_str()
            .unwrap_or_else(|| panic!("{name} 没有 operation_id: {out}"))
            .to_string();
        assert!(!id.is_empty(), "{name}: {out}");
        // 每次调用一个新的，不能几次共用一个。
        assert!(seen.insert(id), "{name} 复用了别人的 operation_id");
    }
}

/// 被策略拒的调用要留下审计记录，而且用的是同一个 operation_id。
#[test]
fn a_rejected_call_is_recorded_in_the_operation_log() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);

    let refused = invoke(&ctx, "exec_command", json!({"cmd": "rg --version"}));
    assert_err(&refused);
    let operation_id = refused["operation_id"].as_str().expect("id").to_string();

    let log = invoke(&ctx, "operation_log", json!({"limit": 20}));
    let entries = log["operations"]
        .as_array()
        .or_else(|| log["items"].as_array())
        .unwrap_or_else(|| panic!("操作日志的形状变了: {log}"));
    let entry = entries
        .iter()
        .find(|item| item["id"].as_str() == Some(operation_id.as_str()))
        .unwrap_or_else(|| panic!("被拒的调用没有记账: {log}"));
    assert_eq!(entry["kind"], "rejected", "{entry}");
    assert_eq!(entry["tool"], "exec_command");
    assert_eq!(
        entry["result_summary"]["code"], "POLICY_REJECTED",
        "{entry}"
    );
    // 脱敏：命令内容不能进日志。
    assert!(
        !serde_json::to_string(entry)
            .unwrap_or_default()
            .contains("rg --version"),
        "日志里带上了命令内容: {entry}"
    );
}

/// 能力状态只能有一个来源。
///
/// `check_exec_environment` 和 `check_command` 都在讲"这个工作区的执行策略"。
/// 两处各算各的，迟早会说出两套话，而模型没办法知道该信哪个（审查 A）。
/// 这条测试把它们绑在一起：两边同名字段必须逐字相等。
#[test]
fn the_environment_tool_and_the_preflight_report_the_same_policy() {
    let fx = tiny_js_fixture();
    let ctx = common::ctx_with_allowed_commands(&fx.root, "only:pwd,cargo");

    let environment = invoke(&ctx, "check_exec_environment", json!({}));
    assert_ok(&environment);
    let checked = invoke(&ctx, "check_command", json!({"cmd": "cargo --version"}));
    assert_ok(&checked);

    assert_eq!(
        environment["policy"], checked["policy"],
        "两边的策略快照对不上"
    );

    // 顶层那些老字段是兼容别名，值必须来自同一份快照。
    let policy = &checked["policy"];
    assert_eq!(environment["permission_mode"], policy["permission_mode"]);
    assert_eq!(environment["network_allowed"], policy["network_allowed"]);
    assert_eq!(environment["allowed_commands"], policy["allowed_commands"]);
    assert_eq!(
        environment["system_command_allowlist"],
        policy["allowed_commands"]
    );
    assert_eq!(
        environment["workspace_exec_sandbox_enforced"],
        policy["sandbox_enforced"]
    );
    assert_eq!(
        environment["workspace_local_entries"]["enabled"],
        policy["workspace_local_entries"]
    );
    // 收窄过的白名单在两边都要如实反映。
    assert_eq!(policy["allowlist_mode"], "only");
    assert_eq!(environment["preflight"]["tool"], "check_command");
}

/// notebook 从读到改的一整圈：read_notebook 给 cell 视图和版本，
/// apply_patch 的 notebook_edits 按 cell id 改，落盘的还是一份 Jupyter
/// 打得开的 notebook（RFC-0003 G3.2）。
#[test]
fn a_notebook_is_read_as_cells_and_edited_by_cell_id() {
    let dir = tempfile::tempdir().expect("workspace");
    let notebook = dir.path().join("analysis.ipynb");
    let source = r##"{
 "cells": [
  {
   "cell_type": "code",
   "execution_count": 3,
   "id": "aaaa1111",
   "metadata": {},
   "outputs": [
    {
     "name": "stdout",
     "output_type": "stream",
     "text": [
      "42\n"
     ]
    }
   ],
   "source": [
    "print(6 * 7)\n"
   ]
  }
 ],
 "metadata": {
  "language_info": {
   "name": "python"
  }
 },
 "nbformat": 4,
 "nbformat_minor": 5
}
"##;
    fs::write(&notebook, source).expect("write");
    let ctx = ctx_for(dir.path());

    let read = invoke(&ctx, "read_notebook", json!({"path": "analysis.ipynb"}));
    assert_ok(&read);
    assert_eq!(read["total_cells"], json!(1));
    assert_eq!(read["language"], "python");
    assert_eq!(read["nbformat"], "4.5");
    assert_eq!(read["next_start_cell"], json!(null));
    let content = read["content"].as_str().expect("content");
    assert!(content.contains("<cell id=\"aaaa1111\""), "{content}");
    assert!(content.contains("print(6 * 7)"), "{content}");
    assert!(content.contains("42"), "输出也要给出来: {content}");
    let version = read["version"].as_str().expect("version").to_string();

    // read_file 照旧给 JSON 原文：已经有人照着它用普通补丁改 notebook。
    let raw = invoke(&ctx, "read_file", json!({"path": "analysis.ipynb"}));
    assert_ok(&raw);
    assert!(raw["content"]
        .as_str()
        .unwrap_or("")
        .contains("\"nbformat\""));

    let edited = invoke(
        &ctx,
        "apply_patch",
        json!({
            "notebook_edits": [{
                "path": "analysis.ipynb",
                "cells": [
                    {"cell_id": "aaaa1111", "new_source": "print('changed')\n"},
                    {"cell_id": "aaaa1111", "new_source": "# notes\n", "cell_type": "markdown", "edit_mode": "insert"}
                ]
            }],
            "expected_versions": {"analysis.ipynb": version}
        }),
    );
    assert_ok(&edited);
    assert_eq!(edited["files_modified"], json!(["analysis.ipynb"]));

    let after: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&notebook).expect("read")).expect("json");
    let cells = after["cells"].as_array().expect("cells");
    assert_eq!(cells.len(), 2);
    assert_eq!(cells[0]["source"], json!(["print('changed')\n"]));
    // 换了 source 的代码 cell 不能留着旧输出——那是骗人的。
    assert_eq!(cells[0]["outputs"], json!([]));
    assert_eq!(cells[0]["execution_count"], json!(null));
    assert_eq!(cells[1]["cell_type"], "markdown");
    assert!(cells[1]["id"].as_str().expect("id").len() == 8);
    assert!(cells[1].get("outputs").is_none(), "markdown 不该有 outputs");
}

/// cell 编辑和普通补丁共用同一次事务：别的文件失败，notebook 也不落盘。
#[test]
fn a_failed_patch_also_rolls_back_the_notebook_edits() {
    let dir = tempfile::tempdir().expect("workspace");
    let notebook = dir.path().join("a.ipynb");
    let source = r#"{"cells":[{"cell_type":"code","id":"c1","metadata":{},"outputs":[],"source":["x\n"]}],"metadata":{},"nbformat":4,"nbformat_minor":5}"#;
    fs::write(&notebook, source).expect("write");
    fs::write(dir.path().join("notes.md"), "one\n").expect("write");
    let ctx = ctx_for(dir.path());

    let out = invoke(
        &ctx,
        "apply_patch",
        json!({
            // 这一段对不上：整批都不该落盘
            "patch": "--- a/notes.md\n+++ b/notes.md\n@@\n-nope\n+two\n",
            "notebook_edits": [{
                "path": "a.ipynb",
                "cells": [{"cell_id": "c1", "new_source": "y\n"}]
            }]
        }),
    );
    assert_err(&out);
    assert_eq!(
        fs::read_to_string(&notebook).expect("read"),
        source,
        "notebook 跟着别人的失败一起不落盘"
    );
}

/// 改 notebook 时认不出来的 cell id 要报清楚，并且列出真实存在的。
#[test]
fn an_unknown_cell_id_is_reported_with_the_ones_that_exist() {
    let dir = tempfile::tempdir().expect("workspace");
    fs::write(
        dir.path().join("a.ipynb"),
        r#"{"cells":[{"cell_type":"code","id":"real","metadata":{},"outputs":[],"source":["x\n"]}],"metadata":{},"nbformat":4,"nbformat_minor":5}"#,
    )
    .expect("write");
    let ctx = ctx_for(dir.path());

    let out = invoke(
        &ctx,
        "apply_patch",
        json!({
            "notebook_edits": [{"path": "a.ipynb", "cells": [{"cell_id": "ghost", "new_source": "y\n"}]}]
        }),
    );
    let error = assert_err(&out);
    let diagnostic = &error["error"]["details"]["diagnostics"][0];
    assert_eq!(diagnostic["reason_code"], "notebook_edit_failed", "{error}");
    assert_eq!(diagnostic["file"], "a.ipynb");
    assert!(diagnostic["message"]
        .as_str()
        .unwrap_or_default()
        .contains("real"));
}

/// 不是 notebook 的文件要说清楚，而不是抛一个 JSON 解析错。
#[test]
fn reading_something_that_is_not_a_notebook_says_what_is_wrong() {
    let dir = tempfile::tempdir().expect("workspace");
    fs::write(dir.path().join("plain.ipynb"), "{\"a\": 1}\n").expect("write");
    fs::write(dir.path().join("broken.ipynb"), "not json\n").expect("write");
    let ctx = ctx_for(dir.path());

    let out = invoke(&ctx, "read_notebook", json!({"path": "plain.ipynb"}));
    let error = assert_err(&out);
    assert_eq!(error["error"]["code"], "INVALID_ARGUMENT");
    assert!(error["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .contains("not a Jupyter notebook"));

    let broken = invoke(&ctx, "read_notebook", json!({"path": "broken.ipynb"}));
    assert!(assert_err(&broken)["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .contains("not valid JSON"));
}

/// 读不出状态 ≠ 文件不在。
///
/// `current_file_version` 以前把"权限不够、I/O 出错、那是个目录"全变成
/// `None`，而补丁逻辑拿 `None` 当"文件不存在"。于是一个"这路径应当是空的"
/// 前置条件会在文件其实还在的时候通过——那正是它要挡的事（跨仓评审 X02）。
#[test]
fn a_path_whose_state_cannot_be_read_is_not_treated_as_absent() {
    let dir = tempfile::tempdir().expect("workspace");
    // 目录：存在，但不是能比版本的文件。metadata 成功，is_file 是 false。
    fs::create_dir(dir.path().join("data")).expect("mkdir");
    let ctx = ctx_for(dir.path());

    // 说"data 这个路径应当什么都没有"——它其实有东西，必须拒。
    let out = invoke(
        &ctx,
        "apply_patch",
        json!({
            "patch": "--- a/data\n+++ b/data\n@@\n-x\n+y\n",
            "expected_versions": {"data": null}
        }),
    );
    let error = assert_err(&out);
    assert_eq!(error["error"]["code"], "FILE_VERSION_CONFLICT", "{error}");
    let diagnostic = &error["error"]["details"]["diagnostics"][0];
    assert!(
        diagnostic["message"]
            .as_str()
            .unwrap_or_default()
            .contains("cannot tell the state"),
        "{diagnostic}"
    );
    assert_eq!(error["error"]["details"]["files_changed"], json!(false));

    // 没给前置条件也一样拒：状态都问不出来，落盘前的复核同样没法做。
    let bare = invoke(
        &ctx,
        "apply_patch",
        json!({"patch": "--- a/data\n+++ b/data\n@@\n-x\n+y\n"}),
    );
    assert_eq!(
        assert_err(&bare)["error"]["code"],
        "FILE_VERSION_CONFLICT",
        "{bare}"
    );
}

/// 三态里"确实不在"那一格照旧：说它应当不在、它确实不在，就该放行。
#[test]
fn a_precondition_of_absent_still_passes_when_the_path_really_is_empty() {
    let dir = tempfile::tempdir().expect("workspace");
    let ctx = ctx_for(dir.path());
    let created = invoke(
        &ctx,
        "apply_patch",
        json!({
            "patch": "*** Begin Patch\n*** Add File: fresh.txt\n+mine\n*** End Patch\n",
            "expected_versions": {"fresh.txt": null}
        }),
    );
    assert_ok(&created);
    assert_eq!(
        fs::read_to_string(dir.path().join("fresh.txt")).expect("read"),
        "mine\n"
    );
}

/// 参数名写错了要当场说，不能按默认值跑一遍（审查 A19，U5）。
///
/// `timeout` 不是 `exec_command` 的参数，`timeout_ms` 才是。以前这条调用会
/// **成功**，用默认的 30 秒跑——调用方以为自己给了 10 分钟，命令在第 30 秒被
/// 杀掉，返回值里没有任何地方说过那个 `timeout` 被扔了。看着像"这条命令莫名
/// 其妙超时"，实际是参数根本没生效。
#[test]
fn a_misspelled_argument_is_refused_instead_of_silently_dropped() {
    let fx = tiny_js_fixture();
    let ctx = ctx_with_allowed_commands(&fx.root, TEST_PYTHON);
    let out = invoke(
        &ctx,
        "exec_command",
        json!({"cmd": format!("{TEST_PYTHON} -c pass"), "timeout": 600_000}),
    );
    let error = assert_err(&out);
    assert_eq!(error["error"]["code"], "INVALID_ARGUMENT", "{error}");
    // 报错要能直接改：多了哪个、真名叫什么，都在里面。
    let message = error["error"]["message"].as_str().unwrap_or_default();
    assert!(message.contains("timeout_ms"), "{message}");
    assert_eq!(
        error["error"]["details"]["unknown_arguments"],
        json!(["timeout"])
    );
    // 被拒的调用没启动任何进程，也就没有需要善后的东西。
    assert_eq!(error["error"]["details"]["executed"], json!(false));

    // 名字写对就照常跑。
    let ok = invoke(
        &ctx,
        "exec_command",
        json!({"cmd": format!("{TEST_PYTHON} -c pass"), "timeout_ms": 60_000}),
    );
    assert_ok(&ok);
}

/// 预检要能预检**那一次**调用：`apply_patch` 收的参数，`patch_check` 也得收。
///
/// `.github/workflows/` 下的改动要 `confirm`。`patch_check` 以前不收 `confirm`，
/// 于是想先试一遍的人只能拿到"需要确认"，没法知道确认之后还会不会有别的问题；
/// 现在多给的参数会被拒，这个缺口更是直接变成"预检根本发不出去"。
#[test]
fn patch_check_takes_the_same_arguments_apply_patch_does() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let patch =
        "*** Begin Patch\n*** Add File: .github/workflows/ci.yml\n+name: ci\n*** End Patch\n";

    let unconfirmed = invoke(&ctx, "patch_check", json!({"patch": patch}));
    assert_eq!(
        assert_err(&unconfirmed)["error"]["code"],
        "DANGEROUS_OPERATION_REQUIRES_CONFIRMATION",
        "{unconfirmed}"
    );

    let confirmed = invoke(
        &ctx,
        "patch_check",
        json!({"patch": patch, "confirm": true}),
    );
    let preflight = assert_ok(&confirmed);
    assert_eq!(preflight["preflight"], json!(true));
    assert!(
        !fx.root.join(".github/workflows/ci.yml").exists(),
        "预检不能落盘"
    );

    // 同一组参数真跑一遍，结论要一致。
    let applied = invoke(
        &ctx,
        "apply_patch",
        json!({"patch": patch, "confirm": true}),
    );
    assert_ok(&applied);
    assert!(fx.root.join(".github/workflows/ci.yml").exists());

    // `dry_run` 是 patch_check 自己定死的，收进来只会让人以为能关掉。
    let confusing = invoke(
        &ctx,
        "patch_check",
        json!({"patch": patch, "dry_run": false}),
    );
    assert_eq!(
        assert_err(&confusing)["error"]["code"],
        "INVALID_ARGUMENT",
        "{confusing}"
    );
}

/// 服务端自己往参数里塞的键（MCP server 的 `_host_session_key`）不能被当成
/// 未知参数拒掉——它本来就不在 schema 里。
#[test]
fn the_servers_own_internal_keys_are_not_mistaken_for_client_mistakes() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let out = invoke(
        &ctx,
        "list_dir",
        json!({"path": ".", "_host_session_key": "chatgpt-session"}),
    );
    assert_ok(&out);
}

/// 零结果的第四种："这下面确实没有，但有一棵目录我压根没进去"（U5，验收 A14）。
///
/// `.github` 是点开头的，默认不搜。以前这种情况报的是"searched N file(s),
/// none contained the query"——话没错，但它和"这个仓库真的没有 workflow"
/// 长得一模一样，而下一步完全不同：一个是换关键词，一个是加 include_hidden。
#[test]
fn a_search_that_never_entered_a_dot_directory_says_so() {
    let dir = tempfile::tempdir().expect("workspace");
    fs::create_dir_all(dir.path().join(".github/workflows")).expect("mkdir");
    fs::write(
        dir.path().join(".github/workflows/ci.yml"),
        "runs-on: ubuntu-latest\n",
    )
    .expect("workflow");
    fs::write(dir.path().join("main.rs"), "fn main() {}\n").expect("src");
    // 这一棵是 include_ignored 管的，不该被算成"隐藏目录"。
    fs::create_dir_all(dir.path().join("node_modules/x")).expect("mkdir");
    fs::write(dir.path().join("node_modules/x/i.js"), "runs-on\n").expect("dep");
    let ctx = ctx_for(dir.path());

    let blind = invoke(&ctx, "search_text", json!({"query": "runs-on"}));
    let payload = assert_ok(&blind);
    assert_eq!(payload["total_matches"], json!(0), "{payload}");
    assert_eq!(payload["skipped_hidden_dirs"], json!(1), "{payload}");
    let warnings = payload["warnings"].as_array().cloned().unwrap_or_default();
    assert!(
        warnings.iter().any(|w| {
            let text = w.as_str().unwrap_or_default();
            text.contains(".github") && text.contains("include_hidden=true")
        }),
        "没说清 .github 整棵没搜：{payload}"
    );
    // 两句话都要在：搜过的那些里没有，另外还有一棵没进去。
    assert!(
        warnings
            .iter()
            .any(|w| w.as_str().unwrap_or_default().contains("none contained")),
        "{payload}"
    );

    // 加上开关就找得到，而且不再提这句。
    let seeing = invoke(
        &ctx,
        "search_text",
        json!({"query": "runs-on", "include_hidden": true}),
    );
    let payload = assert_ok(&seeing);
    assert_eq!(payload["total_matches"], json!(1), "{payload}");
    assert_eq!(payload["skipped_hidden_dirs"], json!(0), "{payload}");

    // list_files 是同一件事：`**/*.yml` 空手而归时也得说为什么。
    let listed = invoke(&ctx, "list_files", json!({"patterns": ["**/*.yml"]}));
    let payload = assert_ok(&listed);
    assert!(payload["files"].as_array().is_some_and(|f| f.is_empty()));
    assert!(
        payload["warnings"]
            .as_array()
            .map(|w| w.iter().any(|item| item
                .as_str()
                .unwrap_or_default()
                .contains("include_hidden=true")))
            .unwrap_or(false),
        "{payload}"
    );
}
