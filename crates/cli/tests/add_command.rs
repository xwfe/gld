//! `gld add` 的参数怎么理解：有一个目录不存在就一个都不加，单个词不存在时提示 --name。

mod common;

use common::env::Env;

/// 服务里的项目名（`gld --json ls` 的 `service.members`）。
fn names(env: &Env) -> Vec<String> {
    let listed = env.json(&["--json", "ls"]);
    let members = listed["service"]["members"]
        .as_array()
        .unwrap_or_else(|| panic!("没有 service.members：{listed}"));
    members
        .iter()
        .filter_map(|member| member["name"].as_str().map(str::to_string))
        .collect()
}

#[test]
fn nothing_is_added_when_one_directory_is_missing() {
    let env = Env::new();
    let real = env.home.path().join("real");
    std::fs::create_dir_all(&real).expect("real");
    let missing = env.home.path().join("missing");

    let output = env.gld(&[
        "add",
        real.to_str().expect("path"),
        missing.to_str().expect("path"),
    ]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("一个都没加"), "{stderr}");
    assert!(!names(&env).contains(&"real".to_string()), "不能加了一半");

    // 单独一个不存在的词：可能是想起名字，提示 --name。
    let word = env.gld(&["add", "nosuch-project"]);
    assert!(!word.status.success());
    assert!(
        String::from_utf8_lossy(&word.stderr).contains("--name nosuch-project"),
        "{}",
        String::from_utf8_lossy(&word.stderr)
    );
    env.ok(&["daemon", "stop"]);
}
