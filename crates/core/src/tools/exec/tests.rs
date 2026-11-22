// Command behavior through core dispatch: policy, caller-scoped output and patch coordination.

use super::*;
use crate::tools::dispatch::call_tool;
use serde_json::json;
#[cfg(unix)]
use std::path::Path;
#[cfg(unix)]
use std::time::{Duration, Instant};
use tempfile::tempdir;

/// A workspace script that starts `sleep` in the background and waits:
/// `sleep` is a grandchild of gld, not its child.
#[cfg(unix)]
fn tree_workspace() -> (tempfile::TempDir, tempfile::TempDir, ToolContext) {
    use std::os::unix::fs::PermissionsExt;
    let workspace = tempdir().expect("workspace");
    let harness = tempdir().expect("harness");
    let script = workspace.path().join("spawn-tree");
    std::fs::write(
        &script,
        "#!/bin/sh\nsleep 30 &\necho $! > grandchild.pid\nwait\n",
    )
    .expect("script");
    let mut permissions = std::fs::metadata(&script).expect("meta").permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&script, permissions).expect("chmod");
    let ctx = ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
        .expect("context");
    (workspace, harness, ctx)
}

#[cfg(unix)]
fn grandchild_gone(workspace: &Path) -> Result<(), String> {
    let pid_file = workspace.join("grandchild.pid");
    let started = Instant::now();
    while !pid_file.exists() && started.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(20));
    }
    let pid: libc::pid_t = std::fs::read_to_string(&pid_file)
        .map_err(|e| format!("script never wrote its pid: {e}"))?
        .trim()
        .parse()
        .map_err(|e| format!("bad pid: {e}"))?;
    // The killed sleep is an orphan until init reaps it.
    let deadline = Instant::now() + Duration::from_secs(2);
    while unsafe { libc::kill(pid, 0) } == 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    if unsafe { libc::kill(pid, 0) } == 0 {
        unsafe { libc::kill(pid, libc::SIGKILL) };
        return Err(format!("grandchild {pid} outlived the command"));
    }
    Ok(())
}

/// 输出多到把保留缓冲（1 MiB）挤满时，分页必须能走到头。
///
/// 原来走不到：`offset` 是保留缓冲里的位置，`next_offset` 却拿累计字节数
/// 判断"还有没有"。读到缓冲末尾之后，每次都回一个空页、`next_offset` 和
/// 传进去的 offset 一模一样——调用方照着它再读，就是死循环（审查 X01）。
#[cfg(unix)]
#[test]
fn paging_output_bigger_than_the_retained_buffer_terminates() {
    use std::os::unix::fs::PermissionsExt;
    let workspace = tempdir().expect("workspace");
    let harness = tempdir().expect("harness");
    let script = workspace.path().join("flood");
    // 3 MiB，是保留缓冲的三倍。
    std::fs::write(
        &script,
        "#!/bin/sh\nhead -c 3145728 /dev/zero | tr '\\000' a\n",
    )
    .expect("script");
    let mut permissions = std::fs::metadata(&script).expect("meta").permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&script, permissions).expect("chmod");
    let ctx = ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
        .expect("context");

    let output = call_tool(
        &ctx,
        "exec_command",
        &json!({ "cmd": "./flood", "timeout_ms": 30_000, "yield_time_ms": 30_000, "max_output_bytes": 1024 }),
    );
    assert_eq!(output["command_ok"], true, "{output}");
    let stdout_ref = output["output_refs"]["stdout"]
        .as_str()
        .expect("stdout ref")
        .to_string();

    let mut offset = 0u64;
    let mut pages = 0;
    let mut read_bytes = 0usize;
    loop {
        let page = call_tool(
            &ctx,
            "read_output",
            &json!({ "output_ref": stdout_ref, "offset": offset, "limit": 262_144 }),
        );
        assert_eq!(page["ok"], true, "{page}");
        read_bytes += page["content"].as_str().unwrap_or_default().len();
        pages += 1;
        assert!(pages < 64, "分页没走到头，第 {pages} 页还在原地：{page}");
        match page["next_offset"].as_u64() {
            Some(next) => {
                assert!(next > offset, "next_offset 没有前进：{page}");
                offset = next;
            }
            None => break,
        }
    }
    // 缓冲只留最后 1 MiB，所以读到的不会是全部 3 MiB——但必须读得到
    // 留下来的那一段，而且要能停。
    assert!(read_bytes >= 1_000_000, "只读到 {read_bytes} 字节");
}

/// 缓冲已经把开头挤掉了，还照着旧 offset 来读：不能假装那些字节还在。
#[cfg(unix)]
#[test]
fn an_offset_the_buffer_has_dropped_is_reported_as_a_gap() {
    use std::os::unix::fs::PermissionsExt;
    let workspace = tempdir().expect("workspace");
    let harness = tempdir().expect("harness");
    let script = workspace.path().join("flood");
    std::fs::write(
        &script,
        "#!/bin/sh\nhead -c 3145728 /dev/zero | tr '\\000' a\n",
    )
    .expect("script");
    let mut permissions = std::fs::metadata(&script).expect("meta").permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&script, permissions).expect("chmod");
    let ctx = ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
        .expect("context");
    let output = call_tool(
        &ctx,
        "exec_command",
        &json!({ "cmd": "./flood", "timeout_ms": 30_000, "yield_time_ms": 30_000, "max_output_bytes": 1024 }),
    );
    let stdout_ref = output["output_refs"]["stdout"]
        .as_str()
        .expect("stdout ref")
        .to_string();

    let page = call_tool(
        &ctx,
        "read_output",
        &json!({ "output_ref": stdout_ref, "offset": 0, "limit": 4096 }),
    );
    // 前 2 MiB 已经被挤掉了：说清楚从哪儿开始、丢了多少，而不是把
    // 缓冲里的第一个字节当成第 0 个字节。
    assert!(page["offset"].as_u64().unwrap_or(0) > 0, "{page}");
    assert!(page["dropped_bytes"].as_u64().unwrap_or(0) > 0, "{page}");
    assert!(
        page["warnings"]
            .as_array()
            .map(|w| w.iter().any(|item| item
                .as_str()
                .unwrap_or_default()
                .contains("no longer retained")))
            .unwrap_or(false),
        "{page}"
    );
}

/// A command that finishes before yield_time, with more output than
/// max_output_bytes: the result says "truncated" and hands out
/// output_refs, so those refs have to be readable.
#[cfg(unix)]
#[test]
fn output_refs_of_a_command_that_finished_inline_can_be_read() {
    use std::os::unix::fs::PermissionsExt;
    let workspace = tempdir().expect("workspace");
    let harness = tempdir().expect("harness");
    let script = workspace.path().join("noisy");
    std::fs::write(
        &script,
        "#!/bin/sh\nhead -c 5000 /dev/zero | tr '\\000' a\necho END\n",
    )
    .expect("script");
    let mut permissions = std::fs::metadata(&script).expect("meta").permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&script, permissions).expect("chmod");
    let ctx = ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
        .expect("context");

    let output = call_tool(
        &ctx,
        "exec_command",
        &json!({ "cmd": "./noisy", "timeout_ms": 10_000, "yield_time_ms": 10_000, "max_output_bytes": 64 }),
    );
    assert_eq!(output["command_ok"], true, "{output}");
    assert_eq!(output["stdout_truncated"], true, "{output}");
    let stdout_ref = output["output_refs"]["stdout"]
        .as_str()
        .expect("stdout ref");
    let read = call_tool(
        &ctx,
        "read_output",
        &json!({ "output_ref": stdout_ref, "limit": 10_000 }),
    );
    assert_eq!(read["total_stream_bytes"], 5004, "{read}");
    assert!(
        read["content"]
            .as_str()
            .unwrap_or_default()
            .ends_with("aEND\n"),
        "{read}"
    );
}

/// 同步等结果的命令占着工作区写权：这期间别人拿不到，也就打不了补丁。
///
/// 时序：yield 窗口开了 1.5 秒，400 毫秒的时候命令必然还在里面等，那时
/// 写权应该是拿不到的。
#[cfg(unix)]
#[test]
fn a_foreground_command_holds_the_write_lock_while_it_waits() {
    let (_workspace, _harness, ctx) = tree_workspace();
    let runtime = Arc::clone(&ctx.runtime);
    let ctx = Arc::new(ctx);
    let runner = {
        let ctx = Arc::clone(&ctx);
        std::thread::spawn(move || {
            call_tool(
                &ctx,
                "exec_command",
                &json!({ "cmd": "./spawn-tree", "timeout_ms": 3_000, "yield_time_ms": 1_500 }),
            )
        })
    };

    std::thread::sleep(Duration::from_millis(400));
    let taken = runtime.lock_commits_within(Duration::from_millis(200));
    let held_by_command = taken.is_none();
    drop(taken);
    let output = runner.join().expect("命令线程");
    assert!(
        held_by_command,
        "命令还在同步等，写权却被别人拿走了——那就是一边跑命令一边改文件：{output}"
    );
}

/// 后台命令**起来之前**也要拿到写权：起进程那一瞬间看到的文件树不能是
/// 别人写到一半的。
///
/// 这条以前断言的是反面（"写权被占着也照起"）。改掉是因为那条规则有个说不
/// 通的地方：`yield_time_ms: 1` 要等锁、`yield_time_ms: 0` 完全不等，同一条
/// 命令只因为参数差 1 就走了两套并发语义。而一个补丁改三个文件不是原子的，
/// 正好落在中间起来的命令看到的就是改了一个半的文件树。
///
/// 拿到之后立刻放：后台那段照旧没有互斥，见
/// [`a_background_command_does_not_keep_the_write_lock`]。
#[cfg(unix)]
#[test]
fn a_background_command_waits_for_the_write_lock_before_it_starts() {
    let (_workspace, _harness, ctx) = tree_workspace();
    let held = ctx
        .runtime
        .lock_commits_within(Duration::from_millis(50))
        .expect("没人占着却拿不到写权");

    let refused = call_tool(
        &ctx,
        "exec_command",
        &json!({ "cmd": "./spawn-tree", "timeout_ms": 1_000, "yield_time_ms": 0 }),
    );
    assert_eq!(
        refused["error"]["code"], "WORKSPACE_BUSY",
        "有人正在写，后台命令却起来了——它看到的可能是改了一半的文件树：{refused}"
    );

    drop(held);
    let started = call_tool(
        &ctx,
        "exec_command",
        &json!({ "cmd": "./spawn-tree", "timeout_ms": 1_000, "yield_time_ms": 0 }),
    );
    assert!(
        started.get("session_id").is_some(),
        "写权放开了还是起不来：{started}"
    );
}

/// 后台命令跑着的时候落了盘，命令那边看得见：快照里的
/// `workspace_writes_since_start` 从 0 变成 1。
///
/// 后台那段没有互斥，这个数就是事后判断"这条命令的结果还作不作数"的唯一
/// 依据——它编译的可能是改之前的代码。
#[cfg(unix)]
#[test]
fn a_running_command_sees_that_the_workspace_was_written() {
    let (_workspace, _harness, ctx) = tree_workspace();
    let started = call_tool(
        &ctx,
        "exec_command",
        &json!({ "cmd": "./spawn-tree", "timeout_ms": 5_000, "yield_time_ms": 0 }),
    );
    let session_id = started["session_id"].as_str().expect("session id");
    assert_eq!(
        started["workspace_writes_since_start"], 0,
        "还没人写就报写过了：{started}"
    );

    let patched = call_tool(
        &ctx,
        "apply_patch",
        &json!({ "patch": "*** Begin Patch\n*** Add File: while-running.txt\n+hello\n*** End Patch\n" }),
    );
    assert_eq!(patched["ok"], true, "{patched}");

    let after = call_tool(
        &ctx,
        "read_output",
        &json!({ "output_ref": format!("session:{session_id}:stdout") }),
    );
    assert_eq!(after["ok"], true, "{after}");
    let snapshot = call_tool(
        &ctx,
        "write_stdin",
        &json!({ "session_id": session_id, "chars": "", "yield_time_ms": 0 }),
    );
    assert_eq!(
        snapshot["workspace_writes_since_start"], 1,
        "命令跑着的时候工作区被改了，它却什么都不知道：{snapshot}"
    );
}

/// 系统这句话分不出原因，AI 只会重试。经别的程序（比如 python）调用时报错也是原样透出来的，
/// 所以看 stderr，不看命令名。
#[cfg(unix)]
#[test]
fn a_refused_screen_capture_points_at_the_diagnosis() {
    use std::os::unix::fs::PermissionsExt;
    let (workspace, _harness, ctx) = tree_workspace();
    let warnings_for = |stderr: &str| {
        let script = workspace.path().join("capture");
        std::fs::write(&script, format!("#!/bin/sh\necho '{stderr}' >&2\nexit 1\n"))
            .expect("script");
        let mut permissions = std::fs::metadata(&script).expect("meta").permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script, permissions).expect("chmod");
        let result = call_tool(
            &ctx,
            "exec_command",
            &json!({ "cmd": "./capture", "timeout_ms": 5_000 }),
        );
        assert_eq!(result["exit_code"], 1, "{result}");
        result["warnings"].to_string()
    };

    assert!(warnings_for("could not create image from display").contains("desktop authorize"));
    assert!(warnings_for("could not create image from rect").contains("desktop authorize"));
    assert!(!warnings_for("some other failure").contains("desktop authorize"));
}

/// 反过来：命令还在跑的时候打补丁，补丁的结果里得说一声。
///
/// 不是拦下来——后台命令没有终点，拦住等于"有 dev server 就不能改代码"。
/// 提示要做到的是让模型能判断：自己起的命令直接给 `session_id`，停掉重跑
/// 还是认下"那条结果基于旧代码"，由它决定。
#[cfg(unix)]
#[test]
fn a_patch_says_which_of_my_commands_are_still_running() {
    let (_workspace, _harness, ctx) = tree_workspace();
    let quiet = call_tool(
        &ctx,
        "apply_patch",
        &json!({ "patch": "*** Begin Patch\n*** Add File: first.txt\n+hello\n*** End Patch\n" }),
    );
    assert_eq!(
        quiet["warnings"],
        json!([]),
        "没有命令在跑却警告了：{quiet}"
    );

    let started = call_tool(
        &ctx,
        "exec_command",
        &json!({ "cmd": "./spawn-tree", "timeout_ms": 5_000, "yield_time_ms": 0 }),
    );
    let session_id = started["session_id"].as_str().expect("session id");

    let noisy = call_tool(
        &ctx,
        "apply_patch",
        &json!({ "patch": "*** Begin Patch\n*** Add File: second.txt\n+hello\n*** End Patch\n" }),
    );
    assert_eq!(noisy["ok"], true, "{noisy}");
    let warnings = noisy["warnings"].to_string();
    assert!(
        warnings.contains(session_id),
        "警告里没给 session_id，模型想停都不知道停哪条：{warnings}"
    );
    assert!(
        warnings.contains("spawn-tree"),
        "警告里没说是哪条命令：{warnings}"
    );
}

/// 已经跑完的命令不算"还在跑"。
///
/// 这条钉的是那次主动 refresh：命令自己结束时没人通知服务端，
/// `has_exited` 要等有人来读或者超时监视器到点才翻。不主动问一遍的话，
/// 一条早就结束的命令会一直被算进警告里，那条提示就成了狼来了——每次
/// 打补丁都报，模型很快就不看了。
#[cfg(unix)]
#[test]
fn a_command_that_already_finished_is_not_reported_as_running() {
    let (workspace, _harness, ctx) = tree_workspace();
    let quick = workspace.path().join("quick");
    std::fs::write(&quick, "#!/bin/sh\nexit 0\n").expect("script");
    let mut permissions = std::fs::metadata(&quick).expect("meta").permissions();
    {
        use std::os::unix::fs::PermissionsExt;
        permissions.set_mode(0o755);
    }
    std::fs::set_permissions(&quick, permissions).expect("chmod");

    // yield_time_ms: 0 = 起完就走，谁也没去读过它的状态。
    let started = call_tool(
        &ctx,
        "exec_command",
        &json!({ "cmd": "./quick", "timeout_ms": 5_000, "yield_time_ms": 0 }),
    );
    assert!(started.get("session_id").is_some(), "{started}");
    // 给它一点时间退出。退出这件事本身不会有人来记账。
    std::thread::sleep(Duration::from_millis(300));

    let patched = call_tool(
        &ctx,
        "apply_patch",
        &json!({ "patch": "*** Begin Patch\n*** Add File: after-exit.txt\n+hello\n*** End Patch\n" }),
    );
    assert_eq!(
        patched["warnings"],
        json!([]),
        "命令早就退了还被算成在跑：{patched}"
    );
}

/// 起完就放：`npm run dev` 转后台之后，别人照样改得了代码。
///
/// 这是刻意的取舍——后台命令没有终点，一直占着等于把目录锁到天亮。代价
/// （后台那段没有互斥）由两边的可见性来兜：命令这边看
/// `workspace_writes_since_start`，写的那边看 `apply_patch` 的 warnings。
#[cfg(unix)]
#[test]
fn a_background_command_does_not_keep_the_write_lock() {
    let (_workspace, _harness, ctx) = tree_workspace();
    let started = call_tool(
        &ctx,
        "exec_command",
        &json!({ "cmd": "./spawn-tree", "timeout_ms": 3_000, "yield_time_ms": 0 }),
    );
    assert!(started.get("session_id").is_some(), "{started}");

    let taken = ctx.runtime.lock_commits_within(Duration::from_millis(200));
    assert!(
        taken.is_some(),
        "命令转后台了还攥着写权，那有个 dev server 在跑就谁也改不了代码"
    );
}

#[cfg(unix)]
#[test]
fn a_timed_out_command_takes_its_background_children_with_it() {
    let (workspace, _harness, ctx) = tree_workspace();
    let output = call_tool(
        &ctx,
        "exec_command",
        &json!({ "cmd": "./spawn-tree", "timeout_ms": 500, "yield_time_ms": 10_000 }),
    );
    assert_eq!(output["termination_reason"], "timeout", "{output}");
    grandchild_gone(workspace.path()).unwrap();
}

#[cfg(unix)]
#[test]
fn kill_session_takes_the_background_children_with_it() {
    let (workspace, _harness, ctx) = tree_workspace();
    let started = call_tool(
        &ctx,
        "exec_command",
        &json!({ "cmd": "./spawn-tree", "timeout_ms": 60_000, "yield_time_ms": 300 }),
    );
    let session_id = started["session_id"]
        .as_str()
        .unwrap_or_else(|| panic!("no session: {started}"))
        .to_string();
    let killed = call_tool(
        &ctx,
        "kill_session",
        &json!({ "session_id": session_id, "wait_ms": 2000 }),
    );
    assert_eq!(killed["killed"], true, "{killed}");
    grandchild_gone(workspace.path()).unwrap();
}

#[cfg(windows)]
#[test]
fn windows_workspace_scripts_and_python_unicode_execute_successfully() {
    let workspace = tempdir().expect("workspace");
    let harness = tempdir().expect("harness");
    std::fs::write(
        workspace.path().join("any-name.cmd"),
        "@echo tooling-cmd-ok\r\n",
    )
    .expect("cmd script");
    std::fs::write(
        workspace.path().join("any-name.ps1"),
        "Write-Output 'tooling-powershell-ok'\r\n",
    )
    .expect("powershell script");
    std::fs::write(
        workspace.path().join("workflow_probe.py"),
        "print('workflow-ok')\n",
    )
    .expect("python module");
    let ctx = ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
        .expect("context");

    for command in [
        "any-name.cmd",
        "any-name.ps1",
        "cmd /c echo tooling-cmd-ok",
        "powershell -NoProfile -Command \"Write-Output tooling-powershell-ok\"",
        "python -c \"print('中文输出正常 ✅')\"",
    ] {
        let output = call_tool(
            &ctx,
            "exec_command",
            &json!({ "cmd": command, "timeout_ms": 10_000, "yield_time_ms": 10_000 }),
        );
        assert_eq!(output["ok"], true, "{command}: {output}");
        assert_eq!(output["command_ok"], true, "{command}: {output}");
    }

    for _ in 0..10 {
        let output = call_tool(
            &ctx,
            "exec_command",
            &json!({ "cmd": "python -m workflow_probe", "timeout_ms": 10_000 }),
        );
        assert_eq!(output["command_ok"], true, "{output}");
        assert!(output["stdout"]
            .as_str()
            .unwrap_or_default()
            .contains("workflow-ok"));
    }
}

#[cfg(windows)]
#[test]
fn windows_batch_scripts_preserve_space_paths_and_arguments() {
    let parent = tempdir().expect("workspace parent");
    let workspace = parent.path().join("Life Brain 中文");
    std::fs::create_dir_all(&workspace).expect("workspace");
    let harness = tempdir().expect("harness");
    let ctx =
        ToolContext::for_test(workspace.clone(), harness.path().to_path_buf()).expect("context");

    for extension in ["cmd", "bat"] {
        let script_name = format!("run & tooling.{extension}");
        std::fs::write(
            workspace.join(&script_name),
            "@echo off\r\nif not \"%~1\"==\"argument & value\" exit /b 7\r\necho tooling-space-path-ok\r\n",
        )
        .expect("batch script");

        let command = format!(r#""{script_name}" "argument & value""#);
        let output = call_tool(
            &ctx,
            "exec_command",
            &json!({ "cmd": command, "timeout_ms": 10_000, "yield_time_ms": 10_000 }),
        );
        assert_eq!(output["command_ok"], true, "{script_name}: {output}");
        let stdout = output["stdout"].as_str().unwrap_or_default();
        assert!(
            stdout.contains("tooling-space-path-ok"),
            "{script_name}: {output}"
        );
    }
}

#[cfg(unix)]
#[test]
fn unix_workspace_scripts_preserve_space_paths_and_arguments() {
    use std::os::unix::fs::PermissionsExt;

    let parent = tempdir().expect("workspace parent");
    let workspace = parent.path().join("Life Brain 中文");
    std::fs::create_dir_all(&workspace).expect("workspace");
    let harness = tempdir().expect("harness");
    let script_name = "run tooling";
    let script_path = workspace.join(script_name);
    std::fs::write(
        &script_path,
        "#!/bin/sh\nprintf 'tooling-space-path-ok\\n'\nprintf 'argument=[%s]\\n' \"$1\"\n",
    )
    .expect("shell script");
    let mut permissions = std::fs::metadata(&script_path)
        .expect("script metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&script_path, permissions).expect("script executable");

    let ctx = ToolContext::for_test(workspace, harness.path().to_path_buf()).expect("context");
    let command = format!(r#""{script_name}" "argument with spaces""#);
    let output = call_tool(
        &ctx,
        "exec_command",
        &json!({ "cmd": command, "timeout_ms": 10_000, "yield_time_ms": 10_000 }),
    );
    assert_eq!(output["command_ok"], true, "{output}");
    let stdout = output["stdout"].as_str().unwrap_or_default();
    assert!(stdout.contains("tooling-space-path-ok"), "{output}");
    assert!(
        stdout.contains("argument=[argument with spaces]"),
        "{output}"
    );
}

/// 默认扩展名名单里没有 .sh；Unix 上看可执行位，不看名单。没可执行位的照旧拒，
/// 而且报错要说出怎么办，不能只有一句"未获允许"。
#[cfg(unix)]
#[test]
fn unix_executable_sh_scripts_run_and_non_executable_ones_say_why() {
    use std::os::unix::fs::PermissionsExt;

    let workspace = tempdir().expect("workspace");
    let harness = tempdir().expect("harness");
    let scripts = workspace.path().join("scripts");
    std::fs::create_dir_all(&scripts).expect("scripts");
    for (name, mode) in [("gen.sh", 0o755), ("plain.sh", 0o644)] {
        let path = scripts.join(name);
        std::fs::write(&path, "#!/bin/sh\necho sh-script-ran\n").expect("script");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).expect("mode");
    }
    assert!(!crate::tools::policy::PolicySettings::default()
        .workspace_script_extensions
        .contains(".sh"));

    let ctx = ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
        .expect("context");
    let ran = call_tool(
        &ctx,
        "exec_command",
        &json!({ "cmd": "scripts/gen.sh", "timeout_ms": 10_000, "yield_time_ms": 10_000 }),
    );
    assert_eq!(ran["command_ok"], true, "{ran}");
    assert!(
        ran["stdout"]
            .as_str()
            .unwrap_or_default()
            .contains("sh-script-ran"),
        "{ran}"
    );

    let refused = call_tool(
        &ctx,
        "exec_command",
        &json!({ "cmd": "scripts/plain.sh", "timeout_ms": 10_000, "yield_time_ms": 10_000 }),
    );
    assert_eq!(refused["status"], "spawn_failed", "{refused}");
    let message = refused["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("可执行权限") && message.contains("chmod +x"),
        "{refused}"
    );
}
