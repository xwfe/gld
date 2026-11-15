//! 真实隔离 daemon 的 D18-A 边界：配置可保存，未接入的新授权不能创建托管空间。
//! 新 grant 完成后的 HTTP 创建、留存、配额和跨重启往返属于 D18-B/C 验收；
//! 托管空间内部正向测试继续保留，这里不伪造本机操作员来绕过网络认证。
mod common;

use common::env::{free_port, Env};
use common::http::post_json;
use serde_json::{json, Value};

fn assert_not_ready(port: u16, token: Option<&str>, method: &str, params: Value) {
    let response = post_json(
        port,
        "/mcp",
        &json!({
            "jsonrpc": "2.0", "id": 1, "method": method, "params": params,
        })
        .to_string(),
        token,
    );
    assert_eq!(response.status, 503, "{}", response.body);
    assert_eq!(response.json()["error"], "AUTH_NOT_READY");
    assert_eq!(response.header("cache-control"), Some("no-store"));
    if let Some(token) = token {
        assert!(!response.body.contains(token));
    }
}

fn assert_no_managed_spaces(env: &Env) {
    let state = env.json(&["--json", "status"]);
    assert!(state["service"]["members"].as_array().unwrap().is_empty());
    let data: Value =
        serde_json::from_slice(&std::fs::read(env.home.path().join("data/profiles.json")).unwrap())
            .unwrap();
    assert!(data["profiles"].as_array().unwrap().is_empty());

    match std::fs::read_dir(env.home.path().join("work")) {
        Ok(mut entries) => assert!(
            entries.next().is_none(),
            "no managed work root may be created"
        ),
        Err(error) => assert_eq!(error.kind(), std::io::ErrorKind::NotFound),
    }
    match std::fs::read_dir(env.home.path().join("managed-workspaces")) {
        Ok(mut entries) => assert!(
            !entries.any(|entry| {
                entry
                    .unwrap()
                    .path()
                    .extension()
                    .is_some_and(|extension| extension == "json")
            }),
            "no managed session may be registered"
        ),
        Err(error) => assert_eq!(error.kind(), std::io::ErrorKind::NotFound),
    }
}

#[test]
fn zero_project_daemon_rejects_old_master_before_and_after_restart() {
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
    // 仅隔离夹具的旧服务秘密；证明它不会再得到隐式 master 身份。
    let old_token = env.service_secret("bearer_token");
    for restarted in [false, true] {
        if restarted {
            env.ok(&["daemon", "restart"]);
        }
        let status = env.json(&["--json", "status"]);
        assert_eq!(status["service"]["config"]["workSessions"], true);
        for token in [None, Some(old_token.as_str())] {
            assert_not_ready(port, token, "initialize", json!({}));
            assert_not_ready(port, token, "tools/list", json!({}));
            assert_not_ready(
                port,
                token,
                "tools/call",
                json!({
                    "name": "work_session",
                    "arguments": {
                        "action": "start", "client_request_id": "blocked",
                        "purpose": "must not create a directory",
                    },
                }),
            );
        }
        assert_no_managed_spaces(&env);
    }
    let prune = env.json(&["--json", "prune", "--dry-run"]);
    assert!(prune["leftovers"].as_array().unwrap().is_empty(), "{prune}");
}

#[test]
fn capability_defaults_off_and_noauth_is_always_refused() {
    let env = Env::new();
    let status = env.json(&["--json", "status"]);
    assert_eq!(status["service"]["config"]["workSessions"], false);
    for enabled in ["true", "false"] {
        let refused = env.gld(&["upgrade", "--auth", "noauth", "--work-sessions", enabled]);
        assert!(!refused.status.success());
        let message = format!(
            "{}{}",
            String::from_utf8_lossy(&refused.stdout),
            String::from_utf8_lossy(&refused.stderr)
        );
        assert!(message.contains("AUTH_REQUIRED"), "{message}");
    }
    let status = env.json(&["--json", "status"]);
    assert_eq!(status["service"]["config"]["workSessions"], false);
}

#[test]
fn configured_limits_survive_restart_without_opening_a_master_bypass() {
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
    let old_token = env.service_secret("bearer_token");
    let expected = json!({"maxActive": 1, "maxRecords": 2, "maxBytes": 64});
    let before = env.json(&["--json", "status"]);
    assert_eq!(before["service"]["config"]["workSessionLimits"], expected);
    let invalid = env.gld(&["upgrade", "--work-max-active", "3"]);
    assert!(
        !invalid.status.success(),
        "invalid limits must not be saved"
    );
    env.ok(&["daemon", "restart"]);
    let after = env.json(&["--json", "status"]);
    assert_eq!(after["service"]["config"]["workSessionLimits"], expected);
    assert_not_ready(
        port,
        Some(&old_token),
        "tools/call",
        json!({"name": "work_session", "arguments": {"action": "capacity"}}),
    );
    assert_no_managed_spaces(&env);
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
