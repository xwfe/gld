//! A20：把一次真实的仓库维护从头跑到尾，用的就是模型会用的那几个工具。
//!
//! 单条用例故意写得很长：**接缝才是这条测试要验的东西**。前一步返回什么、
//! 下一步能不能原样拿去用，拆成十条互不相干的小测试反而全看不到。流程是
//!
//! ```text
//! 读仓库 → 查隐藏的 workflow → 改 README 和 workflow → 构建 → 读长日志 → 看 CI 状态
//! ```
//!
//! **证据分三类，别混着看**：
//!
//! | 类别 | 这里有没有 | 说明 |
//! | --- | --- | --- |
//! | 本地 | 有 | 读、搜、补丁、执行、分页，全部真跑 |
//! | 合成远端 | 有 | `gh` 是工作区里的假脚本，不联网、不认证 |
//! | 真实授权远端 | **没有** | 没有 GitHub 授权，一次真实请求都没发过 |
//!
//! 也就是说，"gld 能看 CI 状态"这句话在这里只被验到**策略和执行这一段**：
//! 只读子命令放行、写操作被拒、输出拿得回来。真 `gh` 认证之后还会不会有别的
//! 问题，这条测试证明不了。
//!
//! **整个文件只在 unix 上编译**：假 `gh` 是带 shebang 的 sh 脚本，Windows 上
//! 跑不起来。只给那条用例加 `#[cfg(unix)]` 是不够的——剩下的 `use` 和两个辅助
//! 函数就成了没人用的东西，Windows 上 `-D warnings` 直接编译失败。

#![cfg(unix)]

mod common;

use common::*;
use serde_json::{json, Value};

const TEST_PYTHON: &str = "python3";

#[test]
fn a_repository_maintenance_round_trip_holds_together_end_to_end() {
    let fx = repo_maintenance_fixture();
    let ctx = ctx_with_allowed_commands_and_path(
        &fx.root,
        &format!("only:{TEST_PYTHON},gh"),
        &fx.root.join("tools"),
    );

    // 1. 先看仓库里有什么。list_dir 给的 path 要能原样喂进 read_file（A12）。
    let listing = assert_ok(&invoke(&ctx, "list_dir", json!({"path": "."}))).clone();
    let readme = path_of(&listing, "README.md");
    let readme_read = assert_ok(&invoke(&ctx, "read_file", json!({"path": readme}))).clone();
    assert!(
        readme_read["content"]
            .as_str()
            .unwrap_or_default()
            .contains("# demo-service"),
        "{readme_read}"
    );
    let readme_version = readme_read["version"]
        .as_str()
        .expect("version")
        .to_string();

    // 2. 找 workflow。默认不进点开头的目录，所以第一次是零结果——但零结果得
    //    说得出是哪一种：这里要说清 `.github` 整棵没搜，而不是"没有匹配"。
    let blind = assert_ok(&invoke(
        &ctx,
        "search_text",
        json!({"query": "runs-on", "path": "."}),
    ))
    .clone();
    assert_eq!(blind["total_matches"], json!(0), "{blind}");
    assert!(
        blind["skipped_hidden_dirs"].as_u64().unwrap_or(0) >= 1,
        "{blind}"
    );
    assert!(
        warnings_of(&blind)
            .iter()
            .any(|w| w.contains("include_hidden=true") && w.contains(".github")),
        "零结果要指出 .github 没搜：{blind}"
    );

    let found = assert_ok(&invoke(
        &ctx,
        "search_text",
        json!({"query": "runs-on", "path": ".", "include_hidden": true}),
    ))
    .clone();
    let workflow = found["matches"][0]["path"]
        .as_str()
        .expect("workflow path")
        .to_string();
    assert_eq!(workflow, ".github/workflows/ci.yml", "{found}");
    // 搜出来的 path 同样要能直接读。
    let workflow_read = assert_ok(&invoke(&ctx, "read_file", json!({"path": &workflow}))).clone();
    assert!(
        workflow_read["content"]
            .as_str()
            .unwrap_or_default()
            .contains("ubuntu-latest"),
        "{workflow_read}"
    );

    // 3. 改 README。普通文件，不用确认；带上刚才读到的版本号。
    let readme_patch =
        "--- a/README.md\n+++ b/README.md\n@@\n-还没写。\n+见 .github/workflows/ci.yml。\n";
    let preflight = assert_ok(&invoke(
        &ctx,
        "patch_check",
        json!({"patch": readme_patch, "expected_versions": {"README.md": readme_version}}),
    ))
    .clone();
    assert_eq!(preflight["preflight"], json!(true), "{preflight}");
    assert_ok(&invoke(
        &ctx,
        "apply_patch",
        json!({"patch": readme_patch, "expected_versions": {"README.md": readme_version}}),
    ));

    // 4. 改 workflow。这一条要确认——它改的是 GitHub 上的行为，不只是这棵树。
    let workflow_patch = "--- a/.github/workflows/ci.yml\n+++ b/.github/workflows/ci.yml\n@@\n-on: [push]\n+on: [push, pull_request]\n";
    let refused = invoke(&ctx, "patch_check", json!({"patch": workflow_patch}));
    assert_eq!(
        assert_err(&refused)["error"]["code"],
        "DANGEROUS_OPERATION_REQUIRES_CONFIRMATION",
        "{refused}"
    );
    // 预检收的参数和真跑的一样，所以"确认之后还会不会有别的问题"能先问出来。
    assert_ok(&invoke(
        &ctx,
        "patch_check",
        json!({"patch": workflow_patch, "confirm": true}),
    ));
    let applied = assert_ok(&invoke(
        &ctx,
        "apply_patch",
        json!({"patch": workflow_patch, "confirm": true}),
    ))
    .clone();
    assert!(
        warnings_of(&applied)
            .iter()
            .any(|w| w.contains("ci_workflow")),
        "落盘结果要点名敏感改动：{applied}"
    );
    // 同一批里普通的 `.github` 配置不该被一起升级成"要确认"。
    let dependabot = "--- a/.github/dependabot.yml\n+++ b/.github/dependabot.yml\n@@\n-      interval: weekly\n+      interval: daily\n";
    assert_ok(&invoke(&ctx, "apply_patch", json!({"patch": dependabot})));

    // 5. 构建。输出远超单次调用能带回去的量，所以这里必然是截断的。
    let build = assert_ok(&invoke(
        &ctx,
        "exec_command",
        json!({
            "cmd": format!("{TEST_PYTHON} build.py"),
            "timeout_ms": 60_000,
            "yield_time_ms": 30_000,
            "max_output_bytes": 1024
        }),
    ))
    .clone();
    assert_eq!(build["command_ok"], json!(true), "{build}");
    assert_eq!(build["stdout_truncated"], json!(true), "{build}");
    let stdout_ref = build["output_refs"]["stdout"]
        .as_str()
        .expect("stdout ref")
        .to_string();

    // 6. 读长日志。照着 next_offset 翻到底，必须停得下来，也必须读得到结尾。
    let mut offset = 0u64;
    let mut pages = 0;
    let mut text = String::new();
    loop {
        let page = assert_ok(&invoke(
            &ctx,
            "read_output",
            json!({"output_ref": &stdout_ref, "offset": offset, "limit": 4096}),
        ))
        .clone();
        text.push_str(page["content"].as_str().unwrap_or_default());
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
    assert!(pages > 1, "这份日志应当不止一页");
    assert!(text.contains("[build] step 001/400"), "开头没读到");
    assert!(text.trim_end().ends_with("build ok"), "结尾没读到");

    // 7. 看 CI 状态。**这里往下是合成远端**：`gh` 是工作区 tools/ 下的假脚本。
    let rerun = invoke(&ctx, "check_command", json!({"cmd": "gh run rerun 17041"}));
    assert_eq!(rerun["decision"], "deny", "{rerun}");
    assert_eq!(rerun["rule"], "github_command_not_read_only", "{rerun}");

    let status = assert_ok(&invoke(
        &ctx,
        "exec_command",
        json!({"cmd": "gh run list", "timeout_ms": 20_000, "yield_time_ms": 20_000}),
    ))
    .clone();
    assert_eq!(status["command_ok"], json!(true), "{status}");
    let out = status["stdout"].as_str().unwrap_or_default();
    assert!(out.contains("17042") && out.contains("failure"), "{status}");

    // 8. 收尾：改动确实落在盘上，`.git` 一个字节都没被碰过。
    let readme_now = std::fs::read_to_string(fx.root.join("README.md")).expect("readme");
    assert!(
        readme_now.contains(".github/workflows/ci.yml"),
        "{readme_now}"
    );
    let workflow_now =
        std::fs::read_to_string(fx.root.join(".github/workflows/ci.yml")).expect("workflow");
    assert!(workflow_now.contains("pull_request"), "{workflow_now}");
    let into_git = invoke(
        &ctx,
        "apply_patch",
        json!({"patch": "*** Begin Patch\n*** Add File: .git/hooks/pre-commit\n+echo hi\n*** End Patch\n", "confirm": true}),
    );
    assert_eq!(
        assert_err(&into_git)["error"]["code"],
        "PROTECTED_REPOSITORY_ASSET",
        "{into_git}"
    );
}

/// `list_dir` 报的那个 path，一个字符都不改。
fn path_of(listing: &Value, name: &str) -> String {
    listing["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .find(|entry| entry["name"] == json!(name))
        .unwrap_or_else(|| panic!("列表里没有 {name}：{listing}"))["path"]
        .as_str()
        .expect("path")
        .to_string()
}

fn warnings_of(value: &Value) -> Vec<String> {
    value["warnings"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|w| w.as_str().map(str::to_string))
        .collect()
}
