//! 旧数据布局还能被读进来。
//!
//! 数据目录根下曾经是两个文件：`profiles.json`（只有工作区列表）和
//! `app_settings.json`（FRP 配置、代理、密钥）。现在合成了一份
//! `data/profiles.json`。迁移代码只在新文件还不存在时跑一次，跑完就把旧文件
//! 改名成 `.bak`——也就是说**每个数据目录一辈子只经历一次**，坏了不会有人
//! 立刻发现，等发现时用户已经"升级之后工作区全没了"。
//!
//! 而这条路径丢的是密钥：它们随机生成、没有第二份副本，读不到就等于每个
//! ChatGPT 连接器都要重配一遍。所以这里把它钉死。

mod common;

use common::env::Env;

#[test]
fn a_workspace_and_its_secrets_survive_the_old_two_file_layout() {
    let env = Env::new();
    env.ok(&["ws", "add", ".", "--name", "oldlayout"]);
    let token_before = env.json(&[
        "--json",
        "secret",
        "show",
        "bearer_token",
        "-w",
        "oldlayout",
        "--reveal",
    ])["value"]
        .as_str()
        .expect("bearer_token")
        .to_string();

    // 把当前数据拆回旧布局：根目录两个文件，data/ 整个删掉。
    let home = env.home.path();
    let current: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(home.join("data/profiles.json")).expect("读当前数据"),
    )
    .expect("解析当前数据");
    std::fs::write(
        home.join("profiles.json"),
        serde_json::json!({ "profiles": current["profiles"] }).to_string(),
    )
    .expect("写旧 profiles.json");
    // 两份文件都是 snake_case（AppData 和 AppSettings 都没开 rename_all）。
    std::fs::write(
        home.join("app_settings.json"),
        serde_json::json!({
            "workspace_secrets": current["workspace_secrets"],
            "last_workspace_id": current["last_workspace_id"],
        })
        .to_string(),
    )
    .expect("写旧 app_settings.json");
    std::fs::remove_dir_all(home.join("data")).expect("删掉新布局");

    // 下一条命令应当把旧布局读进来。
    let listed = env.ok(&["ws", "list"]);
    assert!(listed.contains("oldlayout"), "工作区没迁过来：{listed}");

    let token_after = env.json(&[
        "--json",
        "secret",
        "show",
        "bearer_token",
        "-w",
        "oldlayout",
        "--reveal",
    ])["value"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert_eq!(
        token_after, token_before,
        "密钥没跟着迁移——客户端里存的 token 会全部失效"
    );

    // 旧文件改名备份，不留在原地重复触发。
    assert!(
        home.join("profiles.json.bak").is_file(),
        "旧 profiles.json 没备份"
    );
    assert!(
        home.join("app_settings.json.bak").is_file(),
        "旧 app_settings.json 没备份"
    );
    assert!(home.join("data/profiles.json").is_file(), "没写出新布局");
}
