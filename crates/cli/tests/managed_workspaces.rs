//! 真实隔离守护进程 + HTTP 鉴权，证明无需项目登记也能工作，而非只验证内部函数。
mod common;

use common::env::{free_port, Env};
use common::http::post_json;
use serde_json::{json, Value};

fn rpc(port: u16, token: &str, method: &str, params: Value) -> Value {
    let response = post_json(
        port,
        "/mcp",
        &json!({
            "jsonrpc":"2.0", "id":1, "method":method, "params":params,
        })
        .to_string(),
        Some(token),
    );
    assert_eq!(response.status, 200, "{}", response.body);
    let response = response.json();
    assert!(response["error"].is_null(), "{response}");
    response["result"].clone()
}

fn call(port: u16, token: &str, name: &str, args: Value) -> Value {
    rpc(
        port,
        token,
        "tools/call",
        json!({"name":name,"arguments":args}),
    )["structuredContent"]
        .clone()
}

#[test]
fn zero_project_http_work_survives_restart_and_is_not_pruned() {
    let env = Env::new();
    let port = free_port();
    env.ok(&[
        "upgrade",
        "--port",
        &port.to_string(),
        "--auth",
        "bearer",
        "--work-sessions",
        "true",
    ]);
    env.ok(&["start"]);
    let state = env.json(&["--json", "status"]);
    assert_eq!(
        state["service"]["members"].as_array().unwrap().len(),
        0,
        "start must not register the current directory"
    );
    assert_eq!(state["service"]["config"]["workSessions"], true);
    let token = env.service_secret("bearer_token");
    let tools = rpc(port, &token, "tools/list", json!({}));
    let definition = tools["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "work_session")
        .expect("work_session");
    assert_eq!(definition["annotations"]["readOnlyHint"], false);
    let initialized = rpc(port, &token, "initialize", json!({}));
    assert!(initialized["instructions"]
        .as_str()
        .unwrap()
        .contains("No user projects"));
    let start_args =
        json!({"action":"start","client_request_id":"convert-one","purpose":"test files"});
    let start = call(port, &token, "work_session", start_args.clone());
    assert_eq!(start["ok"], true, "{start}");
    let id = start["workspace"].as_str().unwrap();
    let repeat = call(port, &token, "work_session", start_args);
    assert_eq!(repeat["workspace"], id);
    assert_eq!(repeat["created"], false);
    let patch = call(
        port,
        &token,
        "apply_patch",
        json!({"workspace":id,
        "patch":"*** Begin Patch\n*** Add File: result.txt\n+verified content\n*** End Patch\n"}),
    );
    assert_eq!(patch["ok"], true, "{patch}");
    let denied = call(
        port,
        &token,
        "read_file",
        json!({"workspace":id,"path":"../../data/profiles.json"}),
    );
    assert_eq!(denied["ok"], false);

    // 交互管道代替睡眠：命令明确等输入，重启检查一定看得到它。
    let run = call(
        port,
        &token,
        "exec_command",
        json!({"workspace":id,
        "argv":["python3","-c","import sys; print('ready',flush=True); print(sys.stdin.readline())"],
        "stdin_mode":"interactive", "yield_time_ms":1,"timeout_ms":30000}),
    );
    assert_eq!(run["ok"], true, "{run}");
    let refused = env.gld(&["daemon", "restart"]);
    assert!(
        !refused.status.success(),
        "restart must preserve active managed commands"
    );
    let done = call(
        port,
        &token,
        "write_stdin",
        json!({"workspace":id,"session_id":run["session_id"],"chars":"finished\n","yield_time_ms":5000}),
    );
    assert_eq!(done["exit_code"], 0, "{done}");
    let prune = env.json(&["--json", "prune", "--dry-run"]);
    assert!(prune["leftovers"].as_array().unwrap().is_empty(), "{prune}");
    env.ok(&["daemon", "restart"]);
    assert_eq!(env.service_secret("bearer_token"), token);
    let read = call(
        port,
        &token,
        "read_file",
        json!({"workspace":id,"path":"result.txt"}),
    );
    assert_eq!(read["content"], "verified content\n");
    assert_eq!(call(port, &token, "list_workspaces", json!({}))["count"], 0);
    let root = env.home.path().join("work").join(id);
    assert!(root.join("result.txt").exists());
    assert!(!root.join(".git").exists());
}

#[test]
fn capability_defaults_off_and_cannot_be_enabled_with_noauth() {
    let env = Env::new();
    let status = env.json(&["--json", "status"]);
    assert_eq!(status["service"]["config"]["workSessions"], false);
    let refused = env.gld(&["upgrade", "--auth", "noauth", "--work-sessions", "true"]);
    assert!(!refused.status.success());
    let status = env.json(&["--json", "status"]);
    assert_eq!(status["service"]["config"]["workSessions"], false);
}

#[test]
fn prune_reports_corrupt_managed_registry_as_failure_not_empty_success() {
    let env = Env::new();
    std::fs::create_dir_all(env.home.path().join("managed-workspaces")).unwrap();
    std::fs::write(
        env.home.path().join("managed-workspaces/broken.json"),
        "invalid",
    )
    .unwrap();
    let refused = env.gld(&["prune", "--dry-run"]);
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("没有清理"));
}
