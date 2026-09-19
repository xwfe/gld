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
    assert_eq!(payload["stdin_open"], true);
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
