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
fn retained_result_survives_expiry_and_a_real_daemon_restart() {
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
    let token = env.service_secret("bearer_token");
    let started = call(
        port,
        &token,
        "work_session",
        json!({"action":"start","client_request_id":"result"}),
    );
    let id = started["workspace"].as_str().unwrap();
    let write = call(
        port,
        &token,
        "apply_patch",
        json!({"workspace":id,"patch":"*** Begin Patch\n*** Add File: result.txt\n+retained\n*** End Patch\n"}),
    );
    assert_eq!(write["ok"], true, "{write}");
    let saved = call(
        port,
        &token,
        "work_session",
        json!({"action":"retain","workspace":id,"keep_paths":["result.txt"]}),
    );
    assert_eq!(saved["ok"], true, "{saved}");
    let artifact = &saved["artifacts"][0]["id"];
    let failed = call(
        port,
        &token,
        "work_session",
        json!({"action":"finish","workspace":id,"keep_paths":[],"outcome":"failed"}),
    );
    assert_eq!(failed["closed"], false, "{failed}");
    // 只调整隔离夹具的时间，不用实际等待一天。
    let record_path = env
        .home
        .path()
        .join("managed-workspaces")
        .join(format!("{id}.json"));
    let mut record: Value = serde_json::from_slice(&std::fs::read(&record_path).unwrap()).unwrap();
    record["expires_at_ms"] = json!(1);
    std::fs::write(&record_path, serde_json::to_vec(&record).unwrap()).unwrap();
    let swept = call(port, &token, "work_session", json!({"action":"sweep"}));
    assert_eq!(swept["removed"], json!([id]), "{swept}");
    assert!(!env.home.path().join("work").join(id).exists());
    assert!(
        !record_path.exists(),
        "closed receipt must leave the active registry"
    );
    env.ok(&["daemon", "restart"]);
    assert_eq!(env.service_secret("bearer_token"), token);
    let read = call(
        port,
        &token,
        "work_session",
        json!({"action":"artifact","artifact_id":artifact,"include_content":true}),
    );
    assert_eq!(read["content"], "retained\n", "{read}");
    let repeated = call(
        port,
        &token,
        "work_session",
        json!({"action":"finish","workspace":id,"keep_paths":[]}),
    );
    assert_eq!(repeated["already_closed"], true, "{repeated}");
    let same_request = call(
        port,
        &token,
        "work_session",
        json!({"action":"start","client_request_id":"result"}),
    );
    assert_eq!(same_request["workspace"], id);
    assert_eq!(same_request["created"], false);
    assert_eq!(same_request["session"]["state"], "closed");
    assert!(!env.home.path().join("work").join(id).exists());
    let prune = env.json(&["--json", "prune", "--dry-run"]);
    assert!(prune["leftovers"].as_array().unwrap().is_empty(), "{prune}");
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

#[test]
fn configured_limits_apply_over_http_and_preserve_archived_retries_after_restart() {
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
        "--work-max-active",
        "1",
        "--work-max-records",
        "2",
        "--work-max-bytes",
        "64",
    ]);
    env.ok(&["start"]);
    let token = env.service_secret("bearer_token");
    let first = call(
        port,
        &token,
        "work_session",
        json!({"action":"start","client_request_id":"one"}),
    );
    assert_eq!(first["ok"], true, "{first}");
    let second_args = json!({"action":"start","client_request_id":"two"});
    assert_eq!(
        call(port, &token, "work_session", second_args.clone())["error"]["code"],
        "WORK_SESSION_LIMIT"
    );
    let id = &first["workspace"];
    assert_eq!(
        call(
            port,
            &token,
            "work_session",
            json!({"action":"finish","workspace":id,"keep_paths":[]})
        )["closed"],
        true
    );
    let second = call(port, &token, "work_session", second_args);
    assert_eq!(second["ok"], true, "{second}");
    let usage = call(port, &token, "work_session", json!({"action":"capacity"}));
    assert_eq!(usage["limits"]["maxActive"], 1);
    assert_eq!(usage["limits"]["maxRecords"], 2);
    assert_eq!(usage["limits"]["maxBytes"], 64);
    assert_eq!(usage["usage"]["closed"], 1);
    env.ok(&["daemon", "restart"]);
    assert_eq!(env.service_secret("bearer_token"), token);
    let repeated = call(
        port,
        &token,
        "work_session",
        json!({"action":"start","client_request_id":"one"}),
    );
    assert_eq!(repeated["workspace"], *id);
    assert_eq!(repeated["session"]["state"], "closed");
    assert_eq!(
        call(
            port,
            &token,
            "work_session",
            json!({"action":"start","client_request_id":"three"})
        )["error"]["code"],
        "WORK_SESSION_LIMIT"
    );
    let invalid = env.gld(&["upgrade", "--work-max-active", "3"]);
    assert!(!invalid.status.success());
    let usage = call(port, &token, "work_session", json!({"action":"capacity"}));
    assert_eq!(
        usage["limits"]["maxActive"], 1,
        "invalid config must not be saved"
    );
}

#[test]
fn startup_cleans_expired_work_without_a_client_request_and_keeps_saved_results() {
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
    let token = env.service_secret("bearer_token");
    let started = call(
        port,
        &token,
        "work_session",
        json!({"action":"start","client_request_id":"startup"}),
    );
    let id = started["workspace"].as_str().unwrap();
    let root = env.home.path().join("work").join(id);
    std::fs::write(root.join("result.txt"), "keep across startup").unwrap();
    let saved = call(
        port,
        &token,
        "work_session",
        json!({"action":"retain","workspace":id,"keep_paths":["result.txt"]}),
    );
    assert_eq!(saved["ok"], true, "{saved}");
    let artifact = saved["artifacts"][0]["id"].clone();
    env.ok(&["daemon", "stop", "--wait", "5"]);
    let path = env
        .home
        .path()
        .join("managed-workspaces")
        .join(format!("{id}.json"));
    let mut record: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    record["expires_at_ms"] = json!(1);
    std::fs::write(path, serde_json::to_vec(&record).unwrap()).unwrap();
    env.ok(&["daemon", "start"]);
    // 不发 start/finish/sweep；文件消失只能来自监听器本次启动的维护。
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while root.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    assert!(
        !root.exists(),
        "startup sweep did not remove the expired fixture"
    );
    let read = call(
        port,
        &token,
        "work_session",
        json!({"action":"artifact","artifact_id":artifact,"include_content":true}),
    );
    assert_eq!(read["content"], "keep across startup", "{read}");
    assert_eq!(env.service_secret("bearer_token"), token);
}
