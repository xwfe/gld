//! grant：一把凭据只开几个项目（RFC-0007）。
//!
//! 起真的服务，OAuth 走真的动态注册 → 授权页 → 换令牌，bearer 走真的请求头。范围落在
//! 哪儿（list_workspaces、调用、工具表）和撤销生效没有，都只从服务的回包里看。

mod common;

use common::env::{free_port, Env};
use common::http::{post_form, post_json, Reply};
use common::oauth::connect_a_connector;
use serde_json::{json, Value};

fn rpc(port: u16, token: &str, method: &str, params: Value) -> Reply {
    let body = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
    post_json(port, "/mcp", &body.to_string(), Some(token))
}

/// 调一个工具，返回它的结构化结果。
fn call(port: u16, token: &str, name: &str, arguments: Value) -> Value {
    let reply = rpc(
        port,
        token,
        "tools/call",
        json!({ "name": name, "arguments": arguments }),
    );
    assert_eq!(reply.status, 200, "HTTP 层就失败了：{}", reply.body);
    let text = reply.json()["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("没有工具结果：{}", reply.body))
        .to_string();
    serde_json::from_str(&text).expect("工具结果是 JSON")
}

fn workspace_names(port: u16, token: &str) -> Vec<String> {
    let listed = call(port, token, "list_workspaces", json!({}));
    listed["workspaces"]
        .as_array()
        .unwrap_or_else(|| panic!("{listed}"))
        .iter()
        .map(|item| item["name"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// 两个项目 api、web 都加进服务，按 `auth` 起在空闲端口上。返回端口和 web 的目录。
fn two_projects(env: &Env, auth: &str) -> (u16, tempfile::TempDir) {
    env.write("only-api.txt", "api\n");
    let web = tempfile::tempdir().expect("web dir");
    std::fs::write(web.path().join("only-web.txt"), "web page\n").expect("web file");
    let port = free_port();
    env.ok(&["add", ".", "--name", "api"]);
    env.ok(&[
        "add",
        web.path().to_str().expect("utf-8 path"),
        "--name",
        "web",
    ]);
    env.ok(&["upgrade", "--port", &port.to_string(), "--auth", auth]);
    env.ok(&["start"]);
    (port, web)
}

/// OAuth：授权页填 grant 的口令，拿到的令牌只看得到 api；服务口令照旧全部。
/// `gld grant rm` 之后下一次请求 401、刷新令牌换不来新的；同名重建旧令牌也不复活。
#[test]
fn an_oauth_grant_sees_only_its_projects_until_it_is_revoked() {
    let env = Env::new();
    let (port, _web) = two_projects(&env, "oauth");

    let grant = env.json(&["--json", "grant", "add", "alice", "api"]);
    let password = grant["oauthPassword"].as_str().expect("口令").to_string();
    let (client_id, access, refresh, token_path) = connect_a_connector(port, &password);

    assert_eq!(workspace_names(port, &access), vec!["api"]);
    let refused = call(
        port,
        &access,
        "read_file",
        json!({ "workspace": "web", "path": "only-web.txt" }),
    );
    assert_eq!(
        refused["error"]["code"], "WORKSPACE_NOT_IN_HUB",
        "{refused}"
    );
    assert!(!refused.to_string().contains("web page"), "{refused}");
    let read = call(
        port,
        &access,
        "read_file",
        json!({ "workspace": "api", "path": "only-api.txt" }),
    );
    assert_eq!(read["ok"], true, "{read}");

    let owner_password = env.service_secret("oauth_password");
    let (_, owner, _, _) = connect_a_connector(port, &owner_password);
    let mut all = workspace_names(port, &owner);
    all.sort();
    assert_eq!(all, vec!["api", "web"], "服务口令还是全部项目");

    let removed = env.json(&["--json", "grant", "rm", "alice"]);
    assert_eq!(removed["grant"]["name"], "alice", "{removed}");
    let after = rpc(port, &access, "tools/list", json!({}));
    assert_eq!(after.status, 401, "作废后还能用：{}", after.body);
    let refreshed = post_form(
        port,
        &token_path,
        &[
            ("grant_type", "refresh_token"),
            ("refresh_token", &refresh),
            ("client_id", &client_id),
        ],
    );
    assert_eq!(
        refreshed.status, 400,
        "刷新换出了新令牌：{}",
        refreshed.body
    );
    assert_eq!(refreshed.json()["error"], "invalid_grant");
    assert_eq!(
        rpc(port, &owner, "tools/list", json!({})).status,
        200,
        "作废 grant 连累了服务口令"
    );

    // 同名重建：新的一把是新 id，旧令牌照样不能用。
    env.ok(&["grant", "add", "alice", "api"]);
    assert_eq!(rpc(port, &access, "tools/list", json!({})).status, 401);
}

/// bearer：grant 的令牌只看得到自己的项目；只读的那把工具表里没有写和执行，硬发也不行。
/// 作废一把能写的，它起的后台命令被停掉。
#[test]
fn bearer_grants_are_scoped_read_only_when_asked_and_revocation_stops_their_commands() {
    let env = Env::new();
    let (port, _web) = two_projects(&env, "bearer");

    // 不带 --write 就是只读。
    let viewer = env.json(&["--json", "grant", "add", "viewer", "api"]);
    let viewer = viewer["bearerToken"].as_str().expect("令牌").to_string();
    assert_eq!(workspace_names(port, &viewer), vec!["api"]);

    let tools = rpc(port, &viewer, "tools/list", json!({})).json();
    let names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    assert!(!names.contains(&"exec_command"), "{names:?}");
    assert!(!names.contains(&"apply_patch"), "{names:?}");
    assert!(names.contains(&"read_file"), "{names:?}");
    let refused = rpc(
        port,
        &viewer,
        "tools/call",
        json!({ "name": "exec_command", "arguments": { "workspace": "api", "cmd": "pwd" } }),
    )
    .json();
    assert_eq!(
        refused["error"]["data"]["reason"], "unknown_tool",
        "{refused}"
    );

    let wrong = rpc(port, "not-a-grant-token", "tools/list", json!({}));
    assert_eq!(wrong.status, 401);

    let worker = env.json(&["--json", "grant", "add", "worker", "api", "--write"]);
    let worker = worker["bearerToken"].as_str().expect("令牌").to_string();
    let started = call(
        port,
        &worker,
        "exec_command",
        json!({
            "workspace": "api",
            "cmd": "python3 -c \"import time; time.sleep(30)\"",
            "yield_time_ms": 0,
            "timeout_ms": 60_000
        }),
    );
    assert_eq!(started["ok"], true, "{started}");
    assert_eq!(started["status"], "running", "{started}");

    let removed = env.json(&["--json", "grant", "rm", "worker"]);
    assert_eq!(removed["stoppedCommands"], 1, "{removed}");
    assert_eq!(rpc(port, &worker, "tools/list", json!({})).status, 401);
    assert_eq!(
        workspace_names(port, &viewer),
        vec!["api"],
        "作废一把连累了另一把"
    );

    let listed = env.json(&["--json", "grant", "ls"]);
    assert_eq!(listed.as_array().map(Vec::len), Some(1), "{listed}");
    assert!(
        !listed.to_string().contains(&viewer),
        "不带 --reveal 就给了明文：{listed}"
    );
}

/// noauth 下谁连上都是全权，grant 挡不住任何人：有 grant 时不给改成 noauth，noauth 下不给建。
/// 项目名写错、项目关了 confine-reads 也不建。
#[test]
fn a_grant_needs_real_auth_and_real_projects() {
    let env = Env::new();
    let _ = two_projects(&env, "bearer");
    let stderr =
        |output: &std::process::Output| String::from_utf8_lossy(&output.stderr).to_string();

    let typo = env.gld(&["grant", "add", "alice", "nope"]);
    assert!(!typo.status.success());
    assert!(stderr(&typo).contains("不在服务里"), "{}", stderr(&typo));

    env.ok(&["set", "web", "confine-reads=false"]);
    let open = env.gld(&["grant", "add", "alice", "web"]);
    assert!(!open.status.success());
    assert!(stderr(&open).contains("confine-reads"), "{}", stderr(&open));

    env.ok(&["grant", "add", "alice", "api"]);
    let to_noauth = env.gld(&["upgrade", "--auth", "noauth"]);
    assert!(!to_noauth.status.success(), "有 grant 时改成了 noauth");
    assert!(
        stderr(&to_noauth).contains("grant"),
        "{}",
        stderr(&to_noauth)
    );
    env.ok(&["grant", "rm", "alice"]);

    env.ok(&["upgrade", "--auth", "noauth"]);
    let refused = env.gld(&["grant", "add", "alice", "api"]);
    assert!(!refused.status.success());
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("noauth"),
        "{}",
        String::from_utf8_lossy(&refused.stderr)
    );
    assert_eq!(env.json(&["--json", "grant", "ls"]), json!([]));
}
