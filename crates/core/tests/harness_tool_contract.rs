use std::fs;

use gld_core::tools::{call_tool, ToolContext};
use serde_json::json;

#[test]
fn 无任务时仍可执行_dry_run_预检() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    fs::write(workspace.join("README.md"), "初始内容\n").expect("写入文件");
    let ctx = ToolContext::for_test(workspace, temp.path().join("harness")).expect("创建上下文");

    let result = call_tool(
        &ctx,
        "apply_patch",
        &json!({
            "dry_run": true,
            "patch": "--- a/README.md\n+++ b/README.md\n@@\n-初始内容\n+预检内容\n"
        }),
    );

    assert_eq!(result["ok"], true);
    assert_eq!(result["preflight"], true);
    assert_eq!(result["harness_mode"], "standalone");
    assert_eq!(
        fs::read_to_string(temp.path().join("workspace/README.md")).unwrap(),
        "初始内容\n"
    );
}

#[test]
fn codex_patch格式支持新增文件dry_run和实际应用() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    let ctx =
        ToolContext::for_test(workspace.clone(), temp.path().join("harness")).expect("创建上下文");
    let patch = "*** Begin Patch\n*** Add File: probe.txt\n+probe-v2\n*** End Patch\n";

    let dry_run = call_tool(
        &ctx,
        "apply_patch",
        &json!({"patch": patch, "dry_run": true}),
    );
    assert_eq!(dry_run["ok"], true);
    assert_eq!(dry_run["dry_run"], true);
    assert!(dry_run["affected_files"]
        .as_array()
        .expect("影响文件")
        .iter()
        .any(|file| file["path"] == "probe.txt" && file["operation"] == "add"));
    assert!(!workspace.join("probe.txt").exists());

    let applied = call_tool(&ctx, "apply_patch", &json!({"patch": patch}));
    assert_eq!(applied["ok"], true);
    assert_eq!(
        fs::read_to_string(workspace.join("probe.txt")).expect("读取新增文件"),
        "probe-v2\n"
    );
}

#[test]
fn 无任务时普通_patch也可执行并保留撤销能力() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    fs::write(workspace.join("README.md"), "初始内容\n").expect("写入文件");
    let ctx =
        ToolContext::for_test(workspace.clone(), temp.path().join("harness")).expect("创建上下文");

    let result = call_tool(
        &ctx,
        "apply_patch",
        &json!({
            "patch": "--- a/README.md\n+++ b/README.md\n@@\n-初始内容\n+已修改\n"
        }),
    );

    assert_eq!(result["ok"], true);
    assert_eq!(result["harness_mode"], "standalone");
    assert!(!result
        .as_object()
        .unwrap()
        .contains_key("pre_change_snapshot_id"));
    assert_eq!(
        fs::read_to_string(workspace.join("README.md")).unwrap(),
        "已修改\n"
    );

    let log = call_tool(&ctx, "operation_log", &json!({}));
    assert_eq!(log["ok"], true);
    assert!(log["operations"]
        .as_array()
        .expect("操作日志")
        .iter()
        .any(|operation| operation["tool"] == "apply_patch"));
}

#[test]
fn 无任务时_exec_command不返回任务门禁错误() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    let ctx = ToolContext::for_test(workspace, temp.path().join("harness")).expect("创建上下文");

    let result = call_tool(
        &ctx,
        "exec_command",
        &json!({"cmd": "git status", "filesystem_scope": "workspace"}),
    );

    assert_ne!(result["error"]["code"], "TASK_STATE_REQUIRED");
    assert_eq!(result["harness_mode"], "standalone");
    assert_eq!(result["execution_mode"], "direct");
    assert_eq!(result["task_required"], false);
    assert_eq!(result["command"], "git status");
    assert_eq!(result["status"], "exited");
    assert!(result["exit_code"].is_i64() || result["exit_code"].is_u64());
    assert!(result["duration_ms"].is_u64());
    assert_eq!(result["duration_ms"], result["elapsed_ms"]);
    assert_eq!(result["next_actions"], json!([]));
    assert!(result["recovery_hint"].is_string());
    assert!(!result
        .as_object()
        .unwrap()
        .contains_key("pre_change_snapshot_id"));
}

#[test]
fn 无任务时_exec错误不应建议启动任务() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    let ctx = ToolContext::for_test(workspace, temp.path().join("harness")).expect("创建上下文");

    let result = call_tool(
        &ctx,
        "exec_command",
        &json!({"cmd": "python -c \"import sys; sys.exit(1)\""}),
    );

    assert_eq!(result["harness_mode"], "standalone");
    assert_eq!(result["task_required"], false);
    assert_eq!(result["next_actions"], json!([]));
    assert!(result["recovery_hint"].is_string());
    if let Some(actions) = result["harness"]["next_actions"].as_array() {
        assert!(!actions.iter().any(|action| action == "start_task"));
    }
}

#[test]
fn workspace_allows_exec_during_transition() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    let ctx = ToolContext::for_test(workspace, temp.path().join("harness")).expect("创建上下文");

    let result = call_tool(&ctx, "exec_command", &json!({"cmd": "python --version"}));

    assert_ne!(result["error"]["code"], "EXEC_SANDBOX_UNAVAILABLE");
    assert_eq!(result["execution_mode"], "direct");
    assert_eq!(result["filesystem_scope"], "workspace");
    assert_eq!(result["sandbox_enforced"], false);
}

#[test]
fn harness_tools_support_task_lifecycle() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    fs::write(workspace.join("README.md"), "初始内容\n").expect("写入文件");
    let ctx = ToolContext::for_test(workspace, temp.path().join("harness")).expect("创建上下文");

    let started = call_tool(
        &ctx,
        "start_task",
        &json!({"objective": "补齐 Harness 状态"}),
    );
    assert_eq!(started["ok"], true);
    let task_id = started["task"]["id"].as_str().expect("任务 ID");

    let updated = call_tool(
        &ctx,
        "update_task",
        &json!({"task_id": task_id, "pending_steps": ["接入门禁"]}),
    );
    assert_eq!(updated["ok"], true);
    let context = call_tool(&ctx, "task_context", &json!({}));
    assert_eq!(context["ok"], true);
    assert_eq!(context["task"]["id"], task_id);
    assert!(!context["events"].as_array().expect("事件").is_empty());

    let finished = call_tool(
        &ctx,
        "finish_task",
        &json!({"task_id": task_id, "allow_unverified": true}),
    );
    assert_eq!(finished["ok"], true);
    assert_eq!(finished["task"]["status"], "completed_unverified");
}

#[test]
fn 外部修改会在写工具执行前被拒绝() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    fs::write(workspace.join("README.md"), "初始内容\n").expect("写入文件");
    let ctx =
        ToolContext::for_test(workspace.clone(), temp.path().join("harness")).expect("创建上下文");
    let started = call_tool(&ctx, "start_task", &json!({"objective": "检查外部变化"}));
    let task_id = started["task"]["id"].as_str().expect("任务 ID");
    fs::write(workspace.join("README.md"), "外部修改\n").expect("模拟外部修改");

    let result = call_tool(
        &ctx,
        "exec_command",
        &json!({"cmd": "git status", "filesystem_scope": "workspace"}),
    );

    assert_eq!(result["ok"], false);
    assert_eq!(result["error"]["code"], "FILE_CHANGED_EXTERNALLY");
    assert_eq!(
        ctx.harness
            .current_task()
            .expect("读取任务")
            .expect("活动任务")
            .id,
        task_id
    );
}

#[test]
fn 工具清单包含项目状态和任务上下文能力() {
    let tools = gld_core::tools::list_tools_for_profile("advanced");
    let names = tools
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect::<Vec<_>>();
    for expected in [
        "project_state",
        "start_task",
        "task_context",
        "list_task_events",
        "change_summary",
    ] {
        assert!(names.contains(&expected), "缺少工具 {expected}");
    }
    assert!(!names.contains(&"undo_last_patch"));
}

/// 开了任务之后还能干活——这条路曾经整个是死的。
///
/// 三处都会往工作区写东西，而它们都不是"外部修改"：
///
/// 1. 每次工具调用都会 load-or-create `.gld/planning/state.json`，
///    连 `task_manage start` 自己也会；
/// 2. planning 工具改这个状态文件；
/// 3. history 工具往 `docs/history-session/` 写档案。
///
/// 以前这三样都会让下一次 exec_command / apply_patch 报 FILE_CHANGED_EXTERNALLY，
/// 也就是"一开任务就写不了东西"，而报错指向一个根本查不到的外部改动。
#[test]
fn 开了任务之后工具自己的写入不会把自己锁死() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    fs::write(workspace.join("README.md"), "初始内容\n").expect("写入文件");
    let ctx =
        ToolContext::for_test(workspace.clone(), temp.path().join("harness")).expect("创建上下文");

    let started = call_tool(
        &ctx,
        "task_manage",
        &json!({"action": "start", "objective": "干活"}),
    );
    assert_eq!(started["ok"], true, "{started}");

    // ① 开完任务，第一次写操作就得能过。
    let first = call_tool(&ctx, "exec_command", &json!({"cmd": "echo hi"}));
    assert_eq!(first["ok"], true, "开了任务却直接写不了：{first}");

    // ② planning 改自己的状态文件之后，照样能写。
    let planned = call_tool(
        &ctx,
        "planning_manage",
        &json!({"action": "create_goal", "title": "目标", "objective": "试"}),
    );
    assert_eq!(planned["ok"], true, "{planned}");
    let after_planning = call_tool(&ctx, "exec_command", &json!({"cmd": "echo hi"}));
    assert_eq!(
        after_planning["ok"], true,
        "planning 之后被锁住了：{after_planning}"
    );

    // ③ history 往项目里写档案之后，照样能写。
    let archived = call_tool(
        &ctx,
        "history_session_bootstrap",
        &json!({"initial_user_input": "测试"}),
    );
    assert_eq!(archived["ok"], true, "{archived}");
    let after_history = call_tool(&ctx, "exec_command", &json!({"cmd": "echo hi"}));
    assert_eq!(
        after_history["ok"], true,
        "history 之后被锁住了：{after_history}"
    );

    // 反向：真正的外部修改仍然要拦住，别把检测能力一起丢了。
    fs::write(workspace.join("README.md"), "别人改的\n").expect("模拟外部修改");
    let blocked = call_tool(&ctx, "exec_command", &json!({"cmd": "echo hi"}));
    assert_eq!(blocked["ok"], false, "外部修改必须仍被拦住：{blocked}");
    assert_eq!(
        blocked["error"]["code"], "FILE_CHANGED_EXTERNALLY",
        "{blocked}"
    );
}

fn reply_bytes(value: &serde_json::Value) -> usize {
    serde_json::to_vec(value).expect("序列化").len()
}

/// task_context 的 schema 声明了 max_bytes，以前代码不读：`task` 里带着
/// 基线的逐文件清单（每个文件一条 sha256），2108 个文件的项目光这一项就约
/// 450 KB，远超 schema 的上限 131072。
#[test]
fn task_context_在文件很多的工作区也不超过_max_bytes() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    for i in 0..1500 {
        let dir = workspace.join(format!("src/module_{i:04}"));
        fs::create_dir_all(&dir).expect("创建目录");
        fs::write(
            dir.join("a_reasonably_descriptive_file_name.rs"),
            "fn f() {}\n",
        )
        .expect("写入文件");
    }
    let ctx = ToolContext::for_test(workspace, temp.path().join("harness")).expect("创建上下文");
    let started = call_tool(&ctx, "start_task", &json!({"objective": "大仓库"}));
    assert_eq!(started["ok"], true, "{started}");

    let context = call_tool(&ctx, "task_context", &json!({}));
    assert_eq!(context["ok"], true, "{context}");
    assert!(
        reply_bytes(&context) <= 32_768,
        "默认 max_bytes 是 32768，实际 {} 字节",
        reply_bytes(&context)
    );
    let baseline = &context["task"]["baseline"];
    assert!(
        baseline.get("entries").is_none(),
        "逐文件清单不该出现在上下文里"
    );
    assert_eq!(baseline["entry_count"], 1500);
    assert!(!baseline["worktree_fingerprint"]
        .as_str()
        .unwrap()
        .is_empty());
}

/// 以前固定取前 100 条事件、`truncated` 恒为 false：超过 100 条时后面的
/// 静默丢掉，单条事件再大也照单全收。现在按字节装，装不下就明说，并给出
/// 从哪里接着用 list_task_events 读。
#[test]
fn task_context_按字节装事件并说清从哪接着读() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    fs::write(workspace.join("README.md"), "初始内容\n").expect("写入文件");
    let ctx = ToolContext::for_test(workspace, temp.path().join("harness")).expect("创建上下文");
    let started = call_tool(&ctx, "start_task", &json!({"objective": "很多步"}));
    let task_id = started["task"]["id"].as_str().expect("任务 ID").to_string();
    for i in 0..150 {
        let updated = call_tool(
            &ctx,
            "update_task",
            &json!({"task_id": task_id, "pending_steps": [format!("第 {i} 步：{}", "细节".repeat(60))]}),
        );
        assert_eq!(updated["ok"], true, "{updated}");
    }
    let mut all = Vec::new();
    loop {
        let page = call_tool(
            &ctx,
            "list_task_events",
            &json!({"task_id": task_id, "cursor": all.len(), "limit": 200}),
        );
        let events = page["events"].as_array().expect("事件").clone();
        if events.is_empty() {
            break;
        }
        all.extend(events);
    }
    assert!(
        all.len() > 100,
        "要超过旧的 100 条上限才测得到，实际 {}",
        all.len()
    );

    // 小预算：装不下，要说 truncated，并且 next_cursor 正好接上。
    let small = call_tool(&ctx, "task_context", &json!({"max_bytes": 8192}));
    assert_eq!(small["ok"], true, "{small}");
    assert!(
        reply_bytes(&small) <= 8192,
        "实际 {} 字节",
        reply_bytes(&small)
    );
    let shown = small["events"].as_array().expect("事件");
    assert!(!shown.is_empty());
    assert_eq!(small["truncated"], true);
    assert_eq!(small["next_cursor"], shown.len());
    assert_eq!(shown.as_slice(), &all[..shown.len()], "从头按顺序装");

    // 大预算：全部装得下（超过 100 条），不截断。
    let large = call_tool(&ctx, "task_context", &json!({"max_bytes": 131072}));
    assert!(
        reply_bytes(&large) <= 131_072,
        "实际 {} 字节",
        reply_bytes(&large)
    );
    assert_eq!(large["events"].as_array().unwrap().len(), all.len());
    assert_eq!(large["truncated"], false);
    assert_eq!(large["next_cursor"], all.len());
}
