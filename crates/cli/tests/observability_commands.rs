//! 那几个"只是打印点东西"的命令——它们坏了不会有人立刻发现。
//!
//! `health` / `usage` / `context` / `history` / `logs` / `completions` /
//! `settings` / `tunnel snippet` 之前一条集成测试都没有。它们的共同点是
//! **失败时不报错，只是内容不对**：
//!
//! - `context` 列错了 → 你以为某份说明注入给 AI 了，其实没有（或反过来）；
//! - `usage` 一直是 0 → 计数根本没接上，但看起来像"今天没用过"；
//! - `health` 在有代理的环境里把本地探测报成 502 → 排障时被引到完全错的方向；
//! - `snippet` 少半截 → 拿去起 frpc 起不来，而报错来自 frpc 不是 gld；
//! - `completions` 输出空 → 用户 source 一下没反应，也不会报错。

mod common;

use common::env::{free_port, Env};
use common::http::post_json;

/// 补全脚本必须五种 shell 都能生成，而且不依赖守护进程和工作区。
///
/// `completions` 在 Backend 连接之前就被处理掉了——装完 gld 第一件事往往
/// 就是配补全，那时候既没有工作区也没有守护进程。这条路一旦被改坏，
/// 表现是"装好就用不了"，最劝退。
#[test]
fn completions_work_for_every_shell_without_a_workspace_or_daemon() {
    let env = Env::new();

    for shell in ["bash", "zsh", "fish", "powershell", "elvish"] {
        let script = env.ok(&["completions", shell]);
        assert!(
            script.len() > 1000,
            "{shell} 的补全脚本只有 {} 字节，八成是空的",
            script.len()
        );
        assert!(
            script.contains("gld"),
            "{shell} 的补全脚本里没有 gld，补全的是别的命令？"
        );
        // 补全脚本要能补到子命令，否则等于没有。
        assert!(
            script.contains("workspace") && script.contains("expose"),
            "{shell} 的补全脚本里没有子命令"
        );
    }

    // 没有工作区、没有守护进程，也不该顺手拉起一个。
    assert_ne!(
        env.gld(&["daemon", "status"]).status.code(),
        Some(0),
        "生成补全脚本不该把守护进程拉起来"
    );
}

/// `usage` 的计数得真的跟着请求走。
///
/// 一直是 0 和"今天没用过"长得一模一样，没有对照就发现不了计数没接上。
#[test]
fn usage_counts_real_requests() {
    let env = Env::new();
    let port = free_port();
    env.ok(&[
        "ws",
        "add",
        ".",
        "--name",
        "counted",
        "--mcp-port",
        &port.to_string(),
    ]);
    env.ok(&["ws", "set", "auth=noauth"]);
    env.ok(&["start"]);

    let before = mcp_request_count(&env);
    assert_eq!(before, 0, "刚起来就有请求计数？");

    for _ in 0..3 {
        let response = post_json(
            port,
            "/mcp",
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
            None,
        );
        assert_eq!(response.status, 200);
    }

    let after = mcp_request_count(&env);
    assert_eq!(after, 3, "发了 3 次请求，计数是 {after}");
}

fn mcp_request_count(env: &Env) -> u64 {
    env.json(&["--json", "usage"])
        .as_array()
        .and_then(|services| {
            services
                .iter()
                .find(|service| service["service"] == "mcp")
                .and_then(|service| service["requestCount"].as_u64())
        })
        .expect("usage 里没有 mcp 那一行")
}

/// `logs` 要能看到刚发生的那次请求。
///
/// 排障文档第一步就是 `gld logs`。日志文件名或路径改了而这条链路没跟上时，
/// 表现是"日志是空的"——而用户会以为是请求没到。
#[test]
fn logs_show_the_request_that_just_happened() {
    let env = Env::new();
    let port = free_port();
    env.ok(&[
        "ws",
        "add",
        ".",
        "--name",
        "logged",
        "--mcp-port",
        &port.to_string(),
    ]);
    env.ok(&["ws", "set", "auth=noauth"]);
    env.ok(&["start"]);
    post_json(
        port,
        "/mcp",
        r#"{"jsonrpc":"2.0","id":7,"method":"tools/list"}"#,
        None,
    );

    let logs = env.ok(&["logs", "-n", "50"]);
    assert!(
        logs.contains("tools/list"),
        "日志里看不到刚发的那次请求：{logs}"
    );
}

/// `health` 的本地探测必须绕过 HTTP 代理。
///
/// 很多开发环境设了 `HTTP_PROXY`。不绕过的话 curl 会把 127.0.0.1 也发给代理，
/// 本地明明好好的却报 502——排障文档里专门有一行讲这个，因为它把人引到
/// "服务是不是没起来"这个完全错误的方向上去。
///
/// 这里把代理指向一个笃定连不上的端口（9/discard，而且没人在听）。
#[test]
fn health_probes_the_local_endpoint_without_going_through_a_proxy() {
    let env = Env::new();
    let port = free_port();
    env.ok(&[
        "ws",
        "add",
        ".",
        "--name",
        "proxied",
        "--mcp-port",
        &port.to_string(),
    ]);
    env.ok(&["ws", "set", "auth=noauth"]);
    env.ok(&["start"]);

    let output = env.gld_with_env(
        &["health"],
        &[
            ("HTTP_PROXY", "http://127.0.0.1:9"),
            ("HTTPS_PROXY", "http://127.0.0.1:9"),
            ("http_proxy", "http://127.0.0.1:9"),
            ("https_proxy", "http://127.0.0.1:9"),
        ],
    );
    let text = String::from_utf8_lossy(&output.stdout);

    let local = text
        .lines()
        .find(|line| line.contains("本地 /mcp"))
        .unwrap_or_else(|| panic!("health 输出里没有本地 /mcp 那一行：{text}"));
    assert!(
        local.contains("HTTP 200"),
        "设了死代理之后本地探测就不通了，说明没绕过代理：{local}"
    );
}

/// `context` 说会注入的，必须真的注入；说不注入的，必须真的没进去。
///
/// 这条以前是错的：`gld context` 把扫描到的说明全列出来，而默认工具集
/// compact 为了省 token 只注入工作区里的 AGENTS.md 一份，Skill 一个都不带。
/// 有人把规则写进 `.cursorrules`，在 context 里看到它列着，以为生效了——
/// 改半天没反应也想不到是这儿。
///
/// 交叉验证的对象是 MCP `initialize` 回包里的 instructions：那是真正递给
/// 模型的文本，不是另一处推算出来的数字。
#[test]
fn context_marks_what_is_actually_injected() {
    let env = Env::new();
    // 两份内容各带一个独一无二的标记，好在注入的文本里精确找。
    env.write("AGENTS.md", "AGENTS-MARKER-1f2e\n");
    env.write(".cursorrules", "CURSOR-MARKER-9a8b\n");
    let port = free_port();
    env.ok(&[
        "ws",
        "add",
        ".",
        "--name",
        "ctx",
        "--mcp-port",
        &port.to_string(),
    ]);
    env.ok(&["ws", "set", "auth=noauth"]);
    env.ok(&["start"]);

    // 默认 compact：只有 AGENTS.md 进得去。
    let snapshot = env.json(&["--json", "context"]);
    let listed = instruction_paths(&snapshot);
    let injected = injected_paths(&snapshot);
    assert!(
        listed.iter().any(|path| path.contains(".cursorrules")),
        "扫描应当发现 .cursorrules：{listed:?}"
    );
    assert!(
        !injected.iter().any(|path| path.contains(".cursorrules")),
        "compact 下 .cursorrules 不该被标成会注入：{injected:?}"
    );
    assert!(
        injected.iter().any(|path| path.contains("AGENTS.md")),
        "compact 下 AGENTS.md 应当会注入：{injected:?}"
    );
    assert_eq!(snapshot["skillsInjected"], false, "compact 下 Skill 不注入");

    let text = mcp_instructions(port);
    assert!(
        text.contains("AGENTS-MARKER-1f2e"),
        "报告说注入了 AGENTS.md，实际没进去"
    );
    assert!(
        !text.contains("CURSOR-MARKER-9a8b"),
        "报告说 .cursorrules 不注入，实际进去了"
    );

    // 换成 advanced，两份都该进去，报告也要跟着变。
    env.ok(&["ws", "set", "tool-profile=advanced"]);
    let snapshot = env.json(&["--json", "context"]);
    assert_eq!(
        injected_paths(&snapshot).len(),
        instruction_paths(&snapshot).len(),
        "advanced 下扫到的都该注入"
    );
    assert_eq!(snapshot["skillsInjected"], true);

    let text = mcp_instructions(port);
    assert!(
        text.contains("AGENTS-MARKER-1f2e") && text.contains("CURSOR-MARKER-9a8b"),
        "advanced 下两份说明都该注入：{text}"
    );
}

fn instruction_paths(snapshot: &serde_json::Value) -> Vec<String> {
    snapshot["instructions"]
        .as_array()
        .expect("instructions 数组")
        .iter()
        .map(|doc| doc["path"].as_str().unwrap_or_default().to_string())
        .collect()
}

fn injected_paths(snapshot: &serde_json::Value) -> Vec<String> {
    snapshot["injectedInstructionPaths"]
        .as_array()
        .expect("injectedInstructionPaths 数组")
        .iter()
        .map(|path| path.as_str().unwrap_or_default().to_string())
        .collect()
}

/// MCP `initialize` 回包里的 instructions——真正递给模型的那段文本。
fn mcp_instructions(port: u16) -> String {
    let response = post_json(
        port,
        "/mcp",
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"1"}}}"#,
        None,
    );
    assert_eq!(response.status, 200, "initialize 没通");
    let body: serde_json::Value = serde_json::from_str(&response.body).expect("initialize 回包");
    body["result"]["instructions"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// `history` 在没有档案时要说清楚下一步，而不是空着。
#[test]
fn history_explains_itself_when_there_is_nothing_yet() {
    let env = Env::new();
    env.ok(&["ws", "add", ".", "--name", "hist"]);

    let text = env.ok(&["history"]);
    assert!(text.contains("还没有历史会话"), "{text}");
    // 空列表也得告诉人这东西是怎么产生的，否则只能去翻文档。
    assert!(text.contains("docs/history-session"), "{text}");
    assert_eq!(
        env.json(&["--json", "history"])["sessions"]
            .as_array()
            .map(Vec::len),
        Some(0),
        "--json 下也该是干净的空数组"
    );
}

/// 全局设置要能存下来、并且被下一条命令读到。
#[test]
fn global_settings_round_trip_and_reject_nonsense() {
    let env = Env::new();
    env.ok(&["ws", "add", ".", "--name", "cfg"]);

    env.ok(&[
        "settings",
        "proxy",
        "--mode",
        "manual",
        "--url",
        "http://127.0.0.1:7890",
    ]);
    let shown = env.json(&["--json", "settings", "show"]);
    assert_eq!(shown["proxy"]["mode"], "manual");
    assert_eq!(shown["proxy"]["url"], "http://127.0.0.1:7890");

    // manual 模式没有地址是配不出去的，必须当场拒绝而不是存一个半成品。
    let bad = env.gld(&["settings", "proxy", "--mode", "manual", "--url", ""]);
    assert!(!bad.status.success(), "manual 缺地址被接受了");
    assert!(String::from_utf8_lossy(&bad.stderr).contains("必须填写代理地址"));
    assert_eq!(
        env.json(&["--json", "settings", "show"])["proxy"]["url"],
        "http://127.0.0.1:7890",
        "被拒绝的设置不该动到已存的值"
    );

    env.ok(&["settings", "runtime", "--lan-access", "true"]);
    assert_eq!(
        env.json(&["--json", "settings", "show"])["runtime"]["allowLanAccess"],
        true
    );
}

/// `tunnel snippet` 给的必须是一份能直接跑的 frpc 配置。
///
/// 以前它只吐 `[[proxies]]` 一段：没有 serverAddr、没有 token，存成 frpc.toml
/// 起不来——而报错来自 frpc（"server address is empty"），不是 gld，
/// 用户根本不会怀疑到这条命令头上。
#[test]
fn tunnel_snippet_is_a_complete_frpc_config_with_the_token_hidden_by_default() {
    let env = Env::new();
    env.ok(&["ws", "add", ".", "--name", "snip"]);
    env.ok(&[
        "frp",
        "add",
        "--name",
        "office",
        "--server",
        "frp.example.com",
        "--port",
        "7000",
        "--token",
        "super-secret-token",
    ]);
    env.ok(&[
        "ws",
        "set",
        "tunnel=frp",
        "frp-profile=office",
        "frp-subdomain=demo",
    ]);

    let masked = env.ok(&["tunnel", "snippet"]);
    for needed in ["serverAddr", "serverPort", "[[proxies]]", "subdomain"] {
        assert!(masked.contains(needed), "配置里缺 {needed}：{masked}");
    }
    assert!(
        !masked.contains("super-secret-token"),
        "默认输出里带了真实 token：{masked}"
    );
    assert!(
        masked.contains("--reveal"),
        "脱敏了却没说怎么拿真值：{masked}"
    );

    let revealed = env.ok(&["tunnel", "snippet", "--reveal"]);
    assert!(
        revealed.contains("super-secret-token"),
        "--reveal 没给出真实 token：{revealed}"
    );
}
