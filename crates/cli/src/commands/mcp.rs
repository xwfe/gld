use gld_core::app::{McpServersDto, McpTestDto};
use gld_daemon::Request;

use super::Ctx;
use crate::cli::McpCmd;
use crate::error::{CliError, CliResult};

pub async fn run(ctx: &mut Ctx, command: McpCmd) -> CliResult {
    match command {
        McpCmd::List => {
            let view: McpServersDto = ctx.backend.call_typed(Request::McpServers).await?;
            if !ctx.out.json_or(&view) {
                print_list(ctx, &view);
            }
            Ok(())
        }
        McpCmd::On { names } => {
            let view: McpServersDto = ctx
                .backend
                .call_typed(Request::SwitchMcpServers {
                    names: names.clone(),
                    on: true,
                    all: false,
                })
                .await?;
            if ctx.out.json_or(&view) {
                return Ok(());
            }
            ctx.out.line(format!(
                "已开：{}。连上服务的 AI 用 list_mcp_tools 看它们的工具、call_mcp_tool 调用。",
                names.join("、")
            ));
            for server in view.servers.iter().filter(|s| names.contains(&s.name)) {
                for note in &server.notes {
                    ctx.out.note(format!("{}：{note}", server.name));
                }
            }
            if view.tool_profile == "read-only" {
                ctx.out.note(
                    "服务的工具集是 read-only，开了也不转：转过去的工具能做什么由 server 决定，只读管不住它。要用就 gld upgrade --tool-profile compact。",
                );
            }
            if view
                .servers
                .iter()
                .any(|s| names.contains(&s.name) && s.kind == "local process")
            {
                ctx.out.note(
                    "它们以你的身份在这台机器上跑，能碰到什么由 server 自己决定，不受任何项目的读写范围约束。",
                );
            }
            ctx.out.note(
                "ChatGPT 这类客户端只在连上时读一次工具表：这是开的第一个的话，要在客户端里刷新一下连接（ChatGPT 是连接器设置里的刷新），才看得见这三个工具。",
            );
            Ok(())
        }
        McpCmd::Off { names, all } => {
            let view: McpServersDto = ctx
                .backend
                .call_typed(Request::SwitchMcpServers {
                    names: names.clone(),
                    on: false,
                    all,
                })
                .await?;
            if !ctx.out.json_or(&view) {
                if all {
                    ctx.out.line("已全关。");
                } else {
                    ctx.out.line(format!("已关：{}。", names.join("、")));
                }
                ctx.out
                    .note("正开着的 server 在服务收到下一个请求时收掉；AI 再调它们会被告知没开。");
            }
            Ok(())
        }
        McpCmd::Test { name } => {
            let tested: McpTestDto = ctx
                .backend
                .call_typed(Request::TestMcpServer { name })
                .await?;
            if ctx.out.json_or(&tested) {
                return if tested.ok {
                    Ok(())
                } else {
                    Err(CliError::new(format!("{} 起不来", tested.name)))
                };
            }
            print_test(ctx, &tested)
        }
    }
}

fn print_list(ctx: &Ctx, view: &McpServersDto) {
    if view.servers.is_empty() {
        ctx.out.line(
            "~/.claude.json 和 ~/.codex/config.toml 里一个 MCP server 都没有（在 Claude Code 里 claude mcp add --scope user，或在 Codex 里 codex mcp add 装）。",
        );
    } else {
        let rows: Vec<Vec<String>> = view
            .servers
            .iter()
            .map(|server| {
                vec![
                    server.name.clone(),
                    if server.on {
                        ctx.out.green("开")
                    } else {
                        ctx.out.dim("关")
                    },
                    kind_label(&server.kind).into(),
                    server.target.clone(),
                    server.source.clone(),
                ]
            })
            .collect();
        ctx.out.table(
            &["名字", "服务转不转", "在哪跑", "跑的是什么", "来自"],
            &rows,
        );
        for server in &view.servers {
            for note in &server.notes {
                ctx.out.note(format!("{}：{note}", server.name));
            }
        }
    }
    if !view.missing.is_empty() {
        ctx.out.note(format!(
            "开着但配置里已经找不到了：{}（gld mcp off {} 把它从名单里去掉）",
            view.missing.join("、"),
            view.missing.join(" ")
        ));
    }
    for problem in &view.problems {
        ctx.out.note(ctx.out.yellow(problem));
    }
    let any_on = view.servers.iter().any(|server| server.on);
    if any_on && view.tool_profile == "read-only" {
        ctx.out.note(
            "服务的工具集是 read-only，开着的也不转（gld upgrade --tool-profile compact 才转）。",
        );
    }
    if !any_on && !view.servers.is_empty() {
        ctx.out
            .note("默认一个都不开。gld mcp on <名字> 开，gld mcp test <名字> 先试起一次。");
    }
}

fn print_test(ctx: &Ctx, tested: &McpTestDto) -> CliResult {
    let seconds = tested.startup_ms as f64 / 1000.0;
    if !tested.ok {
        ctx.out.line(format!(
            "{} {} 起不来（{}：{}，用时 {seconds:.1} 秒）",
            ctx.out.ok_mark(false),
            tested.name,
            kind_label(&tested.kind),
            tested.target
        ));
        if let Some(error) = &tested.error {
            ctx.out.line(format!("  {error}"));
        }
        if tested.kind == "local process" {
            ctx.out.note(
                "找不到程序多半是守护进程的 PATH 里没有它（npx、uvx 常装在 mise / nvm 的目录里）：用 gld cfg runtime --executable-paths 加上它所在的目录，或在配置里写绝对路径。",
            );
        }
        return Err(CliError::new(format!("{} 起不来", tested.name)));
    }
    let who = match (&tested.server_name, &tested.server_version) {
        (Some(name), Some(version)) => format!("{name} {version}"),
        (Some(name), None) => name.clone(),
        _ => "没报名字".into(),
    };
    ctx.out.line(format!(
        "{} {} 起来了：{seconds:.1} 秒，MCP {}，{who}",
        ctx.out.ok_mark(true),
        tested.name,
        tested.protocol.as_deref().unwrap_or("?")
    ));
    ctx.out.line(format!(
        "  工具（{}）：{}",
        tested.tools.len(),
        if tested.tools.is_empty() {
            "无".to_string()
        } else {
            tested.tools.join(", ")
        }
    ));
    if !tested.filtered_tools.is_empty() {
        ctx.out.line(format!(
            "  配置里不放的（{}）：{}",
            tested.filtered_tools.len(),
            tested.filtered_tools.join(", ")
        ));
    }
    if tested.instructions_bytes > 0 {
        ctx.out.line(format!(
            "  它给 AI 的说明 {} 字节（AI 调 list_mcp_tools 时拿到）",
            tested.instructions_bytes
        ));
    }
    Ok(())
}

fn kind_label(kind: &str) -> &'static str {
    match kind {
        "local process" => "本机进程",
        "local URL" => "本机地址",
        _ => "远端地址",
    }
}
