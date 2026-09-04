//! `gld expose`：从"刚登记完项目"到"手里有个能贴进 ChatGPT 的公网地址"。
//!
//! 这条路以前要三条命令，中间那条 `gld restart` 忘了就是"配了没反应"。
//! 合成一条之后，值得盯住的是三件在真实使用中最容易出岔子的事：
//!
//! 1. 服务本来没在跑，expose 得自己把它拉起来——隧道把本地端口转出去，
//!    本地没人听的话公网地址拿到了也只是 502；
//! 2. 隧道起不来必须当场报错。start 内部那次隧道尝试是"失败只写日志"的
//!    （不能让隧道问题把服务一起拖垮），照搬到 expose 上就会变成
//!    "报告成功、但没有地址"；
//! 3. 关掉之后不能留残值：从有地址切到 --off，`gld connect` 不该还显示
//!    那个已经失效的地址。

mod common;

use common::env::{free_port, Env};

/// 一个假的 cloudflared：打出 quick 隧道那两行关键日志，然后挂着不退。
///
/// gld 认两样东西——含 `.trycloudflare.com` 的那行拿地址，
/// `Registered tunnel connection` 那行才算连上（只看到地址不算，
/// UDP 被挡的网络里地址照打但永远连不上）。
const FAKE_CLOUDFLARED: &str = "#!/bin/sh
echo 'INF |  https://gld-expose-test.trycloudflare.com  |'
echo 'INF Registered tunnel connection connIndex=0'
while true; do sleep 1; done
";

#[test]
fn expose_takes_a_fresh_workspace_all_the_way_to_a_public_url() {
    let mut env = Env::new();
    env.fake_binary("cloudflared", FAKE_CLOUDFLARED);
    let port = free_port();
    env.ok(&[
        "ws",
        "add",
        ".",
        "--name",
        "pub",
        "--mcp-port",
        &port.to_string(),
    ]);

    // 服务此刻没在跑，一条 expose 要负责起服务 + 起隧道 + 报地址。
    let text = env.ok(&["expose"]);
    assert!(
        text.contains("https://gld-expose-test.trycloudflare.com/mcp"),
        "没拿到公网地址：{text}"
    );

    let overview = env.json(&["--json", "ps"]);
    assert_eq!(
        overview.as_array().map(Vec::len),
        Some(1),
        "expose 应当把服务拉起来：{overview}"
    );

    // --off 要把地址清干净，不能留一个已经失效的值在 connect 里显示。
    env.ok(&["expose", "--off"]);
    let after = env.json(&["--json", "connect"]);
    assert_eq!(
        after["mcp"]["public_url"], "",
        "关掉之后还留着旧地址：{after}"
    );
}

/// 没装 cloudflared 时，expose 必须当场把安装办法给出来。
///
/// 这里不放假二进制，PATH 里就是没有——和用户第一次跑到这一步时一模一样。
#[test]
fn expose_reports_the_missing_binary_instead_of_a_silent_no_url() {
    let env = Env::new();
    env.ok(&[
        "ws",
        "add",
        ".",
        "--name",
        "nocf",
        "--mcp-port",
        &free_port().to_string(),
    ]);

    let output = env.gld(&["expose"]);
    assert!(!output.status.success(), "没有隧道却报成功了");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("未找到 cloudflared"),
        "报错要说清是缺哪个程序：{stderr}"
    );
    assert!(
        stderr.contains("brew install cloudflared"),
        "报错要给能直接敲的安装命令：{stderr}"
    );
}

/// `--url` 是"我自己已经有公网地址了"：只登记，不去起隧道。
///
/// 自建反向代理的人走这条路，机器上根本不会装 cloudflared，
/// 所以这里绝不能因为找不到隧道程序而失败。
#[test]
fn expose_with_a_ready_made_url_does_not_need_any_tunnel_binary() {
    let env = Env::new();
    env.ok(&[
        "ws",
        "add",
        ".",
        "--name",
        "own",
        "--mcp-port",
        &free_port().to_string(),
    ]);

    env.ok(&["expose", "--url", "https://mcp.example.com/"]);
    let connect = env.json(&["--json", "connect"]);
    assert_eq!(
        connect["mcp"]["public_url"], "https://mcp.example.com/mcp",
        "登记的公网地址没生效：{connect}"
    );
}

/// FRP 配置填了个不存在的名字，要在这一步就拦住并把已有的列出来。
#[test]
fn expose_rejects_an_unknown_frp_profile_up_front() {
    let env = Env::new();
    env.ok(&[
        "ws",
        "add",
        ".",
        "--name",
        "frp-ws",
        "--mcp-port",
        &free_port().to_string(),
    ]);
    env.ok(&[
        "frp",
        "add",
        "--name",
        "office",
        "--server",
        "frp.example.com",
    ]);

    let output = env.gld(&["expose", "--frp", "nope"]);
    assert!(!output.status.success(), "不存在的 FRP 配置被接受了");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("没有名为「nope」"), "{stderr}");
    assert!(
        stderr.contains("office"),
        "报错里要列出已有的配置：{stderr}"
    );
}

/// 反过来也得挡：正在被引用的 FRP 配置不能说删就删。
///
/// 删掉之后引用它的工作区不会有任何变化，直到某次 start 报
/// 「引用的 FRP 配置 3f58e6b4… 不存在」——那时手里只剩一个 id，
/// 已经查不出它原来是哪台服务器、token 是什么了。
#[test]
fn removing_an_frp_profile_still_in_use_needs_force() {
    let env = Env::new();
    env.ok(&[
        "ws",
        "add",
        ".",
        "--name",
        "user",
        "--mcp-port",
        &free_port().to_string(),
    ]);
    env.ok(&[
        "frp",
        "add",
        "--name",
        "office",
        "--server",
        "frp.example.com",
    ]);
    env.ok(&["ws", "set", "tunnel=frp", "frp-profile=office"]);
    let id = env.json(&["--json", "frp", "list"])[0]["id"]
        .as_str()
        .expect("id")
        .to_string();

    let output = env.gld(&["frp", "remove", &id]);
    assert!(!output.status.success(), "还在用的配置被静默删了");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("还在被这些地方用着"), "{stderr}");
    assert!(
        stderr.contains("工作区「user」的 MCP"),
        "要指名道姓说是谁在用：{stderr}"
    );
    assert_eq!(
        env.json(&["--json", "frp", "list"])
            .as_array()
            .map(Vec::len),
        Some(1),
        "被拒绝的删除不能留下半个状态"
    );

    // --force 是留给"我知道我在干什么"的出口。
    env.ok(&["frp", "remove", &id, "--force"]);
    assert_eq!(
        env.json(&["--json", "frp", "list"])
            .as_array()
            .map(Vec::len),
        Some(0)
    );
}
