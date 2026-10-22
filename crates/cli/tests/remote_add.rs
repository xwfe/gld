//! `gld remote add`：只给 ccnm 里的项目名就够，名字跟着它、默认 coding、机器交给 ccnm 选；
//! 旧写法 `gld remote add <名字> --node … --remote-workspace …` 照样认。
//!
//! 只写配置、不去连，所以不用装 ccnm。

mod common;

use common::env::Env;

/// 服务里的远端项目：(名字, node, ccnm 里的项目名, 访问上限)，排好序。
fn remotes(env: &Env) -> Vec<(String, String, String, String)> {
    let state = env.json(&["--json", "ls"]);
    let mut rows: Vec<_> = state["service"]["members"]
        .as_array()
        .expect("members")
        .iter()
        .filter(|member| member["kind"] == "remote")
        .map(|member| {
            let field = |key: &str| member[key].as_str().unwrap_or_default().to_string();
            (
                field("name"),
                field("node"),
                field("workspace"),
                field("mode"),
            )
        })
        .collect();
    rows.sort();
    rows
}

#[test]
fn only_the_ccnm_project_name_is_needed() {
    let env = Env::new();
    env.ok(&["remote", "add", "server"]);
    env.ok(&[
        "remote",
        "add",
        "prod",
        "--node",
        "work",
        "--remote-workspace",
        "api",
    ]);
    env.ok(&["remote", "add", "api", "--name", "api-ro", "--mode", "read"]);

    let row = |name: &str, node: &str, workspace: &str, mode: &str| {
        (
            name.to_string(),
            node.to_string(),
            workspace.to_string(),
            mode.to_string(),
        )
    };
    assert_eq!(
        remotes(&env),
        vec![
            row("api-ro", "", "api", "read"),
            row("prod", "work", "api", "coding"),
            row("server", "", "server", "coding"),
        ]
    );
}

/// 旧写法里位置参数是名字，再给 --name 就分不清哪个算数了。
#[test]
fn a_name_and_the_old_remote_workspace_flag_do_not_mix() {
    let env = Env::new();
    let output = env.gld(&[
        "remote",
        "add",
        "x",
        "--name",
        "y",
        "--remote-workspace",
        "z",
    ]);
    assert!(!output.status.success());
    assert!(remotes(&env).is_empty(), "拒了就不该写进配置");
}
