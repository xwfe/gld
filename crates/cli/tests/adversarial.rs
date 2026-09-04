//! 对抗式审查里打穿过的几条路，用真实二进制 + 真实 HTTP 锁住。
//!
//! 每条都是先复现、再修的，注释里写的是**当时的实际症状**，
//! 不是设想的风险——回归时照着注释就能判断是不是同一个问题回来了。

mod common;

use std::fs;

use common::env::{free_port, Env};
use common::http::{post_json, request, Reply};
use serde_json::Value;

/// 起一个带 bearer 认证的 MCP，返回端口和 token。
fn running_mcp(env: &Env, name: &str) -> (u16, String) {
    let port = free_port();
    env.ok(&[
        "ws",
        "add",
        ".",
        "--name",
        name,
        "--mcp-port",
        &port.to_string(),
    ]);
    env.ok(&["ws", "set", "mcp.auth=bearer"]);
    let token = env.json(&["--json", "secret", "show", "bearer_token", "--reveal"])["value"]
        .as_str()
        .expect("bearer token")
        .to_string();
    env.ok(&["start"]);
    (port, token)
}

/// 调一个工具，返回工具自己的结构化结果（不是 JSON-RPC 外壳）。
fn call_tool(port: u16, token: &str, name: &str, arguments: Value) -> Value {
    let body = serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": { "name": name, "arguments": arguments }
    })
    .to_string();
    let reply = post_json(port, "/mcp", &body, Some(token));
    assert_eq!(reply.status, 200, "HTTP 层就失败了：{}", reply.body);
    let text = reply.json()["result"]["content"][0]["text"]
        .as_str()
        .expect("工具结果")
        .to_string();
    serde_json::from_str(&text).expect("工具结果是 JSON")
}

/// 数据文件坏掉时必须停机，不能当成"还没配过"。
///
/// 修之前：截断 profiles.json 之后 `ws list` 回一句"还没有工作区"，
/// 下一条会写盘的命令把空白配置存回去——所有工作区和所有密钥一起没了，
/// 全程没有任何提示。密钥是随机生成、只存在这一个文件里的。
#[test]
fn a_corrupt_data_file_stops_gld_instead_of_wiping_every_secret() {
    let env = Env::new();
    env.ok(&["ws", "add", ".", "--name", "precious"]);

    let data_file = env.home.path().join("data").join("profiles.json");
    let original = fs::read_to_string(&data_file).expect("读数据文件");
    fs::write(&data_file, &original[..original.len() / 2]).expect("截断");

    let listed = env.gld(&["ws", "list"]);
    assert!(!listed.status.success(), "坏文件必须让命令失败");
    let message = String::from_utf8_lossy(&listed.stderr).into_owned();
    assert!(message.contains("数据文件解析失败"), "{message}");
    // 报错要能直接照着做。
    assert!(message.contains("还原备份"), "{message}");

    // 关键断言：坏文件原地保留，没有被空白配置覆盖。
    let after = env.gld(&["ws", "add", ".", "--name", "second"]);
    assert!(!after.status.success());
    assert_eq!(
        fs::read_to_string(&data_file).expect("文件还在"),
        original[..original.len() / 2],
        "gld 不该动这个文件"
    );
}

/// 读工具不能读到 gld 自己的密钥库——**哪怕越界读已经被用户放开**。
///
/// 修之前四条路都通：read_file 绝对路径直读、list_dir 列目录、
/// search_text / list_files 从上一级递归走进去。而 profiles.json 里是
/// **每个**工作区的 bearer_token 明文——读一个工作区就等于拿到全部连接器。
///
/// 这里特意把 mcp.confine-reads 关掉：开着的话请求被外层那道门拦下，
/// 测到的是 READS_CONFINED_TO_WORKSPACE，数据目录这道独立的门根本没走到。
/// 而数据目录恰恰是"用户关掉了限制也不给读"的那一条。
#[test]
fn the_gld_data_home_is_not_readable_even_when_confinement_is_off() {
    let env = Env::new();
    env.write("probe.txt", "workspace file\n");
    let (port, token) = running_mcp(&env, "victim");
    env.ok(&["ws", "set", "mcp.confine-reads=false"]);
    env.ok(&["restart"]);
    let home = env.home.path().display().to_string();

    let denied = |result: &Value| result["error"]["code"] == "GLD_DATA_HOME_DENIED";

    let read = call_tool(
        port,
        &token,
        "read_file",
        serde_json::json!({ "path": format!("{home}/data/profiles.json") }),
    );
    assert!(denied(&read), "read_file 读到了密钥库：{read}");

    let listed = call_tool(
        port,
        &token,
        "list_dir",
        serde_json::json!({ "path": format!("{home}/data") }),
    );
    assert!(denied(&listed), "list_dir 列出了数据目录：{listed}");

    let searched = call_tool(
        port,
        &token,
        "search_text",
        serde_json::json!({
            "query": "bearer_token", "path": format!("{home}/data"),
            "include_hidden": true, "include_ignored": true
        }),
    );
    assert!(denied(&searched), "search_text 搜到了密钥库：{searched}");

    // 还有一条路是"起点在数据目录上一级、靠 WalkDir 自己走进去"——
    // 单靠入口校验挡不住，得靠遍历侧的过滤。这里不测它：数据目录的上一级
    // 是系统临时目录，递归走一遍要十几秒还会读到别的测试的残留。
    // 那条由 gld-core 的单测 `walking_into_the_gld_data_home_is_skipped`
    // 直接断言遍历过滤函数，更精确也更快。

    // 反向断言：关掉限制之后，Workspace 外的**无关**文件确实读得到——
    // 证明上面几条挡的是数据目录本身，不是开关没生效。
    let neighbour = env
        .project
        .path()
        .parent()
        .expect("上一级")
        .join("gld-neighbour.txt");
    fs::write(&neighbour, "neighbour\n").expect("写外部文件");
    let outside = call_tool(
        port,
        &token,
        "read_file",
        serde_json::json!({ "path": neighbour.display().to_string() }),
    );
    let _ = fs::remove_file(&neighbour);
    assert_eq!(
        outside["ok"], true,
        "confine-reads=false 之后外部文件该读得到：{outside}"
    );

    let inside = call_tool(
        port,
        &token,
        "read_file",
        serde_json::json!({ "path": "probe.txt" }),
    );
    assert_eq!(inside["ok"], true, "工作区内的文件被误挡了：{inside}");
}

/// 默认就把读限制在 Workspace 内，而且报错要告诉人怎么放开。
///
/// 这是 0.3.0 改的默认值：桌面版和之前的 gld 都允许 `read_file` 给绝对路径
/// 读 `~/.ssh/id_rsa`。本机自用时那只是方便，但服务能挂到公网给 ChatGPT 用，
/// 仓库里任何一段文字都可能是提示词注入，"能读整台机器"就成了实打实的风险。
#[test]
fn reads_are_confined_to_the_workspace_by_default() {
    let env = Env::new();
    env.write("probe.txt", "workspace file\n");
    let (port, token) = running_mcp(&env, "confined");

    let neighbour = env
        .project
        .path()
        .parent()
        .expect("上一级")
        .join("gld-outside.txt");
    fs::write(&neighbour, "outside\n").expect("写外部文件");
    let blocked = call_tool(
        port,
        &token,
        "read_file",
        serde_json::json!({ "path": neighbour.display().to_string() }),
    );
    assert_eq!(
        blocked["error"]["code"], "READS_CONFINED_TO_WORKSPACE",
        "默认就该挡住：{blocked}"
    );
    // 读隔壁仓库是正当需求，报错必须给出放开的办法。
    assert!(
        blocked["error"]["message"]
            .as_str()
            .unwrap_or("")
            .contains("confine-reads=false"),
        "报错里要写清楚怎么关：{blocked}"
    );

    // 开关得真的有用：关掉之后同一条路径要能读。
    env.ok(&["ws", "set", "mcp.confine-reads=false"]);
    env.ok(&["restart"]);
    let allowed = call_tool(
        port,
        &token,
        "read_file",
        serde_json::json!({ "path": neighbour.display().to_string() }),
    );
    let _ = fs::remove_file(&neighbour);
    assert_eq!(allowed["ok"], true, "关掉之后该能读：{allowed}");
}

/// bearer 认证没有 token 时，服务不许起来。
///
/// 修之前照起不误：`gld status` 显示 running、端口也通，但**任何**请求都拿 401
/// ——包括配置完全正确的客户端。用户看到的是"服务好好的，客户端连不上"。
/// Actions 那侧一直会拒绝启动，MCP 这侧没有，两边不一致。
#[test]
fn mcp_refuses_to_start_when_bearer_auth_has_no_token() {
    let env = Env::new();
    let port = free_port();
    env.ok(&[
        "ws",
        "add",
        ".",
        "--name",
        "tokenless",
        "--mcp-port",
        &port.to_string(),
    ]);
    env.ok(&["ws", "set", "mcp.auth=bearer"]);

    // 模拟数据文件里没有这个密钥（手工编辑 / 旧版导入 / 备份缺字段）。
    let data_file = env.home.path().join("data").join("profiles.json");
    let mut data: Value =
        serde_json::from_str(&fs::read_to_string(&data_file).expect("读")).expect("解析");
    for secrets in data["workspace_secrets"]
        .as_object_mut()
        .expect("workspace_secrets")
        .values_mut()
    {
        secrets
            .as_object_mut()
            .expect("secrets")
            .remove("bearer_token");
    }
    fs::write(&data_file, data.to_string()).expect("写回");

    let started = env.gld(&["start"]);
    let message = format!(
        "{}{}",
        String::from_utf8_lossy(&started.stdout),
        String::from_utf8_lossy(&started.stderr)
    );
    assert!(!started.status.success(), "不该启动成功：{message}");
    assert!(message.contains("bearer_token"), "{message}");
    // 报错里要给能直接敲的命令。
    assert!(
        message.contains("gld secret regen bearer_token"),
        "{message}"
    );
}

/// 网关不能让公网请求自己指定"这个服务对外叫什么"。
///
/// 上游的 external_base_url 在没配 public_url 时会认 `X-Forwarded-Host`——
/// 这是给"用户自己架反向代理"用的，直连时必须保留。但全局网关是直接挂在
/// 公网上的，它原样转发这几个头就等于把 OAuth 的 issuer、发现文档里的端点、
/// 授权页表单的 action 全交给请求方决定。
#[test]
fn the_gateway_does_not_let_the_public_dictate_the_upstream_identity() {
    let env = Env::new();
    let mcp_port = free_port();
    let gateway_port = free_port();
    let id = env.json(&[
        "--json",
        "ws",
        "add",
        ".",
        "--name",
        "spoof",
        "--mcp-port",
        &mcp_port.to_string(),
    ])["id"]
        .as_str()
        .expect("workspace id")
        .to_string();
    // public_url 留空，逼上游走"认请求头"那条分支——有配置时头本来就不参与。
    env.ok(&["ws", "set", "mcp.auth=oauth", "mcp.global-gateway=true"]);
    env.ok(&[
        "gateway",
        "set",
        "--enabled",
        "true",
        "--port",
        &gateway_port.to_string(),
        "--tunnel",
        "none",
    ]);
    env.ok(&["start", "-s", "mcp"]);
    env.ok(&["gateway", "start"]);

    let spoofed = [
        ("X-Forwarded-Host", "evil.example"),
        ("X-Forwarded-Proto", "https"),
    ];
    let issuer = |reply: &Reply| reply.json()["issuer"].as_str().unwrap_or("").to_string();
    let metadata = "/.well-known/oauth-authorization-server";

    // 直连必须仍然认这个头：用户自己架 nginx / cloudflared 时靠的就是它。
    let direct = request(mcp_port, "GET", metadata, &spoofed, "");
    assert_eq!(
        issuer(&direct),
        "https://evil.example",
        "直连时反代场景的转发头不该被吃掉：{}",
        direct.body
    );

    // 过网关就不认了。
    let through_reply = request(
        gateway_port,
        "GET",
        &format!("{metadata}/w/{id}"),
        &spoofed,
        "",
    );
    let through_gateway = issuer(&through_reply);
    assert!(
        !through_gateway.contains("evil.example"),
        "网关把公网传进来的 X-Forwarded-Host 透给上游了：{}",
        through_reply.body
    );
    assert!(
        through_gateway.starts_with("http://127.0.0.1"),
        "应当退回本机地址，实际：{through_gateway}"
    );

    env.ok(&["gateway", "stop"]);
    env.ok(&["stop"]);
}

/// `Authorization` 的 scheme 大小写不敏感（RFC 7235 / RFC 6750）。
///
/// 修之前写死 `strip_prefix("Bearer ")`，发 `authorization: bearer xxx`
/// 的客户端一直拿 401——而 token 明明是对的，这种 401 最难查。
#[test]
fn the_authorization_scheme_is_matched_case_insensitively() {
    let env = Env::new();
    let (port, token) = running_mcp(&env, "casing");
    let body = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;

    for scheme in ["Bearer", "bearer", "BEARER"] {
        let header = format!("{scheme} {token}");
        let reply = request(
            port,
            "POST",
            "/mcp",
            &[
                ("Content-Type", "application/json"),
                ("Authorization", &header),
            ],
            body,
        );
        assert_eq!(reply.status, 200, "{scheme} 应当被接受：{}", reply.body);
    }

    // 错 token 仍然要拒。
    assert_eq!(post_json(port, "/mcp", body, Some("wrong")).status, 401);
}
