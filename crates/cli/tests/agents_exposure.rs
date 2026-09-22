//! `~/.agents/mcp.json` 经真实服务生效（toexec RFC-0001）。
//!
//! 单元测试证明了规则本身；这里证明守护进程读的是它自己 HOME 下的那份、
//! 改完不用重启、被关掉的工具调用时说得出是谁关的，以及文件写坏时客户端
//! 看到的是原因而不是"gld 没有工具"。

mod common;

use common::env::Env;
use serde_json::{json, Value};

fn tool_names(env_service: &common::service::Service) -> Vec<String> {
    let reply = env_service.rpc("tools/list", json!({}));
    assert_eq!(reply.status, 200, "{}", reply.body);
    reply.json()["result"]["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("没有工具表：{}", reply.body))
        .iter()
        .filter_map(|tool| tool["name"].as_str().map(str::to_string))
        .collect()
}

fn doctor_row(env: &Env) -> Value {
    let output = env.gld(&["--json", "doctor"]);
    let payload: Value = serde_json::from_slice(&output.stdout).expect("doctor json");
    payload["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["label"] == "暴露规则")
        .cloned()
        .unwrap_or_else(|| panic!("doctor 里没有暴露规则那一行：{payload:#}"))
}

#[test]
fn the_agents_file_narrows_the_service_without_a_restart() {
    let env = Env::new();
    for name in ["deploy", "keep"] {
        env.write(
            &format!(".agents/skills/{name}/SKILL.md"),
            &format!("---\nname: {name}\ndescription: The {name} flow\n---\nSteps.\n"),
        );
    }
    let service = env.serve("exp", "noauth");
    assert!(tool_names(&service).contains(&"exec_command".to_string()));

    // 服务已经在跑：写完文件，下一次请求就按它来。
    env.agents_file(
        r#"{"mcpServers": {"gld": {"disabledTools": ["exec_command"]}},
            "skillOverrides": {"deploy": "off"}}"#,
    );
    let names = tool_names(&service);
    assert!(!names.contains(&"exec_command".to_string()), "{names:?}");
    assert!(names.contains(&"read_file".to_string()), "{names:?}");

    // 按名字硬调：拒掉，并说清是哪条规则。
    let refused = service.call_tool("exec_command", json!({ "cmd": "echo hi" }));
    let text = refused.to_string();
    assert!(text.contains("TOOL_TURNED_OFF"), "{text}");
    assert!(text.contains("mcpServers.gld.disabledTools"), "{text}");

    let skills = service.call_tool("list_skills", json!({})).to_string();
    assert!(skills.contains("keep"), "{skills}");
    assert!(!skills.contains("deploy"), "{skills}");

    let row = doctor_row(&env);
    assert_eq!(row["level"], "ok", "{row}");
    assert!(
        row["detail"].as_str().unwrap().contains("exec_command"),
        "{row}"
    );
    env.ok(&["stop"]);
}

#[test]
fn a_broken_agents_file_is_reported_not_taken_as_no_rules() {
    let env = Env::new();
    let service = env.serve("broken", "noauth");
    env.agents_file(r#"{"mcpServers": {"gld": {"disabledTools": "exec_command"}}}"#);

    let reply = service.rpc("tools/list", json!({}));
    let body = reply.json();
    let message = body["error"]["message"]
        .as_str()
        .unwrap_or_else(|| panic!("该是一个带原因的错误：{}", reply.body));
    assert!(message.contains(".agents/mcp.json"), "{message}");
    assert!(
        message.contains("mcpServers.gld.disabledTools"),
        "{message}"
    );

    let row = doctor_row(&env);
    assert_eq!(row["level"], "fail", "{row}");
    assert!(
        row["detail"].as_str().unwrap().contains("一个工具都不给"),
        "{row}"
    );
    env.ok(&["stop"]);
}
