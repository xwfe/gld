use std::fs;

use gld_core::harness::{Harness, TaskStatus};
use serde_json::json;

fn fixture() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    let harness_root = temp.path().join("harness");
    fs::create_dir_all(&workspace).expect("创建工作区");
    fs::write(workspace.join("README.md"), "初始内容\n").expect("写入夹具");
    (temp, workspace, harness_root)
}

#[test]
fn 任务创建会捕获基线并在重启后恢复() {
    let (_temp, workspace, harness_root) = fixture();
    let harness = Harness::new(workspace.clone(), harness_root.clone()).expect("创建 Harness");

    let task = harness
        .start_task("实现 Harness 基础能力")
        .expect("启动任务");

    assert_eq!(task.status, TaskStatus::Active);
    assert_eq!(task.objective, "实现 Harness 基础能力");
    assert!(!task.baseline.worktree_fingerprint.is_empty());
    assert_eq!(
        harness
            .current_task()
            .expect("读取任务")
            .expect("活动任务")
            .id,
        task.id
    );

    let restarted = Harness::new(workspace, harness_root).expect("重启 Harness");
    assert_eq!(
        restarted
            .current_task()
            .expect("恢复任务")
            .expect("活动任务")
            .id,
        task.id
    );
}

#[test]
fn 同一工作区只允许一个可写任务且拒绝非法迁移() {
    let (_temp, workspace, harness_root) = fixture();
    let harness = Harness::new(workspace, harness_root).expect("创建 Harness");
    let task = harness.start_task("第一个任务").expect("启动任务");

    let duplicate = harness
        .start_task("第二个任务")
        .expect_err("应拒绝第二个任务");
    assert_eq!(duplicate.code(), "TASK_ALREADY_ACTIVE");

    let invalid = harness
        .transition(&task.id, TaskStatus::Completed)
        .expect_err("active 不应直接完成");
    assert_eq!(invalid.code(), "INVALID_TASK_TRANSITION");

    let paused = harness
        .transition(&task.id, TaskStatus::Paused)
        .expect("暂停任务");
    assert_eq!(paused.status, TaskStatus::Paused);
    let resumed = harness
        .transition(&task.id, TaskStatus::Active)
        .expect("恢复任务");
    assert_eq!(resumed.status, TaskStatus::Active);
}

#[test]
fn 外部文件变化会被识别且操作会留下事件() {
    let (_temp, workspace, harness_root) = fixture();
    let harness = Harness::new(workspace.clone(), harness_root).expect("创建 Harness");
    let task = harness.start_task("验证外部变更").expect("启动任务");

    fs::write(workspace.join("README.md"), "外部修改\n").expect("模拟外部修改");
    let stale = harness
        .check_baseline(&task.id)
        .expect_err("应识别外部修改");
    assert_eq!(stale.code(), "FILE_CHANGED_EXTERNALLY");

    harness
        .record_event(
            &task.id,
            "operation_finished",
            Some("read_file"),
            json!({"reason": "确认外部变更"}),
            json!({"ok": true}),
        )
        .expect("记录事件");
    let events = harness.list_events(&task.id, 0, 10).expect("读取事件");
    assert!(events.len() >= 2);
    assert!(events
        .iter()
        .any(|event| event.tool_name.as_deref() == Some("read_file")));
}

#[test]
fn project_state包含分支任务和脏状态摘要() {
    let (_temp, workspace, harness_root) = fixture();
    let harness = Harness::new(workspace, harness_root).expect("创建 Harness");
    let task = harness.start_task("生成项目状态").expect("启动任务");

    let state = harness.project_state(20).expect("读取项目状态");

    assert_eq!(state.active_task_id.as_deref(), Some(task.id.as_str()));
    assert!(!state.files.is_empty());
    assert!(state.task.is_some());
}

/// gld 自己在项目里的状态目录不算工作区变化。
///
/// `.gld/planning/state.json` 由 dispatch 的公共路径 load-or-create——每一次
/// 工具调用都可能写它。算进指纹的话就是自己锁死自己：`task_manage start` 记下
/// 基线的同时创建了这个文件，紧接着第一次 `exec_command` 就被判成
/// FILE_CHANGED_EXTERNALLY。开了任务反而什么都干不了，而报错让人去查一个
/// 根本不存在的"外部修改"。
///
/// `.coding-tools` 是它改名前的叫法，老项目里还留着，一并跳过。
#[test]
fn gld自己的状态目录不计入基线() {
    let (_temp, workspace, harness_root) = fixture();
    let harness = Harness::new(workspace.clone(), harness_root).expect("创建 Harness");
    let task = harness
        .start_task("验证状态目录不干扰基线")
        .expect("启动任务");

    for relative in [
        ".gld/planning/state.json",
        ".coding-tools/planning/state.json",
    ] {
        let path = workspace.join(relative);
        fs::create_dir_all(path.parent().expect("父目录")).expect("建目录");
        fs::write(&path, r#"{"mode":"direct"}"#).expect("写状态文件");

        harness
            .check_baseline(&task.id)
            .unwrap_or_else(|error| panic!("{relative} 被当成了外部修改：{}", error.code()));
    }

    // 反向：真的改了项目文件仍然要报出来，别把检测能力一起关掉。
    fs::write(workspace.join("README.md"), "外部改的\n").expect("模拟外部修改");
    assert_eq!(
        harness
            .check_baseline(&task.id)
            .expect_err("外部修改必须仍被识别")
            .code(),
        "FILE_CHANGED_EXTERNALLY"
    );
}
