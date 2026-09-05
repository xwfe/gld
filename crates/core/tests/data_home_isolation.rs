//! 跑测试不能把用户的 `~/.config/gld` 写脏。
//!
//! 这条曾经不成立：`ToolContext::new` 会去 `~/.config/gld/harness` 建目录，
//! `managed_frpc_config_path` 这类"取路径"的函数也会顺手 create_dir_all。
//! 结果是 clone 下来跑一次 `cargo test`，主目录里就多出
//! `~/.config/gld/harness/workspaces/<一堆 id>` 和 `~/.config/gld/frpc/first-workspace`
//! 这种测试夹具目录——而且没人会注意到。

mod common;

use std::path::PathBuf;

/// 建一个真的 ToolContext（以前就是这一步写脏主目录的），确认状态落在隔离目录里。
///
/// 这里只做正向断言：harness 必须出现在 GLD_HOME 下。不去断言真实的
/// `~/.config/gld` 不存在——用户自己用过 gld 的话它本来就在，那样会误报。
#[test]
fn tool_context_state_lands_inside_the_isolated_data_home() {
    common::isolate_data_home();

    let home = std::env::var_os("GLD_HOME")
        .map(PathBuf::from)
        .expect("isolate_data_home 应当设好 GLD_HOME");

    let fixture = common::tiny_js_fixture();
    let _ctx = common::ctx_for(&fixture.root);

    assert!(
        home.join("harness").exists(),
        "Harness 状态应当落在隔离的数据目录 {} 里",
        home.display()
    );
}
