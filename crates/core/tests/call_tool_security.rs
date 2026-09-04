mod common;

use std::fs;

use common::*;
use serde_json::json;

const TRAVERSAL_PATCH: &str = r#"*** Begin Patch
*** Update File: ../outside-secret.txt
@@
-TOP_SECRET_DO_NOT_READ
+unsafe
*** End Patch
"#;

#[test]
fn read_file_rejects_symlink_escape() {
    let fx = malicious_fixture();
    let ctx = ctx_for(&fx.root);
    let out = invoke(&ctx, "read_file", json!({"path": "outside-link.txt"}));
    if fx.root.join("outside-link.txt").exists() {
        assert_security_or_policy_err(&out);
    }
}

#[test]
fn apply_patch_rejects_traversal_target() {
    let fx = malicious_fixture();
    let ctx = ctx_for(&fx.root);
    let out = invoke(&ctx, "apply_patch", json!({"patch": TRAVERSAL_PATCH}));
    assert_security_or_policy_err(&out);
}

/// 关掉 confine-reads 之后，绝对路径能读到外面。
///
/// 默认是关不掉的那一侧（0.3.0 起 confine-reads=true）；这条测的是
/// "开关真的有用"，对应的默认行为由 read_file_rejects_absolute_paths_by_default 守着。
#[test]
fn read_file_allows_explicit_external_read_only_path_when_confinement_is_off() {
    let fx = malicious_fixture();
    let out = invoke(
        &common::ctx_for_unconfined_reads(&fx.root),
        "read_file",
        json!({"path": fx.outside_secret.to_string_lossy()}),
    );
    let result = assert_ok(&out);
    assert!(result["content"]
        .as_str()
        .unwrap_or("")
        .contains("TOP_SECRET"));
}

/// 关掉 confine-reads 之后，遍历类工具也能走到外面。
#[test]
fn external_read_tools_allow_directory_listing_and_search_when_confinement_is_off() {
    let fx = malicious_fixture();
    let ctx = common::ctx_for_unconfined_reads(&fx.root);
    let parent = fx.outside_secret.parent().expect("外部目录");
    let parent_text = parent.to_string_lossy().to_string();

    let listed_result = invoke(&ctx, "list_dir", json!({"path": parent_text}));
    let listed = assert_ok(&listed_result);
    assert!(listed["entries"]
        .as_array()
        .unwrap()
        .iter()
        .any(|entry| entry["name"] == "outside-secret.txt"));

    let files_result = invoke(
        &ctx,
        "list_files",
        json!({"path": parent.to_string_lossy(), "patterns": ["**/*"]}),
    );
    let files = assert_ok(&files_result);
    assert!(files["files"]
        .as_array()
        .unwrap()
        .iter()
        .any(|file| file["path"]
            .as_str()
            .unwrap_or("")
            .ends_with("outside-secret.txt")));

    let matches_result = invoke(
        &ctx,
        "search_text",
        json!({"path": parent.to_string_lossy(), "query": "TOP_SECRET"}),
    );
    let matches = assert_ok(&matches_result);
    assert!(matches["matches"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item["path"]
            .as_str()
            .unwrap_or("")
            .ends_with("outside-secret.txt")));
}

/// 关掉 confine-reads 之后，view_image 也能读外面的图。
#[test]
fn view_image_allows_explicit_external_read_only_path_when_confinement_is_off() {
    let fx = malicious_fixture();
    let image_path = fx
        .outside_secret
        .parent()
        .expect("外部目录")
        .join("outside-probe.png");
    let png_1x1: &[u8] = &[
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 6,
        0, 0, 0, 31, 21, 196, 137, 0, 0, 0, 13, 73, 68, 65, 84, 120, 156, 99, 248, 207, 192, 240,
        31, 0, 5, 0, 1, 255, 137, 153, 61, 29, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
    ];
    fs::write(&image_path, png_1x1).expect("写入测试图片");
    let result = invoke(
        &common::ctx_for_unconfined_reads(&fx.root),
        "view_image",
        json!({"path": image_path.to_string_lossy(), "output": "data_url"}),
    );
    let payload = assert_ok(&result);
    let canonical_image_path = fs::canonicalize(&image_path).expect("规范化测试图片路径");
    assert_eq!(
        payload["path"],
        canonical_image_path
            .to_string_lossy()
            .replace('\\', "/")
            .trim_start_matches("//?/")
    );
    assert!(payload["base64"].as_str().is_some());
}

#[test]
fn exec_command_rejects_workdir_escape_via_policy() {
    assert_policy_rejects("exec_command", json!({"cmd": "pwd", "workdir": ".."}));
}

#[test]
fn exec_command_allows_workspace_child_process_during_transition() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let out = invoke(&ctx, "exec_command", json!({"cmd": "python --version"}));
    let result = assert_ok(&out);
    assert_eq!(result["filesystem_scope"], "workspace");
    assert_eq!(result["sandbox_enforced"], false);
    assert_eq!(result["child_process"], true);
}

#[test]
fn exec_command_rejects_host_scope_even_with_confirmation() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let out = invoke(
        &ctx,
        "exec_command",
        json!({
            "cmd": "python --version",
            "filesystem_scope": "host",
            "confirm": true
        }),
    );
    assert_eq!(out["error"]["code"], "POLICY_REJECTED");
    assert!(out["summary"]
        .as_str()
        .unwrap_or("")
        .contains("EXTERNAL_EXECUTION_NOT_ALLOWED"));
}

#[test]
fn exec_command_rejects_disallowed_destructive_command() {
    assert_policy_rejects("exec_command", json!({"cmd": "rm -rf /"}));
}

#[test]
fn dangerous_command_requires_explicit_confirmation() {
    let policy = gld_core::tools::policy::PolicySettings::default();
    let accepted = gld_core::tools::policy::validate_tool_arguments(
        "exec_command",
        &json!({"cmd": "git reset --hard HEAD", "confirm": true}),
        &policy,
    );
    assert!(accepted.is_ok());
}

#[test]
fn deleting_readme_requires_explicit_confirmation() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let out = invoke(
        &ctx,
        "apply_patch",
        json!({
            "patch": "--- a/README.md\n+++ /dev/null\n@@\n-project\n"
        }),
    );
    assert_eq!(
        out["error"]["code"],
        "DANGEROUS_OPERATION_REQUIRES_CONFIRMATION"
    );
}

#[test]
fn deleting_git_assets_is_always_rejected() {
    let fx = tiny_js_fixture();
    let git_dir = fx.root.join(".git");
    fs::create_dir_all(&git_dir).expect("创建 git 目录");
    fs::write(git_dir.join("config"), "[core]\n").expect("创建 git 配置");
    let ctx = ctx_for(&fx.root);
    let out = invoke(
        &ctx,
        "apply_patch",
        json!({
            "confirm": true,
            "patch": "--- a/.git/config\n+++ /dev/null\n@@\n-[core]\n"
        }),
    );
    assert_eq!(out["error"]["code"], "PROTECTED_REPOSITORY_ASSET");
}

#[test]
fn patch_check_rejects_all_git_and_github_writes() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    for path in [".git/probe.txt", ".github/probe.yml"] {
        let out = invoke(
            &ctx,
            "apply_patch",
            json!({
                "dry_run": true,
                "patch": format!("*** Begin Patch\n*** Add File: {path}\n+probe\n*** End Patch\n")
            }),
        );
        assert_eq!(out["error"]["code"], "PROTECTED_REPOSITORY_ASSET");
    }
}

#[test]
fn destructive_command_targeting_git_is_always_rejected() {
    let policy = gld_core::tools::policy::PolicySettings::default();
    let error = gld_core::tools::policy::validate_tool_arguments(
        "exec_command",
        &json!({"cmd": "rm -rf .git", "confirm": true}),
        &policy,
    )
    .expect_err("删除 .git 必须拒绝");
    assert!(error.0.contains("PROTECTED_REPOSITORY_ASSET"));
}

#[test]
fn interpreter_command_cannot_delete_git_assets() {
    let policy = gld_core::tools::policy::PolicySettings::default();
    let error = gld_core::tools::policy::validate_tool_arguments(
        "exec_command",
        &json!({
            "cmd": "python -c \"import shutil; shutil.rmtree('.git')\"",
            "confirm": true
        }),
        &policy,
    )
    .expect_err("解释器删除 .git 必须拒绝");
    assert!(error.0.contains("PROTECTED_REPOSITORY_ASSET"));
}

#[test]
fn interpreter_command_cannot_delete_github_assets() {
    let policy = gld_core::tools::policy::PolicySettings::default();
    let error = gld_core::tools::policy::validate_tool_arguments(
        "exec_command",
        &json!({
            "cmd": "python -c \"import os; os.remove('.github/workflows/ci.yml')\"",
            "confirm": true
        }),
        &policy,
    )
    .expect_err("解释器删除 .github 必须拒绝");
    assert!(error.0.contains("PROTECTED_REPOSITORY_ASSET"));
}

#[test]
fn interpreter_command_cannot_write_outside_workspace_scope() {
    let policy = gld_core::tools::policy::PolicySettings::default();
    let error = gld_core::tools::policy::validate_tool_arguments(
        "exec_command",
        &json!({
            "cmd": "python -c \"from pathlib import Path; Path('../outside.txt').write_text('x')\"",
            "filesystem_scope": "workspace"
        }),
        &policy,
    )
    .expect_err("workspace scope 不得写入外部路径");
    assert!(error.0.contains("WORKSPACE_PATH_PROTECTED"));
}

#[test]
fn interpreter_command_cannot_write_git_files() {
    let policy = gld_core::tools::policy::PolicySettings::default();
    let error = gld_core::tools::policy::validate_tool_arguments(
        "exec_command",
        &json!({
            "cmd": "python -c \"from pathlib import Path; Path('.git/config').write_text('x')\"",
            "filesystem_scope": "workspace"
        }),
        &policy,
    )
    .expect_err("普通解释器不得写入 .git");
    assert!(error.0.contains("PROTECTED_REPOSITORY_ASSET"));
}

#[test]
fn apply_patch_allows_modifying_a_normal_file() {
    let fx = tiny_js_fixture();
    let target = fx.root.join("src/normal.txt");
    fs::write(&target, "before\n").expect("创建待修改文件");
    let ctx = ctx_for(&fx.root);
    let out = invoke(
        &ctx,
        "apply_patch",
        json!({
            "patch": "--- a/src/normal.txt\n+++ b/src/normal.txt\n@@\n-before\n+after\n"
        }),
    );
    assert_ok(&out);
    assert!(fs::read_to_string(target)
        .expect("读取修改后的文件")
        .contains("after"));
}

#[test]
fn apply_patch_allows_deleting_a_normal_file() {
    let fx = tiny_js_fixture();
    let target = fx.root.join("src/delete-me.js");
    fs::write(&target, "delete me\n").expect("创建待删除文件");
    let ctx = ctx_for(&fx.root);
    let out = invoke(
        &ctx,
        "apply_patch",
        json!({
            "patch": "--- a/src/delete-me.js\n+++ /dev/null\n@@\n-delete me\n"
        }),
    );
    assert_ok(&out);
    assert!(!target.exists());
}

#[test]
fn apply_patch_rejects_absolute_path_target() {
    let fx = tiny_js_fixture();
    let ctx = ctx_for(&fx.root);
    let out = invoke(
        &ctx,
        "apply_patch",
        json!({
            "patch": "--- a/C:/outside-secret.txt\n+++ b/C:/outside-secret.txt\n@@\n-TOP_SECRET_DO_NOT_READ\n+unsafe\n"
        }),
    );
    assert_security_or_policy_err(&out);
}

#[test]
fn exec_command_allows_python_c_but_rejects_shell_escape() {
    let policy = gld_core::tools::policy::PolicySettings::default();
    assert!(gld_core::tools::policy::validate_tool_arguments(
        "exec_command",
        &json!({"cmd": "python -c \"import os; print(os.getcwd())\""}),
        &policy,
    )
    .is_ok());
    assert_policy_rejects(
        "exec_command",
        json!({"cmd": "python -c \"print(1)\" && rm -rf /"}),
    );
}

#[test]
fn exec_command_rejects_shell_chaining() {
    assert_policy_rejects("exec_command", json!({"cmd": "echo hi && rm -rf /"}));
}

#[test]
fn safe_permission_mode_blocks_network_looking_command() {
    let policy = gld_core::tools::policy::PolicySettings {
        permission_mode: "safe".into(),
        ..Default::default()
    };
    let err = gld_core::tools::policy::validate_tool_arguments(
        "exec_command",
        &json!({"cmd": "curl https://example.com"}),
        &policy,
    )
    .expect_err("network command should be blocked in safe mode");
    assert!(err.0.contains("Network-looking"));
}

/// 默认（confine-reads=true）下，四个读工具都不能碰 Workspace 外面。
///
/// 上面几条 `..._when_confinement_is_off` 验的是"开关能放开"，
/// 这条验的是"默认真的关着"。两侧都要有，否则改坏一边不会被发现。
#[test]
fn every_read_tool_stays_inside_the_workspace_by_default() {
    let fx = malicious_fixture();
    let ctx = ctx_for(&fx.root);
    let outside_file = fx.outside_secret.to_string_lossy().to_string();
    let outside_dir = fx
        .outside_secret
        .parent()
        .expect("外部目录")
        .to_string_lossy()
        .to_string();

    for (tool, args) in [
        ("read_file", json!({ "path": &outside_file })),
        ("list_dir", json!({ "path": &outside_dir })),
        (
            "list_files",
            json!({ "path": &outside_dir, "patterns": ["**/*"] }),
        ),
        (
            "search_text",
            json!({ "path": &outside_dir, "query": "TOP_SECRET" }),
        ),
        ("view_image", json!({ "path": &outside_file })),
    ] {
        let out = invoke(&ctx, tool, args);
        let err = assert_err(&out);
        assert_eq!(
            err["error"]["code"], "READS_CONFINED_TO_WORKSPACE",
            "{tool} 应当被限制在 Workspace 内：{out}"
        );
    }

    // 反向断言：工作区内的文件照常读，别把默认做成"什么都读不了"。
    assert_ok(&invoke(&ctx, "list_dir", json!({ "path": "." })));
}
