use super::commit::faults;
use super::parse::{Hunk, HunkLine};
use super::*;
use serde_json::json;
use tempfile::tempdir;

fn apply_patch(ws: &Workspace, args: &Value) -> Result<Value, WorkspaceError> {
    let request = PatchRequest::from_args(args)?;
    match apply(ws, request, |_, _, _| false, || {})? {
        PatchOutcome::Checked(result) | PatchOutcome::Applied { result, .. } => Ok(result),
    }
}

fn patch_check(ws: &Workspace, args: &Value) -> Result<Value, WorkspaceError> {
    let mut args = args.clone();
    args["dry_run"] = Value::Bool(true);
    apply_patch(ws, &args)
}

fn workspace_with_file() -> (tempfile::TempDir, Workspace) {
    let workspace = tempdir().expect("workspace");
    std::fs::write(workspace.path().join("main.rs"), "old\n").expect("file");
    let context = Workspace::new(workspace.path().to_path_buf(), Vec::new()).expect("workspace");
    (workspace, context)
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
    let script = workspace.path().join("run.sh");
    std::fs::write(&script, "#!/bin/sh\nold\n").expect("写脚本");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("设权限");
    let context = Workspace::new(workspace.path().to_path_buf(), Vec::new()).expect("workspace");

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
    let (workspace, context) = workspace_with_file();
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
    let (_workspace, context) = workspace_with_file();
    let result = patch_check(&context, &patch()).expect("patch check");
    assert_eq!(result["preflight"], true);
    assert_eq!(
        std::fs::read_to_string(context.root().join("main.rs")).unwrap(),
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
    let (_workspace, context) = workspace_with_file();
    let result = apply_patch(
        &context,
        &json!({
            "patch": "*** Begin Patch\n*** Delete File: main.rs\n*** Add File: main.rs\n+fresh\n*** End Patch\n"
        }),
    )
    .expect("replace file");
    assert_eq!(result["files_modified"], json!(["main.rs"]));
    assert_eq!(
        std::fs::read_to_string(context.root().join("main.rs")).unwrap(),
        "fresh\n"
    );
}

#[test]
fn validation_failure_in_later_file_keeps_all_files_unchanged() {
    let (_workspace, context) = workspace_with_file();
    let error = apply_patch(
        &context,
        &json!({
            "patch": "--- a/main.rs\n+++ b/main.rs\n@@\n-old\n+new\n--- a/missing.rs\n+++ b/missing.rs\n@@\n-old\n+new\n"
        }),
    )
    .expect_err("later file fails preflight");
    assert_eq!(error.to_error_value()["code"], "NOT_FOUND");
    assert_eq!(
        std::fs::read_to_string(context.root().join("main.rs")).unwrap(),
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
    let body = (1..=60)
        .map(|n| format!("line {n}"))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(workspace.path().join("m.txt"), format!("{body}\n")).expect("file");
    let context = Workspace::new(workspace.path().to_path_buf(), Vec::new()).expect("workspace");

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
    std::fs::write(workspace.path().join("m.txt"), "alpha\nbeta\ngamma\n").expect("file");
    let context = Workspace::new(workspace.path().to_path_buf(), Vec::new()).expect("workspace");

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
    std::fs::write(workspace.path().join("a.txt"), "a-old\n").expect("a");
    std::fs::write(workspace.path().join("b.txt"), "b-old\n").expect("b");
    let context = Workspace::new(workspace.path().to_path_buf(), Vec::new()).expect("workspace");

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
    std::fs::write(workspace.path().join("m.txt"), "one\ntwo\n").expect("file");
    let context = Workspace::new(workspace.path().to_path_buf(), Vec::new()).expect("workspace");

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
    let (_workspace, context) = workspace_with_file();
    let main = context.root().join("main.rs");
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
    let (_workspace, context) = workspace_with_file();
    let fresh = context.root().join("fresh.txt");
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
    let (_workspace, context) = workspace_with_file();
    let main = context.root().join("main.rs");
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
    let body = (1..=30)
        .map(|n| format!("line {n}"))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(workspace.path().join("m.txt"), format!("{body}\n")).expect("file");
    let context = Workspace::new(workspace.path().to_path_buf(), Vec::new()).expect("workspace");

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
    let patch = "*** Begin Patch\n*** Update File: m.rs\n@@ fn b() {\n-  x\n+  y\n*** End Patch\n";
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
    let patch = "--- a/m.txt\n+++ b/m.txt\n@@ -1,1 +1,3 @@\n x\n+p\n+q\n@@ -5,1 +7,1 @@\n-x\n+z\n";
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
        let (_workspace, context) = workspace_with_file();
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
            std::fs::read_to_string(context.root().join("main.rs")).unwrap(),
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
    std::fs::write(workspace.path().join("m.txt"), "a\nb\n").expect("file");
    let context = Workspace::new(workspace.path().to_path_buf(), Vec::new()).expect("workspace");

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
    let (_workspace, context) = workspace_with_file();
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
        std::fs::read_to_string(context.root().join("main.rs")).unwrap(),
        "old\n"
    );
}

/// 不认识的 `*** ` 指令（比如真 Codex 有、这里没实现的 `*** Move to:`）
/// 今天被静默跳过：模型以为文件挪了，实际没挪。
#[test]
fn an_unsupported_codex_directive_is_refused_instead_of_skipped() {
    let (_workspace, context) = workspace_with_file();
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
        std::fs::read_to_string(context.root().join("main.rs")).unwrap(),
        "old\n"
    );
    assert!(!context.root().join("moved.rs").exists());
}

/// 上下文在文件里出现多次、又没有行号也没有锚点时，今天挑第一处改。
/// 挑错了就是改错地方，而且看起来是成功的。**只在这个 hunk 会删/改行时
/// 才算歧义**：纯插入挑哪一处都不破坏原有内容，而且现有补丁大量这么写。
#[test]
fn an_ambiguous_change_names_the_candidates_instead_of_guessing() {
    let workspace = tempdir().expect("workspace");
    std::fs::write(workspace.path().join("m.txt"), "x\ny\nx\n").expect("file");
    let context = Workspace::new(workspace.path().to_path_buf(), Vec::new()).expect("workspace");

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
    let (_workspace, context) = workspace_with_file();
    let target = context.root().join("main.rs");
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
    std::fs::write(workspace.path().join("a.txt"), "a-old\n").expect("a");
    std::fs::write(workspace.path().join("b.txt"), "b-old\n").expect("b");
    let context = Workspace::new(workspace.path().to_path_buf(), Vec::new()).expect("workspace");
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
    std::fs::write(workspace.path().join("a.txt"), "a-old\n").expect("a");
    std::fs::write(workspace.path().join("b.txt"), "b-old\n").expect("b");
    let context = Workspace::new(workspace.path().to_path_buf(), Vec::new()).expect("workspace");
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

#[test]
fn commit_notification_follows_success_and_precedes_version_sampling() {
    use std::cell::Cell;

    let (_directory, ws) = workspace_with_file();
    let notified = Cell::new(false);
    for args in [
        json!({ "patch": patch()["patch"], "dry_run": true }),
        json!({ "patch": "--- a/main.rs\n+++ b/main.rs\n@@\n-missing\n+new\n" }),
    ] {
        let request = PatchRequest::from_args(&args).expect("request");
        let _ = apply(&ws, request, |_, _, _| false, || notified.set(true));
        assert!(!notified.get(), "preflight and failure must not notify");
    }

    let path = ws.root().join("main.rs");
    let before = crate::workspace::current_file_version(&path).expect("old version");
    let args = patch();
    let request = PatchRequest::from_args(&args).expect("request");
    let outcome = apply(
        &ws,
        request,
        |_, _, _| false,
        || {
            assert_eq!(std::fs::read_to_string(&path).unwrap(), "new\n");
            notified.set(true);
            // An external edit at this boundary makes the sampling order observable.
            std::fs::write(&path, "after notification\n").expect("external edit");
        },
    )
    .expect("apply");
    let PatchOutcome::Applied {
        result,
        previous_versions,
    } = outcome
    else {
        panic!("a real commit must return Applied");
    };
    assert!(notified.get());
    assert_eq!(previous_versions["main.rs"], before);
    assert_eq!(
        result["file_versions"]["main.rs"],
        crate::workspace::current_file_version(&path).unwrap()
    );
}
