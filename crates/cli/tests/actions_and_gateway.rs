//! 另外两条"接客户端"的路：GPT Actions 和全局入口。
//!
//! MCP 服务那条路有 oauth_connector_flow 盯着，这两条之前只有单元测试。
//! 它们的失败方式都很难查：
//!
//! - Actions（RFC-0004 之后唯一还按项目起的线路）：自定义 GPT 是照着
//!   `/openapi.json` 生成调用的。文档里少一个 security、servers 写的是 127.0.0.1，
//!   GPT 那边只会说"调用失败"，本机 curl 却一切正常，因为 curl 不看文档。
//! - 全局入口（旧的共享入口）：按 `/w/<id>` 和 `/hub` 分流。路由错了就是把请求
//!   转给了别人的项目——这种错误不会报错，只会读到别人的文件。

mod common;

use std::time::Duration;

use common::env::{free_port, Env};
use common::http::{get, post_json};

/// Actions 服务：文档能被 GPT 导入，接口按 API Key 认证。
#[test]
fn actions_service_serves_an_importable_openapi_and_enforces_the_api_key() {
    let env = Env::new();
    env.write("note.txt", "actions-e2e-marker\n");
    let port = free_port();
    env.ok(&["add", ".", "--name", "act"]);
    env.ok(&["set", "act", &format!("actions.port={port}")]);
    env.ok(&["start", "-s", "actions"]);

    let health = get(port, "/health").json();
    assert_eq!(health["ok"], true);
    assert_eq!(
        health["auth_type"], "api_key",
        "新项目的 Actions 默认用 API Key"
    );
    let tools_loaded = health["tools_loaded"].as_u64().unwrap_or(0);
    assert!(tools_loaded > 0, "一个工具都没装上：{health}");

    let doc = get(port, "/openapi.json").json();
    assert_eq!(doc["openapi"], "3.1.0");
    assert_eq!(
        doc["servers"][0]["url"],
        format!("http://127.0.0.1:{port}"),
        "没配公网时，文档里的 servers 应当就是本机地址"
    );

    let paths = doc["paths"].as_object().expect("paths");
    assert_eq!(
        paths.len() as u64,
        tools_loaded,
        "/health 报的工具数应当就是文档里的接口数"
    );
    for (path, item) in paths {
        let operation = &item["post"];
        assert!(
            operation["operationId"].is_string(),
            "{path} 缺 operationId，GPT 靠它给函数命名"
        );
        assert_eq!(
            operation["security"][0]["bearerAuth"],
            serde_json::json!([]),
            "{path} 没声明 security，GPT 就不会带 Authorization 头"
        );
        assert!(
            operation["x-openai-isConsequential"].is_boolean(),
            "{path} 缺 x-openai-isConsequential，GPT 会对每次调用都弹确认"
        );
    }
    assert!(
        doc["components"]["securitySchemes"]["bearerAuth"]["scheme"] == "bearer",
        "securitySchemes 对不上：{}",
        doc["components"]["securitySchemes"]
    );

    // 自定义 GPT 要求填一个隐私政策地址，服务自带一页。
    assert_eq!(get(port, "/privacy").status, 200);

    // 认证：没 Key、错 Key 都得挡住，对的 Key 才真的执行工具。
    let key = env.json(&[
        "--json",
        "secret",
        "ls",
        "actions_api_key",
        "--reveal",
        "-w",
        "act",
    ])["value"]
        .as_str()
        .expect("actions_api_key")
        .to_string();
    let body = r#"{"path":"note.txt"}"#;
    assert_eq!(
        post_json(port, "/actions/read_file", body, None).status,
        401
    );
    assert_eq!(
        post_json(port, "/actions/read_file", body, Some("wrong-key")).status,
        401
    );
    let called = post_json(port, "/actions/read_file", body, Some(&key));
    assert_eq!(called.status, 200, "带对 Key 也调不通：{}", called.body);
    assert!(
        called.body.contains("actions-e2e-marker"),
        "没读到项目里的文件：{}",
        called.body
    );

    // 换成 OAuth：文档也得跟着换，否则 GPT 不知道要走授权流程，一样次次 401。
    env.ok(&["set", "actions.auth=oauth"]);
    env.ok(&["restart", "-s", "actions"]);
    let doc = get(port, "/openapi.json").json();
    let flow = &doc["components"]["securitySchemes"]["oauthAuth"]["flows"]["authorizationCode"];
    assert_eq!(
        flow["authorizationUrl"],
        format!("http://127.0.0.1:{port}/oauth/authorize"),
        "OAuth 模式下文档要给出授权端点：{}",
        doc["components"]["securitySchemes"]
    );
    assert_eq!(
        doc["paths"]["/actions/read_file"]["post"]["security"],
        serde_json::json!([{ "oauthAuth": [] }])
    );
    // 服务端也确实换成要 OAuth token 了：原来的 API Key 不再好使。
    assert_eq!(
        post_json(port, "/actions/read_file", body, Some(&key)).status,
        401,
        "换成 OAuth 后旧的 API Key 不该还能用"
    );

    env.ok(&["stop"]);
}

/// 全局入口：按 `/w/<项目 id>/actions` 和 `/hub` 分流，没接入的不该被转发。
#[test]
fn global_gateway_routes_only_what_opted_in() {
    let env = Env::new();
    let service_port = free_port();
    let actions_port = free_port();
    let gateway_port = free_port();
    let id = env.json(&["--json", "add", ".", "--name", "gw"])[0]["id"]
        .as_str()
        .expect("project id")
        .to_string();
    env.ok(&["set", "gw", &format!("actions.port={actions_port}")]);
    env.ok(&[
        "upgrade",
        "--port",
        &service_port.to_string(),
        "--auth",
        "noauth",
    ]);
    env.ok(&["start"]);

    env.ok(&[
        "gateway",
        "set",
        "--enabled",
        "true",
        "--port",
        &gateway_port.to_string(),
        // 公网地址在测试里填一个假的就够：要验的是 gld 拼给用户的路径对不对，
        // 不是这个域名真的能解析。
        "--public-url",
        "https://gw.example.com",
    ]);
    env.ok(&["gateway", "start"]);
    assert_eq!(get(gateway_port, "/health").json()["ok"], true);

    // 服务还没接入全局入口：`/hub` 存在，但不该把请求转过去。
    let list_tools = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;
    let not_routed = post_json(gateway_port, "/hub/mcp", list_tools, None);
    assert_eq!(not_routed.status, 404, "{}", not_routed.body);
    assert!(
        not_routed.body.contains("not routed"),
        "拒绝的理由应当是没接入，实际：{}",
        not_routed.body
    );

    // 项目的 Actions 也一样：没接入就不转，理由要分清是"没接入"还是"找不到"。
    let actions_path = format!("/w/{id}/actions/openapi.json");
    env.ok(&["start", "-s", "actions"]);
    let not_routed = get(gateway_port, &actions_path);
    assert_eq!(not_routed.status, 404, "{}", not_routed.body);
    assert!(
        not_routed.body.contains("not routed"),
        "{}",
        not_routed.body
    );

    // 接入后文档里的 servers 必须换成公网路径——否则 GPT 导入的是
    // http://127.0.0.1:<port>，它根本打不到。
    env.ok(&["set", "gw", "actions.global-gateway=true"]);
    let routed = wait_for_get(gateway_port, &actions_path);
    assert_eq!(routed.status, 200, "接入后应当能路由：{}", routed.body);
    assert_eq!(
        routed.json()["servers"][0]["url"],
        format!("https://gw.example.com/w/{id}/actions")
    );
    assert_eq!(
        connect_openapi_url(&env),
        format!("https://gw.example.com/w/{id}/actions/openapi.json")
    );

    // 不存在的项目不能穿透。
    let unknown = get(
        gateway_port,
        "/w/00000000000000000000000000000000/actions/openapi.json",
    );
    assert_eq!(unknown.status, 404, "未知项目必须 404：{}", unknown.body);
    assert!(
        unknown.body.contains("workspace not found"),
        "未知项目的理由应当是找不到，实际：{}",
        unknown.body
    );

    env.ok(&["gateway", "stop"]);
    env.ok(&["stop"]);
}

/// `gateway set --frp-profile` 收的写法要和别处一模一样。
///
/// 用户在 `gld frp add --name 公司` 里给的是名字，`gld share --tunnel frp:公司`
/// 也认名字。网关这边以前只认 32 位 id：同一个名字在一处能用、在另一处报
/// "没有名为「公司」的 FRP 配置"，而报错还建议你去 `gld frp add` 再建一个。
#[test]
fn the_gateway_takes_an_frp_profile_by_name_just_like_a_workspace_does() {
    let env = Env::new();
    env.ok(&[
        "frp",
        "add",
        "--name",
        "公司",
        "--server",
        "frp.example.com",
        "--port",
        "7000",
    ]);
    let id = env.json(&["--json", "frp", "list"])[0]["id"]
        .as_str()
        .expect("frp profile id")
        .to_string();

    // 名称。
    env.ok(&["gateway", "set", "--tunnel", "frp", "--frp-profile", "公司"]);
    assert_eq!(
        gateway_frp_profile(&env),
        id,
        "按名称给的时候，存下来的应当是解析后的 id"
    );

    // id 前缀（≥4 位，和项目 selector 同一个口径）。
    env.ok(&["gateway", "set", "--frp-profile", &id[..4]]);
    assert_eq!(gateway_frp_profile(&env), id);

    // 完整 id 当然也认。
    env.ok(&["gateway", "set", "--frp-profile", &id]);
    assert_eq!(gateway_frp_profile(&env), id);

    // 填错的名字必须当场拒，并且把有哪些可选直接列出来——否则要等到
    // `gateway start` 才发现，那时的报错只说隧道起不来。
    let rejected = env.gld(&["gateway", "set", "--frp-profile", "不存在"]);
    assert!(
        !rejected.status.success(),
        "不存在的 FRP 配置不该被静默接受"
    );
    let message = String::from_utf8_lossy(&rejected.stderr);
    assert!(
        message.contains("没有名为「不存在」"),
        "报错要点名是哪个值不对：{message}"
    );
    assert!(
        message.contains("公司") && message.contains(&id),
        "报错里要能直接看到已有的配置，不用再跑 gld frp list：{message}"
    );
    assert_eq!(
        gateway_frp_profile(&env),
        id,
        "被拒的那次不能把原来的值改掉"
    );

    // 改别的字段不受牵连：命令行是"读旧配置→改给出的项→整体发回"，
    // frp_profile_id 每次都原样带上。要是每次都重新校验，一个已经失效的
    // FRP 配置会连改端口都失败，而用户根本没碰那个字段。
    //
    // 失效是真会发生的：`frp remove` 平时会拦住还被引用的配置（报错里就点名
    // 全局入口），但 `--force` 是它自己给的出路，走完就留下一个悬空引用。
    env.ok(&["frp", "remove", &id, "--force"]);
    env.ok(&["gateway", "set", "--port", "28999"]);
    assert_eq!(
        env.json(&["--json", "gateway", "ls"])["config"]["localPort"],
        28999,
        "一个失效的 frp-profile 不该拖累无关字段"
    );
}

/// 字段名取不到时直接把整段打出来：`unwrap_or_default()` 会把"字段改名了"
/// 变成一句"期望 abc 实际空字符串"，查起来要重跑一遍才知道是断言写错了。
fn gateway_frp_profile(env: &Env) -> String {
    let show = env.json(&["--json", "gateway", "ls"]);
    show["config"]["frpProfileId"]
        .as_str()
        .unwrap_or_else(|| panic!("gateway ls 里没有 config.frpProfileId：{show}"))
        .to_string()
}

fn connect_openapi_url(env: &Env) -> String {
    env.json(&["--json", "ls", "gw"])["actions"]["openapiUrl"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// 改配置到入口按新配置转发之间可能有一小段延迟，等它一会儿。
fn wait_for_get(port: u16, path: &str) -> common::http::Reply {
    let mut last = get(port, path);
    for _ in 0..50 {
        if last.status == 200 {
            return last;
        }
        std::thread::sleep(Duration::from_millis(100));
        last = get(port, path);
    }
    last
}

/// Actions 和本机命令行是两个主体：经 Actions 起的命令，`gld tool call list_runs` 列不到，
/// 反过来也一样；Actions 列得到自己的，给的 output_ref 读得回输出。
///
/// 以前 Actions 调工具不带身份，落到和命令行同一个 `local` 主体上，有了 list_runs 就能直接
/// 列出本机起过的全部命令（独立审查发现）。这条也顺带跑了一遍 Actions 线路上真起进程的工具：
/// 以前只测过 read_file，不经过工具内核里的 block_on。
#[cfg(unix)]
#[test]
fn actions_and_the_local_cli_do_not_see_each_others_runs() {
    use std::os::unix::fs::PermissionsExt;
    let env = Env::new();
    env.write("hello", "#!/bin/sh\necho from-actions\n");
    env.write("local", "#!/bin/sh\necho from-cli\n");
    for name in ["hello", "local"] {
        std::fs::set_permissions(
            env.project.path().join(name),
            std::fs::Permissions::from_mode(0o755),
        )
        .expect("chmod");
    }
    let port = free_port();
    env.ok(&["add", ".", "--name", "act"]);
    env.ok(&["set", "act", &format!("actions.port={port}")]);
    env.ok(&["start", "-s", "actions"]);
    let key = env.json(&[
        "--json",
        "secret",
        "ls",
        "actions_api_key",
        "--reveal",
        "-w",
        "act",
    ])["value"]
        .as_str()
        .expect("actions_api_key")
        .to_string();

    let ran = post_json(
        port,
        "/actions/exec_command",
        r#"{"cmd":"./hello","yield_time_ms":10000}"#,
        Some(&key),
    );
    assert_eq!(ran.status, 200, "经 Actions 跑命令失败：{}", ran.body);
    assert!(ran.body.contains("from-actions"), "{}", ran.body);
    let local = env.json(&["--json", "tool", "call", "exec_command", "cmd=./local"]);
    assert_eq!(local["exit_code"], 0, "{local}");

    let from_cli = env.json(&["--json", "tool", "call", "list_runs"]);
    assert_eq!(
        from_cli["total"], 1,
        "命令行列到了 Actions 的命令：{from_cli}"
    );
    assert_eq!(from_cli["runs"][0]["command"], "./local", "{from_cli}");

    let listed = post_json(port, "/actions/list_runs", "{}", Some(&key));
    assert_eq!(listed.status, 200, "{}", listed.body);
    let listed: serde_json::Value = serde_json::from_str(&listed.body).expect("json");
    let runs = &listed["structured_content"];
    assert_eq!(runs["total"], 1, "Actions 列到了命令行的命令：{runs}");
    assert_eq!(runs["runs"][0]["command"], "./hello", "{runs}");
    let output_ref = runs["runs"][0]["output_refs"]["stdout"]
        .as_str()
        .expect("output_ref");
    let read = post_json(
        port,
        "/actions/read_output",
        &format!(r#"{{"output_ref":"{output_ref}"}}"#),
        Some(&key),
    );
    assert!(read.body.contains("from-actions"), "{}", read.body);

    env.ok(&["stop"]);
}
