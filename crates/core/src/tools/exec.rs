//! 命令工具的应用适配、策略预检与服务器诊断快照。
//!
//! 程序解析和执行资源由 runtime 维护；工具授权与任务编排仍由 dispatch 负责。

use std::sync::Arc;

use serde_json::{json, Value};

use gld_runtime::exec::{native_builtin, probe_program, ExecutableConfig, Execution};

use crate::tools::command_spec::CommandSpec;
use crate::tools::context::ToolContext;
use crate::tools::session::SessionStore;
use crate::tools::workspace::{tool_ok, WorkspaceError};

pub(crate) use gld_runtime::exec::withheld_env_names;
pub use gld_runtime::exec::StdinPlan;

fn execution(ctx: &ToolContext) -> Execution<'_> {
    Execution {
        workspace: &ctx.workspace,
        runtime: &ctx.runtime,
        executables: ExecutableConfig {
            workspace_local_entries: ctx.policy.workspace_local_entries,
            workspace_script_extensions: &ctx.policy.workspace_script_extensions,
        },
        search_path: ctx.executable_path_env(),
        remove_env: &[crate::home::HOME_ENV],
    }
}

pub fn exec_command(
    ctx: &ToolContext,
    sessions: &Arc<SessionStore>,
    args: &Value,
) -> Result<Value, WorkspaceError> {
    gld_runtime::exec::exec_command(&execution(ctx), sessions, args)
}

pub fn exec_health_check(
    ctx: &ToolContext,
    sessions: &Arc<SessionStore>,
) -> Result<Value, WorkspaceError> {
    gld_runtime::exec::exec_health_check(&execution(ctx), sessions)
}

/// 这条命令现在能不能跑——**不跑它**。
///
/// 为什么要有这个工具：拒绝信息里只有一句 `Command is not allowlisted: rg`，
/// 模型看不出是"没装"还是"不许跑"、该换个工具还是该请用户改配置，于是同一条
/// 命令换着花样试五遍（审查 C01、C02、A01）。
///
/// 判定必须和 `exec_command` 用同一条路径：策略校验是同一个
/// `validate_command_for_workspace`，程序解析是同一个 `resolve_program`，
/// 顺序也一样（策略 → workdir → 原生内建 → 解析）。任何一边单独实现一套，
/// 迟早会出现"预检说能跑、真跑被拒"。
///
/// 预检**不产生任何副作用**：不起进程、不跑 `--help`、不登录、不联网、不碰
/// GitHub 和 SSH。只查白名单和文件系统。
pub fn check_command(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    // 两种形式都收，和 exec_command 同一个构造函数。取不出命令来（两个都给、
    // argv 里混了非字符串）本身就是一种判定结果，不能让预检自己先报错退出——
    // 那样模型得到的是"预检坏了"，而不是"你这样传不行"。
    let spec = CommandSpec::from_args(args);
    let cmd = spec
        .as_ref()
        .map(|spec| spec.display.clone())
        .unwrap_or_else(|_| command_echo(args));
    let cmd = cmd.as_str();
    let workdir_raw = args
        .get("workdir")
        .or_else(|| args.get("cwd"))
        .and_then(Value::as_str)
        .unwrap_or(".");

    // 1. 策略。和真实执行调的是同一个函数、同一份参数。
    let policy_error = crate::tools::policy::validate_command_for_workspace(
        args,
        &ctx.policy,
        Some(&ctx.workspace),
    )
    .err();

    // 2. workdir。策略过了之后 exec_command 第一件事就是解析它。
    let workdir = ctx.workspace.resolve_existing(workdir_raw).ok();
    let workdir_problem = match (&policy_error, &workdir) {
        (None, None) => Some("workdir_not_found"),
        (None, Some(resolved)) if !resolved.path.is_dir() => Some("workdir_not_a_directory"),
        _ => None,
    };
    let probe_cwd = workdir
        .as_ref()
        .map(|resolved| resolved.path.clone())
        .unwrap_or_else(|| ctx.workspace.root().to_path_buf());

    // 3. 程序在哪儿。**策略拒了也要查**：拒绝和没装是两回事，把 policy denied
    // 说成"程序不存在"会让模型跑去装一个本来就装着的东西（审查 C02）。
    let parts = spec
        .as_ref()
        .ok()
        .and_then(|spec| spec.resolved_parts().ok())
        .unwrap_or_default();
    let builtin = native_builtin(&parts);
    let program = probe_program(&execution(ctx), &parts, &probe_cwd, builtin);
    let (resolved_by, config_command) = match &policy_error {
        Some(error) => crate::tools::policy::denial_resolution(
            error.reason,
            args,
            &ctx.policy,
            Some(&ctx.workspace),
            parts.first().map(String::as_str),
            &ctx.workspace_name,
        ),
        None => ("other", None),
    };

    let (decision, stage, rule, code, message, suggestion) = match (&policy_error, workdir_problem)
    {
        (Some(error), _) => (
            if error.reason.needs_approval() {
                "needs_approval"
            } else {
                "deny"
            },
            "policy",
            error.reason.slug(),
            Some(error.reason.code()),
            error.message.clone(),
            error.reason.suggestion().to_string(),
        ),
        (None, Some(problem)) => (
            "deny",
            "workdir",
            problem,
            Some("NOT_FOUND"),
            format!("workdir not usable: {workdir_raw}"),
            "workdir 必须是 Workspace 内一个已存在的目录".to_string(),
        ),
        (None, None) => match program["found"].as_bool() {
            Some(false) => (
                "deny",
                "resolve",
                program["reason"].as_str().unwrap_or("program_not_found"),
                program["code"].as_str().map(|_| "COMMAND_REJECTED"),
                program["message"].as_str().unwrap_or_default().to_string(),
                "检查程序名拼写，或确认它在服务端的 PATH 上".to_string(),
            ),
            _ => (
                "allow",
                "none",
                "allowed",
                None,
                String::new(),
                String::new(),
            ),
        },
    };

    let mut result = json!({
        "command": cmd,
        "decision": decision,
        "denied_stage": if decision == "allow" { Value::Null } else { Value::String(stage.into()) },
        "rule": rule,
        "workdir": workdir_raw,
        "resolved_workdir": workdir.as_ref().map(|resolved| resolved.path.display().to_string()),
        "program": program,
        "execution_mode": if builtin.is_some() { "native_builtin" } else { "child_process" },
        "policy": policy_snapshot(ctx),
        "server": server_snapshot(),
        // 预检做了什么、没做什么，明写出来。
        "side_effects": "none",
        "checked": ["policy", "workdir", "program_resolution"],
        "not_checked": [
            "命令自己会不会成功（要真跑才知道）",
            "它启动的子进程会做什么（白名单不是沙箱）"
        ],
        "warnings": []
    });
    if let Some(object) = result.as_object_mut() {
        if decision != "allow" {
            object.insert("code".into(), json!(code));
            object.insert("message".into(), json!(message));
            object.insert("suggestion".into(), json!(suggestion));
            // 语义不动：它说的是"得用户出面"，白名单这类也确实得用户改配置；改成 false，只读它的
            // 调用方会以为不用问用户了。confirm 解不解得开，看 resolved_by（审查 §108）。
            object.insert(
                "needs_user_authorization".into(),
                json!(
                    decision == "needs_approval"
                        || rule == "command_not_allowlisted"
                        || rule == "network_blocked"
                ),
            );
            object.insert("resolved_by".into(), json!(resolved_by));
            if let Some(command) = config_command {
                object.insert("config_command".into(), json!(command));
            }
            let alternatives = alternatives_for(&parts);
            if !alternatives.is_empty() {
                object.insert("alternatives".into(), json!(alternatives));
            }
        }
        if builtin.is_some() {
            object.insert(
                "warnings".into(),
                json!([
                    "这条命令由服务端自己回答，不起子进程；只支持有限语法，复杂参数请改用对应的文件工具"
                ]),
            );
        }
    }
    Ok(tool_ok(result))
}

/// 连命令都取不出来时，结果里的 `command` 显示什么。
///
/// 照着调用方给的原样回显一点，好让人认出自己发的是哪一次；两种形式都没给
/// 就是空串。
fn command_echo(args: &Value) -> String {
    if let Some(cmd) = args.get("cmd").and_then(Value::as_str) {
        return cmd.to_string();
    }
    match args.get("argv") {
        Some(argv) => argv.to_string(),
        None => String::new(),
    }
}

/// 被拒之后还能用什么。只列**已经获准**的工具，不教人绕过拒绝。
fn alternatives_for(parts: &[String]) -> Vec<Value> {
    let Some(name) = parts.first() else {
        return Vec::new();
    };
    let name = name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(name)
        .to_ascii_lowercase();
    let entries: &[(&str, &str)] = match name.as_str() {
        "rg" | "ripgrep" | "ag" | "ack" | "grep" => &[(
            "search_text",
            "工作区文本搜索，带上下文和分页；不支持 rg 的全部参数",
        )],
        "find" | "fd" => &[("list_files", "按 glob 列文件，默认跳过被忽略的目录")],
        "cat" | "head" | "tail" | "less" | "more" => {
            &[("read_file", "按行范围读文件，超长行会截断并说明")]
        }
        "ls" | "dir" | "tree" => &[("list_dir", "列目录，返回可直接交给 read_file 的路径")],
        "sed" | "awk" | "patch" => &[
            ("apply_patch", "改文件用补丁，失败整批不落盘"),
            ("patch_check", "先预检，不落盘"),
        ],
        "git" => &[
            ("git_status", "工作区状态"),
            ("git_diff", "有界 diff"),
            ("git_log", "提交历史"),
        ],
        "ssh" | "scp" | "sftp" => &[(
            "hub",
            "远端机器上的事走已登记的 hub/ccnm 成员，不从这里直连",
        )],
        _ => &[],
    };
    entries
        .iter()
        .map(|(tool, note)| json!({ "tool": tool, "note": note }))
        .collect()
}

/// 当前**运行时真正生效**的那份策略。
pub(crate) fn policy_snapshot(ctx: &ToolContext) -> Value {
    let mut commands = ctx
        .policy
        .allowed_commands
        .iter()
        .cloned()
        .collect::<Vec<_>>();
    commands.sort();
    // 指纹是给"我改完配置生效了没有"用的：改之前改之后各查一次，数不一样就是
    // 生效了。它是**运行时快照**的指纹，不是磁盘上配置文件的版本号——gld 现在
    // 没有配置版本号，不能编一个出来。
    //
    // `PolicySettings` 的每一个字段都要进来。漏一个的后果是"改了配置、指纹没
    // 变"，而这恰恰会被读成"配置没生效"——以前就漏了 `confine_reads`（读能不能
    // 出工作区）、`workspace_script_extensions`、`max_patch_bytes` 和
    // `allowlist_mode` 四个。加字段时这里也要加，否则指纹就是在说谎。
    let fingerprint = {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut extensions = ctx
            .policy
            .workspace_script_extensions
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        // HashSet 的遍历顺序不稳定，先排序，否则同一份配置每次算出的数不一样。
        extensions.sort();
        let mut hasher = DefaultHasher::new();
        commands.hash(&mut hasher);
        ctx.policy.permission_mode.hash(&mut hasher);
        ctx.policy.workspace_local_entries.hash(&mut hasher);
        extensions.hash(&mut hasher);
        ctx.policy.max_patch_bytes.hash(&mut hasher);
        ctx.policy.confine_reads.hash(&mut hasher);
        ctx.policy.allowlist_mode.hash(&mut hasher);
        ctx.policy.command_policy.hash(&mut hasher);
        format!("{:016x}", hasher.finish())
    };
    json!({
        "command_policy": ctx.policy.command_policy,
        "permission_mode": ctx.policy.permission_mode,
        "network_allowed": ctx.policy.network_allowed(),
        "allowlist_mode": ctx.policy.allowlist_mode,
        "allowed_commands": commands,
        "workspace_local_entries": ctx.policy.workspace_local_entries,
        "config_source": ctx.policy.config_source,
        "runtime_fingerprint": fingerprint,
        "fingerprint_note": "运行时生效的策略快照指纹，覆盖 PolicySettings 的全部字段；改配置后重查，数变了才算生效。它不含 tool-profile（那个看 server_info）",
        // 白名单不是沙箱：放行的命令自己能干什么，这里管不着。
        "sandbox_enforced": false,
        "execution_boundary": "policy_only"
    })
}

pub fn server_snapshot() -> Value {
    json!({
        "version": env!("CARGO_PKG_VERSION"),
        "protocol_version": "2025-06-18",
        "tool_api": crate::tools::registry::tool_api_descriptor(),
        // 构建那一刻 HEAD 指着哪个提交（`build.rs` 嵌进来的）。诊断时拿它
        // 和你正在读的源码对一下——同一个版本号能对应几十个提交，光看版本号
        // 判断不出"跑的是不是我改的那份"（跨仓评审 X10）。
        //
        // 从 tarball 编译、`.git` 不在、机器上没装 git，这里就是 null：
        // **说不知道，不拿版本号顶替**。
        "build_commit": crate::BUILD_COMMIT
            .map(Value::from)
            .unwrap_or(Value::Null),
        // 和 ccnm 共用的那几个 crate 实际链进来的是哪一份。
        //
        // 为什么不只报版本号：`Cargo.toml` 里锁的是 **git tag**，而 tag 能被
        // 移动——两次构建都说自己用的 "0.2.1"，里面的代码可以不一样。
        // `Cargo.lock` 里的 `#<sha>` 才是实际链进来的那一份（跨仓评审 X10）。
        "shared_crates": shared_crates()
    })
}

/// `build.rs` 从 `Cargo.lock` 里抄出来的 `name version revision`，逗号分隔。
///
/// 拿不到就是空表——**说不知道，不编一个**。
fn shared_crates() -> Value {
    let Some(raw) = option_env!("GLD_SHARED_CRATES") else {
        return Value::Array(Vec::new());
    };
    let entries = raw
        .split(',')
        .filter_map(|entry| {
            let mut parts = entry.split(' ');
            Some(json!({
                "name": parts.next()?,
                "version": parts.next()?,
                "revision": parts.next()?
            }))
        })
        .collect::<Vec<_>>();
    Value::Array(entries)
}

#[cfg(test)]
mod tests;
