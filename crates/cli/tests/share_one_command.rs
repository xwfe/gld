//! `gld share`：从"刚登记完项目"到"手里有个能贴进 ChatGPT 的公网地址"。
//!
//! 这条路以前要三条命令，中间那条 `gld restart` 忘了就是"配了没反应"。
//! 合成一条之后，值得盯住的是三件在真实使用中最容易出岔子的事：
//!
//! 1. 服务本来没在跑，share 得自己把它拉起来——隧道把本地端口转出去，
//!    本地没人听的话公网地址拿到了也只是 502；
//! 2. 隧道起不来必须当场报错。start 内部那次隧道尝试是"失败只写日志"的
//!    （不能让隧道问题把服务一起拖垮），照搬到 share 上就会变成
//!    "报告成功、但没有地址"；
//! 3. 关掉之后不能留残值：从有地址切到 --off，`gld list` 不该还显示
//!    那个已经失效的地址。

mod common;

use common::env::{free_port, Env};

/// 一个假的 cloudflared：打出 quick 隧道那两行关键日志，然后挂着不退。
///
/// gld 认两样东西——含 `.trycloudflare.com` 的那行拿地址，
/// `Registered tunnel connection` 那行才算连上（只看到地址不算，
/// UDP 被挡的网络里地址照打但永远连不上）。
const FAKE_CLOUDFLARED: &str = "#!/bin/sh
echo 'INF |  https://gld-share-test.trycloudflare.com  |'
echo 'INF Registered tunnel connection connIndex=0'
while true; do sleep 1; done
";

#[test]
fn share_takes_a_fresh_workspace_all_the_way_to_a_public_url() {
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

    // 服务此刻没在跑，一条 share 要负责起服务 + 起隧道 + 报地址。
    let text = env.ok(&["share"]);
    assert!(
        text.contains("https://gld-share-test.trycloudflare.com/mcp"),
        "没拿到公网地址：{text}"
    );

    let overview = env.json(&["--json", "ps"]);
    assert_eq!(
        overview.as_array().map(Vec::len),
        Some(1),
        "share 应当把服务拉起来：{overview}"
    );

    // --off 要把地址清干净，不能留一个已经失效的值在 ls 里显示。
    env.ok(&["share", "--off"]);
    let after = env.json(&["--json", "list"]);
    assert_eq!(
        after["mcp"]["public_url"], "",
        "关掉之后还留着旧地址：{after}"
    );
}

/// 没装 cloudflared 时，share 必须当场把安装办法给出来。
///
/// 本机真的装了 cloudflared 时这条测试没法跑，直接跳过。
///
/// gld 找隧道程序不只看 PATH，还会直接探 `/opt/homebrew/bin/cloudflared`、
/// `/usr/local/bin/cloudflared`（GUI 启动时 PATH 常常不全，所以有这层兜底）。
/// 也就是说装过的机器上没有任何环境变量能造出"没装"这个场景，硬跑只会得到
/// 一条与本测试无关的红色——而 CI 的干净镜像里它照常有效。
#[test]
fn share_reports_the_missing_binary_instead_of_a_silent_no_url() {
    if let Ok(found) = gld_core::tunnel::resolve_cloudflared() {
        eprintln!(
            "跳过：本机装着 cloudflared（{}），造不出没装的场景",
            found.display()
        );
        return;
    }
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

    let output = env.gld(&["share"]);
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

/// `--tunnel <地址>` 是"我自己已经有公网地址了"：只登记，不去起隧道。
///
/// 自建反向代理的人走这条路，机器上根本不会装 cloudflared，
/// 所以这里绝不能因为找不到隧道程序而失败。
#[test]
fn a_ready_made_url_does_not_need_any_tunnel_binary() {
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

    env.ok(&["share", "--tunnel", "https://mcp.example.com/"]);
    let ls = env.json(&["--json", "list"]);
    assert_eq!(
        ls["mcp"]["public_url"], "https://mcp.example.com/mcp",
        "登记的公网地址没生效：{ls}"
    );
}

/// 用户手里的地址通常是从客户端复制来的完整端点，末尾就带着 `/mcp`。
///
/// 存下去前不把它去掉，配置里是 `https://x.com/mcp`，而端点是拼出来的，
/// 客户端拿到的就成了 `https://x.com/mcp/mcp`——404，且看不出哪里错了。
#[test]
fn a_pasted_endpoint_does_not_end_up_doubled() {
    let env = Env::new();
    env.ok(&[
        "ws",
        "add",
        ".",
        "--name",
        "pasted",
        "--mcp-port",
        &free_port().to_string(),
    ]);

    env.ok(&["share", "--tunnel", "https://mcp.example.com/mcp"]);
    assert_eq!(
        env.json(&["--json", "list"])["mcp"]["public_url"],
        "https://mcp.example.com/mcp"
    );
}

/// 缺 Tunnel Token 要在动手之前拦住，并说清楚怎么补上。
///
/// 以前这个值只能事先 `gld secret set cloudflare_token`，忘了的话整条命令会
/// 一路打成功——登记工作区、写配置、起服务——直到最后一步才蹦出
/// "需要填写 Tunnel Token"。前面全是成功，用户只会以为整条命令失败了。
#[test]
fn a_named_cloudflare_tunnel_refuses_before_it_starts_anything() {
    let env = Env::new();
    let output = env.gld(&[
        "start",
        ".",
        "--port",
        &free_port().to_string(),
        "--tunnel",
        "cf:mcp.example.com",
    ]);

    assert!(!output.status.success(), "没有 token 却报成功了");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Tunnel Token"), "没说缺什么：{stderr}");
    assert!(stderr.contains("--token"), "报错要给出怎么补上：{stderr}");
    assert_eq!(
        env.json(&["--json", "ps"]).as_array().map(Vec::len),
        Some(0),
        "拦截发生得太晚，服务已经起来了"
    );
}

/// `--tunnel-token` 让固定域名一步到位，不用先记着去 secret set。
#[test]
fn the_tunnel_token_can_be_given_on_the_command_line() {
    let env = Env::new();
    env.ok(&[
        "ws",
        "add",
        ".",
        "--name",
        "named",
        "--mcp-port",
        &free_port().to_string(),
    ]);

    // 服务没在跑，upgrade 只写配置不去起隧道——这里要验的是 token 有没有落到位。
    env.ok(&[
        "upgrade",
        "--tunnel",
        "cf:mcp.example.com",
        "--tunnel-token",
        "tok-abc123",
    ]);

    let secret = env.json(&["--json", "secret", "show", "cloudflare_token", "--reveal"]);
    assert_eq!(secret["value"], "tok-abc123", "token 没存进去：{secret}");
    assert_eq!(
        env.json(&["--json", "list"])["mcp"]["public_url"],
        "https://mcp.example.com/mcp"
    );
}

/// FRP 配置填了个不存在的名字，要在这一步就拦住并把已有的列出来。
#[test]
fn share_rejects_an_unknown_frp_profile_up_front() {
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

    let output = env.gld(&["share", "--tunnel", "frp:nope"]);
    assert!(!output.status.success(), "不存在的 FRP 配置被接受了");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("没有名为「nope」"), "{stderr}");
    assert!(
        stderr.contains("office"),
        "报错里要列出已有的配置：{stderr}"
    );
}

/// `--tunnel` 写错时要在动配置之前就退出，并把可用写法摆出来。
#[test]
fn a_malformed_tunnel_value_is_rejected_before_anything_changes() {
    let env = Env::new();
    env.ok(&[
        "ws",
        "add",
        ".",
        "--name",
        "bad",
        "--mcp-port",
        &free_port().to_string(),
    ]);

    // 忘了协议头是最常见的写法错误，不能被当成某种模式名默默吃掉。
    let output = env.gld(&["share", "--tunnel", "mcp.example.com"]);
    assert_eq!(output.status.code(), Some(2), "参数错误应当是退出码 2");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("看不懂的公网入口"), "{stderr}");
    assert!(
        stderr.contains("frp:<配置名>"),
        "报错要列出可用写法：{stderr}"
    );

    assert_eq!(
        env.json(&["--json", "list"])["mcp"]["public_url"],
        "",
        "被拒绝的参数不该改到配置"
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
