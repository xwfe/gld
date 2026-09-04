//! `gld planning` 的完整走位，外加一条格式底线：不带 `--json` 时不能甩 JSON。
//!
//! Goal / Plan 是给人看的东西——建目标、拆步骤、推进、验收，每一步的结果
//! 都要人扫一眼就知道现在到哪儿了。之前这几个命令不带 `--json` 也直接打印
//! serde 的 pretty JSON，一屏 id 和 null，得自己在里面找 status。

mod common;

use common::env::Env;

/// 按 JSON Pointer 取字符串字段。
///
/// 注意别用 `value["plans"]["0"]`——serde_json 的字符串索引只认对象，
/// 数组用字符串下标会静默给 null，看着像程序没写进去。
fn field(value: &serde_json::Value, pointer: &str) -> String {
    value
        .pointer(pointer)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string()
}

#[test]
fn planning_lifecycle_reads_like_something_a_person_wrote() {
    let env = Env::new();
    env.ok(&["ws", "add", ".", "--name", "plan"]);

    // 1. 建 Goal。
    let created = env.ok(&[
        "planning",
        "goal",
        "create",
        "--title",
        "打通接入链路",
        "--objective",
        "让 ChatGPT 能连上",
        "--criterion",
        "本地能连",
        "--criterion",
        "公网能连",
    ]);
    assert!(
        !created.trim_start().starts_with('{'),
        "不带 --json 不该直接打印 JSON：{created}"
    );
    assert!(created.contains("打通接入链路"), "{created}");
    assert!(created.contains("本地能连"), "验收项要列出来：{created}");

    let state = env.json(&["--json", "planning", "show"]);
    let goal_id = field(&state, "/goals/0/id");
    assert!(!goal_id.is_empty(), "goal 没建出来：{state}");

    // 2. 建 Plan 挂到 Goal 上，步骤要显示成清单。
    let planned = env.ok(&[
        "planning",
        "plan",
        "create",
        "--title",
        "第一版",
        "--objective",
        "先本地跑通",
        "--goal",
        &goal_id,
        "--step",
        "起服务",
        "--step",
        "接客户端",
    ]);
    assert!(!planned.trim_start().starts_with('{'), "{planned}");
    assert!(
        planned.contains("起服务") && planned.contains("接客户端"),
        "{planned}"
    );

    let state = env.json(&["--json", "planning", "show"]);
    let plan_id = field(&state, "/plans/0/id");
    let step_id = field(&state, "/plans/0/steps/0/id");

    // 3. 推进一步：状态和备注都要落到位。
    env.ok(&[
        "planning",
        "plan",
        "update",
        &plan_id,
        "--step",
        &format!("{step_id}=completed:起好了"),
    ]);
    let state = env.json(&["--json", "planning", "show"]);
    assert_eq!(field(&state, "/plans/0/steps/0/status"), "completed");
    assert_eq!(field(&state, "/plans/0/steps/0/notes"), "起好了");

    // 4. 总览：一眼能看到 Goal、Plan 和步骤，而不是一坨 JSON。
    let shown = env.ok(&["planning", "show"]);
    assert!(!shown.trim_start().starts_with('{'), "{shown}");
    for expected in ["模式", "打通接入链路", "第一版", "起服务"] {
        assert!(
            shown.contains(expected),
            "总览里少了「{expected}」：{shown}"
        );
    }

    // 5. 验收：提交 → 通过 → 归档。
    env.ok(&[
        "planning",
        "plan",
        "update",
        &plan_id,
        "--status",
        "awaiting_acceptance",
    ]);
    let accepted = env.ok(&["planning", "plan", "accept", &plan_id]);
    assert!(accepted.contains("archived"), "验收后应当归档：{accepted}");

    // 6. 驳回要把意见带回来，Goal 回到 active。
    env.ok(&[
        "planning",
        "goal",
        "update",
        &goal_id,
        "--status",
        "awaiting_acceptance",
    ]);
    let rejected = env.ok(&[
        "planning",
        "goal",
        "reject",
        &goal_id,
        "--feedback",
        "公网还没验",
    ]);
    assert!(
        rejected.contains("公网还没验"),
        "驳回意见要显示出来：{rejected}"
    );
    let state = env.json(&["--json", "planning", "show"]);
    assert_eq!(
        field(&state, "/goals/0/status"),
        "active",
        "驳回后 Goal 应当回到 active：{state}"
    );

    // --json 还是干净的 JSON，脚本照用不误。
    let raw = env.ok(&["--json", "planning", "show"]);
    assert!(
        raw.trim_start().starts_with('{'),
        "--json 要给纯 JSON：{raw}"
    );
}
