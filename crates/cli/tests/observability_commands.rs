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

use common::env::Env;
use common::http::post_json;
use common::service::Service;

/// 补全脚本必须五种 shell 都能生成，而且不依赖守护进程和项目。
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
            script.contains("add") && script.contains("share"),
            "{shell} 的补全脚本里没有子命令"
        );
    }

    // 没有项目、没有守护进程，也不该顺手拉起一个。
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
    let port = env.serve("counted", "noauth").port;

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
        .expect("usage 里没有服务那一行")
}

/// `logs` 要能看到刚发生的那次请求。
///
/// 排障文档第一步就是 `gld logs`。日志文件名或路径改了而这条链路没跟上时，
/// 表现是"日志是空的"——而用户会以为是请求没到。
#[test]
fn logs_show_the_request_that_just_happened() {
    let env = Env::new();
    let port = env.serve("logged", "noauth").port;
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

/// 被鉴权挡下的请求也得在 `gld logs` 里留一行，但凭据本身不能进日志。
///
/// 客户端连不上时第一个要分清的是：请求根本没到（隧道 / 地址的问题），还是到了
/// 但凭据不对。以前 401 在记日志之前就返回了，两种情况日志都是一片空白。
/// 日志常被整段贴出去求助，所以连错的 token 也不记——错的往往只差一两个字符。
#[test]
fn logs_show_requests_rejected_by_auth_without_the_credential() {
    let env = Env::new();
    let port = env.serve("guarded", "bearer").port;
    let body = r#"{"jsonrpc":"2.0","id":7,"method":"tools/list"}"#;
    let wrong = "almost-the-right-token-0123456789";
    assert_eq!(post_json(port, "/mcp", body, None).status, 401);
    assert_eq!(post_json(port, "/mcp", body, Some(wrong)).status, 401);

    let logs = env.ok(&["logs", "-n", "50"]);
    assert!(
        logs.contains("[auth] rejected status=401 credential=missing"),
        "没带凭据的请求没记下来：{logs}"
    );
    assert!(
        logs.contains("[auth] rejected status=401 credential=rejected"),
        "凭据错误的请求没记下来：{logs}"
    );
    assert!(!logs.contains(wrong), "错的 token 进了日志：{logs}");
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
    env.serve("proxied", "noauth");

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
/// 交叉验证的对象是服务 `workspace_context` 工具给这个项目的说明和 Skill 目录：
/// 那是真正递给模型的文本（模型第一次进一个项目前先调它），不是另一处推算出来的数字。
#[test]
fn context_marks_what_is_actually_injected() {
    let env = Env::new();
    // 两份内容各带一个独一无二的标记，好在注入的文本里精确找。
    env.write("AGENTS.md", "AGENTS-MARKER-1f2e\n");
    env.write(".cursorrules", "CURSOR-MARKER-9a8b\n");
    // description 写成 `>` 块：gld 原来逐行找 `key:`，这种写法读出来是一个 `>`。
    env.write(
        ".claude/skills/marker-skill/SKILL.md",
        "---\nname: marker-skill\ndescription: >\n  SKILL-MARKER-7c6d handles releases.\n---\nBody.\n",
    );
    // 引号没闭合：读不了。以前静默跳过，现在 context 要说出是哪个文件、为什么。
    env.write(
        ".claude/skills/broken-skill/SKILL.md",
        "---\nname: broken-skill\ndescription: \"never closed\n---\nBody.\n",
    );
    // 人点名才用的：扫得到，但不进给 AI 的目录。
    env.write(
        ".claude/skills/deploy-skill/SKILL.md",
        "---\nname: deploy-skill\ndescription: DEPLOY-MARKER-3b4a ships to prod.\ndisable-model-invocation: true\n---\nBody.\n",
    );
    let service = env.serve("ctx", "noauth");

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
    // compact 以前一条 Skill 都不给，等于默认档下 Skill 整体不可用。现在给一段
    // 有上限的目录（RFC-0003 G2）。
    assert_eq!(
        snapshot["skillsInjected"], true,
        "compact 下 Skill 也进说明"
    );
    // 具体数字不断言：扫到的还包括这台机器上用户级的 skill。
    assert!(
        snapshot["skillsListed"].as_u64().unwrap_or(0) >= 1,
        "至少项目里这一个要进目录：{snapshot}"
    );
    let broken = snapshot["skillsSkipped"]
        .as_array()
        .expect("skillsSkipped 数组")
        .iter()
        .find(|skipped| {
            skipped["path"]
                .as_str()
                .is_some_and(|path| path.contains("broken-skill"))
        })
        .unwrap_or_else(|| panic!("读不了的 skill 没有列出来：{snapshot}"));
    assert!(
        broken["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("frontmatter line 2")),
        "要说出原因：{broken}"
    );
    let human = env.ok(&["context"]);
    assert!(
        human.contains("没收进来") && human.contains("broken-skill"),
        "人看的输出也要列出来：{human}"
    );
    let deploy_line = human
        .lines()
        .find(|line| line.contains("deploy-skill"))
        .unwrap_or_else(|| panic!("点名才用的 skill 也要列出来：{human}"));
    assert!(deploy_line.contains("只在用户点名时用"), "{deploy_line}");

    let text = model_context(&service);
    assert!(
        text.contains("AGENTS-MARKER-1f2e"),
        "报告说注入了 AGENTS.md，实际没进去"
    );
    assert!(
        !text.contains("CURSOR-MARKER-9a8b"),
        "报告说 .cursorrules 不注入，实际进去了"
    );
    assert!(
        text.contains("marker-skill") && text.contains("SKILL-MARKER-7c6d"),
        "compact 下 skill 目录应当进说明，多行 description 也要读对：{text}"
    );
    assert!(
        !text.contains("DEPLOY-MARKER-3b4a") && text.contains("marked disable-model-invocation"),
        "点名才用的 skill 不进目录，只报个数：{text}"
    );

    // 换成 advanced，两份都该进去，报告也要跟着变。
    env.ok(&["set", "tool-profile=advanced"]);
    let snapshot = env.json(&["--json", "context"]);
    assert_eq!(
        injected_paths(&snapshot).len(),
        instruction_paths(&snapshot).len(),
        "advanced 下扫到的都该注入"
    );
    assert_eq!(snapshot["skillsInjected"], true);

    let text = model_context(&service);
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

/// 服务给这个项目的说明和 Skill 目录——模型第一次进项目前调 `workspace_context`
/// 拿到的那段文本。
fn model_context(service: &Service) -> String {
    let context = service.call_tool("workspace_context", serde_json::json!({}));
    assert_eq!(context["ok"], true, "workspace_context 没通：{context}");
    format!(
        "{}\n{}",
        context["instructions"].as_str().unwrap_or_default(),
        context["skills"].as_str().unwrap_or_default()
    )
}

/// `history` 在没有档案时要说清楚下一步，而不是空着。
#[test]
fn history_explains_itself_when_there_is_nothing_yet() {
    let env = Env::new();
    env.ok(&["add", ".", "--name", "hist"]);

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
    env.ok(&["add", ".", "--name", "cfg"]);

    env.ok(&[
        "settings",
        "proxy",
        "--mode",
        "manual",
        "--url",
        "http://127.0.0.1:7890",
    ]);
    let shown = env.json(&["--json", "cfg", "ls"]);
    assert_eq!(shown["proxy"]["mode"], "manual");
    assert_eq!(shown["proxy"]["url"], "http://127.0.0.1:7890");

    // manual 模式没有地址是配不出去的，必须当场拒绝而不是存一个半成品。
    let bad = env.gld(&["settings", "proxy", "--mode", "manual", "--url", ""]);
    assert!(!bad.status.success(), "manual 缺地址被接受了");
    assert!(String::from_utf8_lossy(&bad.stderr).contains("必须填写代理地址"));
    assert_eq!(
        env.json(&["--json", "cfg", "ls"])["proxy"]["url"],
        "http://127.0.0.1:7890",
        "被拒绝的设置不该动到已存的值"
    );

    env.ok(&["settings", "runtime", "--lan-access", "true"]);
    assert_eq!(
        env.json(&["--json", "cfg", "ls"])["runtime"]["allowLanAccess"],
        true
    );
}

/// `tunnel snippet` 给的必须是一份能直接跑的 frpc 配置（项目的 GPT Actions 那条隧道）。
///
/// 以前它只吐 `[[proxies]]` 一段：没有 serverAddr、没有 token，存成 frpc.toml
/// 起不来——而报错来自 frpc（"server address is empty"），不是 gld，
/// 用户根本不会怀疑到这条命令头上。
#[test]
fn tunnel_snippet_is_a_complete_frpc_config_with_the_token_hidden_by_default() {
    let env = Env::new();
    env.ok(&["add", ".", "--name", "snip"]);
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
        "set",
        "actions.tunnel=frp",
        "actions.frp-profile=office",
        "actions.frp-subdomain=demo",
    ]);

    let masked = env.ok(&["tunnel", "snippet", "-s", "actions"]);
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

    let revealed = env.ok(&["tunnel", "snippet", "-s", "actions", "--reveal"]);
    assert!(
        revealed.contains("super-secret-token"),
        "--reveal 没给出真实 token：{revealed}"
    );
}
