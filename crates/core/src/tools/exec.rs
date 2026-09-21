use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

use serde_json::{json, Value};
use tokio::process::Command;

use std::sync::Arc;

use crate::tools::command_spec::CommandSpec;
use crate::tools::context::ToolContext;
use crate::tools::session::{ExecSession, SessionStore};
use crate::tools::workspace::{tool_ok, WorkspaceError};

/// 这条命令的 stdin 怎么办。三种状态，不是两种（方案 E、审查 X03）。
///
/// 以前只有"给了 stdin 就写进去再关"和"什么都不做"两条路，而"什么都不做"
/// 意味着 stdin 一直开着却永远没有数据：`cat`、`grep foo` 这种读标准输入的
/// 命令会一路挂到 `timeout_ms`，看起来像卡死，其实是在等一个永远不来的输入。
#[derive(Debug, Clone)]
pub enum StdinPlan {
    /// 不给输入，起来就关。命令读 stdin 立刻拿到 EOF，该结束就结束。
    CloseImmediately,
    /// 一次性输入，写完就关。
    Once(String),
    /// 保持打开，后面用 `write_stdin` 接着喂。
    ///
    /// **底下是管道，不是 PTY**：认终端才肯交互的程序（`less`、`ssh` 的密码
    /// 提示、带颜色的 REPL）不会因为这个开关就变得可用。真 PTY 是另一件事，
    /// 没做（方案 E：不能只改描述就声称终端程序已兼容）。
    Interactive(String),
}

impl StdinPlan {
    fn from_args(args: &Value) -> Result<Self, WorkspaceError> {
        let text = args
            .get("stdin")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        // `tty` 是老名字，语义一直是"保持 stdin 开着"，不是"给个终端"。
        let interactive = args.get("tty").and_then(Value::as_bool).unwrap_or(false);
        match args.get("stdin_mode").and_then(Value::as_str) {
            None => Ok(if interactive {
                Self::Interactive(text)
            } else if text.is_empty() {
                Self::CloseImmediately
            } else {
                Self::Once(text)
            }),
            Some("close") => Ok(Self::CloseImmediately),
            Some("once") => Ok(Self::Once(text)),
            Some("interactive") => Ok(Self::Interactive(text)),
            Some(other) => Err(WorkspaceError::invalid_argument(format!(
                "Unknown stdin_mode: {other}. Use close, once or interactive"
            ))),
        }
    }

    fn keeps_stdin_open(&self) -> bool {
        matches!(self, Self::Interactive(_))
    }

    fn as_str(&self) -> &'static str {
        match self {
            Self::CloseImmediately => "close",
            Self::Once(_) => "once",
            Self::Interactive(_) => "interactive",
        }
    }
}

/// 按计划把初始输入写进去，该关的关掉。
async fn apply_stdin_plan(
    session: &std::sync::Arc<ExecSession>,
    plan: &StdinPlan,
) -> Result<(), WorkspaceError> {
    use tokio::io::AsyncWriteExt;
    let text = match plan {
        StdinPlan::CloseImmediately => "",
        StdinPlan::Once(text) | StdinPlan::Interactive(text) => text.as_str(),
    };
    let mut stdin_guard = session.stdin.lock().await;
    if let Some(stdin) = stdin_guard.as_mut() {
        if !text.is_empty() {
            stdin
                .write_all(text.as_bytes())
                .await
                .map_err(|_| WorkspaceError::Tool {
                    code: "SESSION_CLOSED",
                    message: "Failed to write stdin.".into(),
                    category: "runtime",
                    retryable: false,
                })?;
            stdin.flush().await.ok();
        }
        if !plan.keeps_stdin_open() {
            let _ = stdin.shutdown().await;
        }
    }
    if !plan.keeps_stdin_open() {
        *stdin_guard = None;
        drop(stdin_guard);
        session.mark_stdin_closed();
    }
    Ok(())
}

/// 取命令本身失败了（`cmd` 与 `argv` 都给、argv 里混了数字、引号没配对……）。
///
/// 这些在策略那一层就会先被拦下并带着 `PolicyReason` 报出去，所以走到执行
/// 路径上的只剩防御性的一份；映射成参数错误即可，不为它再造一个错误码。
fn spec_error(error: crate::tools::policy::PolicyError) -> WorkspaceError {
    WorkspaceError::invalid_argument(error.message)
}

pub fn exec_command(
    ctx: &ToolContext,
    sessions: &Arc<SessionStore>,
    args: &Value,
) -> Result<Value, WorkspaceError> {
    // `cmd`（一行）和 `argv`（逐格）在这里合成同一个东西，后面的解析、执行、
    // 记账都只认它。策略那一层刚刚用同一个 `CommandSpec::from_args` 判过一遍，
    // 所以这里再构造一次不会得出不同的结论——真出错也是它先报。
    let spec = CommandSpec::from_args(args).map_err(spec_error)?;
    let cmd = spec.display.as_str();
    let workdir_raw = args
        .get("workdir")
        .or_else(|| args.get("cwd"))
        .and_then(Value::as_str)
        .unwrap_or(".");
    let workdir = ctx.workspace.resolve_existing(workdir_raw)?;
    if !workdir.path.is_dir() {
        return Err(WorkspaceError::not_a_directory(
            "workdir is not a directory",
        ));
    }
    let filesystem_scope = args
        .get("filesystem_scope")
        .and_then(Value::as_str)
        .unwrap_or("workspace")
        .to_string();
    validate_child_process_scope(ctx, args)?;
    if let Some(result) = run_native_diagnostic(ctx, &spec, &workdir.path)? {
        let mut result = result;
        if let Some(object) = result.as_object_mut() {
            object.insert(
                "filesystem_scope".into(),
                Value::String(filesystem_scope.clone()),
            );
            object.insert("sandbox_enforced".into(), Value::Bool(false));
            object.insert(
                "execution_boundary".into(),
                Value::String("policy_only".into()),
            );
            object.insert("child_process".into(), Value::Bool(false));
            object.insert("transport_ok".into(), Value::Bool(true));
            object.insert("command_ok".into(), Value::Bool(true));
        }
        return Ok(tool_ok(result));
    }
    let timeout_ms = crate::tools::args::bounded(args, "exec_command", "timeout_ms");
    let max_output = crate::tools::args::bounded(args, "exec_command", "max_output_bytes") as usize;
    let yield_ms = crate::tools::args::bounded(args, "exec_command", "yield_time_ms");
    let stdin_plan = StdinPlan::from_args(args)?;

    // **每条命令起来之前都要先拿到工作区的写权**，不管它是同步等还是转后台。
    // 挡的是"一边跑命令一边打补丁"——那种交叉出来的结果没法解释：命令读到的是
    // 半新半旧的文件。一个补丁改三个文件不是原子的（单个文件的替换才是），
    // 正好落在中间起来的命令看到的就是改了一个半的文件树。
    //
    // **占到哪为止：到这次调用返回。** 命令在 yield_time_ms 之内跑完，那就是
    // 全程；没跑完就转后台（拿着 session_id 继续跑到 timeout_ms），写权在返回
    // 那一刻就放了，**后面那段没有互斥**。传 yield_time_ms: 0 的那一路，占的
    // 就只有起进程的这一瞬间。
    //
    // 后台那段为什么不继续占：后台命令没有终点可言，占着写权等于把目录锁到
    // 天亮，`npm run dev` 起来之后谁也别想改代码了。补的是另一条路——
    // 会话快照里的 `workspace_writes_since_start` 告诉命令这边"你跑的这段时间
    // 工作区被改过几次"，`apply_patch` 的 warnings 告诉写的那边"这儿还有几条
    // 命令在跑"。两边都看得见，但谁也不挡谁。
    //
    // 这里不猜命令写不写文件：`cargo build` 写、`ls` 不写，靠命令文本判断只会
    // 漏判，而漏判给人"已经协调了"的错觉。按运行形态一刀切，代价是 `sleep 5`
    // 这种纯等待的命令也占着写权。
    let write_guard = match ctx.runtime.lock_commits() {
        Some(guard) => guard,
        None => return Err(crate::tools::workspace_runtime::write_lock_busy()),
    };

    let result = crate::async_rt::block_on(async {
        run_command(
            ctx,
            sessions,
            &spec,
            &workdir.path,
            Duration::from_millis(timeout_ms),
            Duration::from_millis(yield_ms),
            max_output,
            &stdin_plan,
        )
        .await
    });
    // 到这儿命令要么已经结束（或被 kill），要么转后台了，写权都可以放了。
    // 显式写出来，是因为下面还有一段拼结果的代码，读的人不该去想锁是在哪一行
    // 没的。
    drop(write_guard);

    match result {
        Ok(mut out) => {
            if let Some(object) = out.as_object_mut() {
                object.insert("filesystem_scope".into(), Value::String(filesystem_scope));
                object.insert("sandbox_enforced".into(), Value::Bool(false));
                object.insert(
                    "execution_boundary".into(),
                    Value::String("policy_only".into()),
                );
                object.insert("child_process".into(), Value::Bool(true));
                // stdin 这次是怎么安排的：close（起来就关，命令读到 EOF）、
                // once（写完就关）、interactive（留着，用 write_stdin 接着喂）。
                // `pty` 永远是 false：interactive 底下是管道，认终端的程序
                // 不会因为它变得可用（审查 X03）。
                object.insert(
                    "stdin_mode".into(),
                    Value::String(stdin_plan.as_str().into()),
                );
                object.insert("pty".into(), Value::Bool(false));
            }
            Ok(tool_ok(out))
        }
        Err(error) => match execution_failure_result(&error, cmd, &workdir.path) {
            Some(result) => Ok(tool_ok(result)),
            None => Err(error),
        },
    }
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
    let program = probe_program(ctx, &parts, &probe_cwd, builtin);

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
            object.insert(
                "needs_user_authorization".into(),
                json!(
                    decision == "needs_approval"
                        || rule == "command_not_allowlisted"
                        || rule == "network_blocked"
                ),
            );
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

/// 程序在不在、在哪儿。走的是真实执行那条 `resolve_program`。
fn probe_program(ctx: &ToolContext, parts: &[String], cwd: &Path, builtin: Option<&str>) -> Value {
    let Some(raw) = parts.first() else {
        return json!({
            "requested": Value::Null,
            "found": Value::Null,
            "source": "unknown",
            "note": "命令是空的，没什么可解析"
        });
    };
    if builtin.is_some() {
        return json!({
            "requested": raw,
            "found": true,
            "source": "native_builtin",
            "path": Value::Null,
            "note": "服务端内建，不需要磁盘上的可执行文件"
        });
    }
    let search_path = ctx.executable_path_env();
    match resolve_program(
        raw,
        cwd,
        ctx.workspace.root(),
        &ctx.policy,
        search_path.as_deref(),
    ) {
        Ok(path) => {
            let inside = Path::new(&path).starts_with(ctx.workspace.root());
            json!({
                "requested": raw,
                "found": true,
                "path": path,
                "source": if inside { "workspace_entry" } else { "path" }
            })
        }
        Err(error) => json!({
            "requested": raw,
            "found": false,
            "path": Value::Null,
            "source": if error.code() == "COMMAND_REJECTED" { "not_found" } else { "rejected" },
            "reason": if error.code() == "EXECUTABLE_OUTSIDE_WORKSPACE" {
                "executable_outside_workspace"
            } else {
                "program_not_found"
            },
            "code": error.code(),
            "message": error.message()
        }),
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
        format!("{:016x}", hasher.finish())
    };
    json!({
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

fn server_snapshot() -> Value {
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
        "build_commit": option_env!("GLD_BUILD_COMMIT")
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

fn validate_child_process_scope(_ctx: &ToolContext, args: &Value) -> Result<(), WorkspaceError> {
    let scope = args
        .get("filesystem_scope")
        .and_then(Value::as_str)
        .unwrap_or("workspace");
    match scope {
        "workspace" => Ok(()),
        "host" => Err(WorkspaceError::ToolDetails {
            code: "EXTERNAL_EXECUTION_NOT_ALLOWED",
            message: "exec_command 只允许在 Workspace 内执行，Workspace 外执行已禁用。".into(),
            category: "permission",
            retryable: false,
            details: json!({
                "stage": "policy",
                "filesystem_scope": "host",
                "sandbox_enforced": false,
                "recoverable": false,
                "suggestion": "将 filesystem_scope 设置为 workspace，并在当前 Workspace 内执行"
            }),
        }),
        _ => Err(WorkspaceError::invalid_argument(
            "filesystem_scope must be workspace",
        )),
    }
}

/// 这条命令服务端自己就能答，不用起子进程。
///
/// 单独一个函数，是因为预检（`check_command`）必须和真实执行看法一致：
/// 说"会起子进程"结果没起，或者反过来，模型据此做的判断就全是错的。
fn native_builtin(parts: &[String]) -> Option<&'static str> {
    match parts.first()?.to_ascii_lowercase().as_str() {
        "pwd" if parts.len() == 1 => Some("pwd"),
        "ls" => Some("ls"),
        "dir" => Some("dir"),
        "which" if parts.len() == 2 => Some("which"),
        "echo" => Some("echo"),
        _ => None,
    }
}

fn run_native_diagnostic(
    ctx: &ToolContext,
    spec: &CommandSpec,
    cwd: &Path,
) -> Result<Option<Value>, WorkspaceError> {
    let cmd = spec.display.as_str();
    let parts = spec.resolved_parts().map_err(spec_error)?;
    if parts.is_empty() {
        return Ok(None);
    }

    let stdout = match native_builtin(&parts) {
        Some("pwd") => Some(format!("{}\n", cwd.display())),
        Some("ls") | Some("dir") => Some(list_directory(ctx, cwd, &parts[1..])?),
        Some("which") => {
            let search_path = ctx.executable_path_env();
            let path = which_on_path(&parts[1], cwd, search_path.as_deref()).ok_or_else(|| {
                WorkspaceError::Tool {
                    code: "COMMAND_NOT_FOUND",
                    message: format!("Program not found on PATH: {}", parts[1]),
                    category: "runtime",
                    retryable: false,
                }
            })?;
            Some(format!("{}\n", path.display()))
        }
        Some("echo") => Some(format!("{}\n", parts[1..].join(" "))),
        _ => None,
    };

    Ok(stdout.map(|stdout| {
        json!({
            "command": cmd,
            "resolved_cwd": cwd.display().to_string(),
            "status": "exited",
            "termination_reason": "exited",
            "recoverable": false,
            "suggestion": "命令已完成",
            "exit_code": 0,
            "stdout": stdout,
            "stderr": "",
            "stdout_truncated": false,
            "stderr_truncated": false,
            "duration_ms": 0,
            "elapsed_ms": 0,
            "execution_mode": "native_builtin",
            "command_runner": "native_builtin",
            // 说清它是什么：服务端自己答的，不是系统上那个同名命令。只说
            // "没起子进程"的话，`ls` 少了 `-la` 的信息会被当成机器出了问题。
            "warnings": ["answered by the server itself, no child process: this is a minimal built-in (pwd / ls / dir / which / echo), not the system command. Options are not accepted — use list_dir, list_files or read_file for anything more."]
        })
    }))
}

fn list_directory(
    ctx: &ToolContext,
    cwd: &Path,
    args: &[String],
) -> Result<String, WorkspaceError> {
    // 服务端内建的 `ls` 是个极简实现，不是系统 ls。带 flag 来的多半以为它是，
    // 而 `-la` 会被当成目录名，报出来的是"路径不存在"——看着像目录没了
    // （方案 B 的原生 ls 一条）。直接说清楚它是什么、该用哪个工具。
    if let Some(flag) = args.iter().find(|arg| arg.starts_with('-')) {
        return Err(WorkspaceError::invalid_argument(format!(
            "原生 ls/dir 不接受选项（{flag}）：它是服务端内建的目录列表，只支持 `ls [目录]`。要大小、时间、类型用 list_dir，要按 glob 找文件用 list_files"
        )));
    }
    let target = match args {
        [] => cwd.to_path_buf(),
        // 相对路径按 **workdir** 解析，不是工作区根：`workdir=crates` 时
        // `ls src` 指的是 `crates/src`，跟在真实 shell 里输入它的结果一致。
        [path] => ctx.workspace.resolve_existing_at(cwd, path)?.path,
        _ => {
            return Err(WorkspaceError::invalid_argument(
                "ls/dir accepts at most one directory path",
            ))
        }
    };
    if !target.is_dir() {
        return Err(WorkspaceError::not_a_directory(
            "ls/dir target is not a directory",
        ));
    }

    let mut entries = std::fs::read_dir(target)
        .map_err(|error| WorkspaceError::ToolDetails {
            code: "DIRECTORY_READ_FAILED",
            message: format!("Failed to read directory: {error}"),
            category: "runtime",
            retryable: true,
            details: json!({
                "stage": "native_builtin",
                "reason": "directory_read_failed",
                "retryable": true
            }),
        })?
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    entries.sort_unstable();
    Ok(if entries.is_empty() {
        String::new()
    } else {
        format!("{}\n", entries.join("\n"))
    })
}

#[allow(clippy::too_many_arguments)]
async fn run_command(
    ctx: &ToolContext,
    sessions: &Arc<SessionStore>,
    spec: &CommandSpec,
    cwd: &Path,
    limit: Duration,
    yield_time: Duration,
    max_output: usize,
    stdin_plan: &StdinPlan,
) -> Result<Value, WorkspaceError> {
    let tty = stdin_plan.keeps_stdin_open();
    let cmd = spec.display.as_str();
    let search_path = ctx.executable_path_env();
    let (program, args) = parse_and_resolve(
        spec,
        cwd,
        ctx.workspace.root(),
        &ctx.policy,
        search_path.as_deref(),
    )?;
    let start = Instant::now();

    let mut command = command_for_program(&program, &args);
    if let Some(path) = search_path.as_ref() {
        command.env("PATH", path);
    }
    command
        .current_dir(platform_command_path(cwd))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    #[cfg(windows)]
    command
        .env("PYTHONUTF8", "1")
        .env("PYTHONIOENCODING", "utf-8")
        .env("PYTHONLEGACYWINDOWSSTDIO", "0");

    // Its own process group, so a timeout or kill_session reaches whatever
    // the command started in the background too, not only the command.
    #[cfg(unix)]
    command.process_group(0);

    let child = command.spawn().map_err(|e| WorkspaceError::ToolDetails {
        code: "COMMAND_SPAWN_FAILED",
        message: format!("Failed to start command: {e}"),
        category: "runtime",
        retryable: true,
        details: json!({
            "termination_reason": "spawn_failed",
            "recoverable": true,
            "suggestion": "检查命令路径、权限和运行时环境后重试"
        }),
    })?;

    let session = sessions.insert(ExecSession::new_with_mode(
        child,
        tty,
        cmd.to_string(),
        ctx.runtime.write_counter(),
    ));
    session.spawn_readers().await;
    let deadline = start + limit;

    // **stdin 先安排好，再考虑要不要立刻返回。**顺序反过来的后果是
    // `yield_time_ms: 0` 那一路把初始输入整个丢掉：命令拿到一个开着、却永远
    // 不会有数据的 stdin，于是挂到超时——而调用方明明传了 stdin（审查 X03、
    // 方案 E "先设置输入/EOF 语义，再进行 yield"）。
    apply_stdin_plan(&session, stdin_plan).await?;

    if yield_time.is_zero() {
        let snapshot = session.snapshot(max_output);
        spawn_timeout_monitor(sessions.clone(), session.clone(), deadline);
        return Ok(merge_exec_result(snapshot, start, cmd, cwd, true));
    }

    loop {
        session.refresh_status().await;
        if session.has_exited() {
            session.wait_for_readers().await;
            let snapshot = session.snapshot(max_output);
            // The result hands out output_refs (and says when it cut the
            // output), so they must stay readable for a while, like after a
            // timeout; removing the session here made every one of them
            // SESSION_NOT_FOUND.
            schedule_session_eviction(sessions.clone(), session.session_id.clone());
            return Ok(merge_exec_result(snapshot, start, cmd, cwd, false));
        }
        if !tty && Instant::now() >= deadline {
            session.mark_termination_reason("timeout");
            session.kill_and_wait().await;
            session.refresh_status().await;
            session.wait_for_readers().await;
            let snapshot = session.snapshot(max_output);
            // Snapshot is embedded; schedule eviction so abandoned timeouts do not linger.
            schedule_session_eviction(sessions.clone(), session.session_id.clone());
            return Err(WorkspaceError::ToolDetails {
                code: "TIMEOUT",
                message: "Command timed out.".into(),
                category: "runtime",
                retryable: true,
                details: json!({
                    "termination_reason": "timeout",
                    "recoverable": true,
                    "suggestion": "读取 output_refs，调整 timeout_ms 后重试",
                    "session": snapshot
                }),
            });
        }
        if Instant::now() - start >= yield_time || tty {
            let snapshot = session.snapshot(max_output);
            spawn_timeout_monitor(sessions.clone(), session.clone(), deadline);
            return Ok(merge_exec_result(snapshot, start, cmd, cwd, true));
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn spawn_timeout_monitor(
    sessions: Arc<SessionStore>,
    session: Arc<ExecSession>,
    deadline: Instant,
) {
    crate::async_rt::spawn(async move {
        let remaining = deadline.saturating_duration_since(Instant::now());
        tokio::time::sleep(remaining).await;
        session.refresh_status().await;
        if !session.has_exited() {
            session.mark_termination_reason("timeout");
            session.kill_and_wait().await;
            session.refresh_status().await;
            session.wait_for_readers().await;
        }
        // Keep the session briefly so clients can still read_output / probe status.
        schedule_session_eviction(sessions, session.session_id.clone());
    });
}

/// 到保留期就把会话从表里去掉，把那两个 1 MiB 的缓冲还回去。
///
/// **判据不在这里**，在 `SessionStore::sweep`：这个定时器只是让内存早点还，
/// 睡过头或者根本没跑（进程被 SIGKILL）也不影响正确性，下一次 get / insert
/// 会扫到它。保留期统一从进程结束那一刻算（审查 X02）。
fn schedule_session_eviction(sessions: Arc<SessionStore>, session_id: String) {
    crate::async_rt::spawn(async move {
        tokio::time::sleep(sessions.retention()).await;
        sessions.remove(&session_id);
    });
}

/// 探针命令记在**调用方自己的**会话表里：它跟普通命令一样会起进程，超时时
/// 也一样留下一条可读的会话，只有主人看得见。
pub fn exec_health_check(
    ctx: &ToolContext,
    sessions: &Arc<SessionStore>,
) -> Result<Value, WorkspaceError> {
    let start = Instant::now();
    let cwd = ctx.workspace.root().to_path_buf();
    #[cfg(windows)]
    let probe = CommandSpec::internal(vec![
        "cmd.exe",
        "/d",
        "/c",
        "echo exec-health && echo exec-health-stderr 1>&2",
    ]);
    #[cfg(not(windows))]
    let probe = CommandSpec::internal(vec![
        "sh",
        "-c",
        "printf exec-health; printf exec-health-stderr >&2",
    ]);

    let result = crate::async_rt::block_on(run_command(
        ctx,
        sessions,
        &probe,
        &cwd,
        Duration::from_secs(5),
        Duration::from_secs(5),
        16_384,
        &StdinPlan::CloseImmediately,
    ));

    let mut response = json!({
        "worker": {"alive": true},
        "session_create": false,
        "command_run": false,
        "stdout_capture": false,
        "stderr_capture": false,
        "duration_ms": start.elapsed().as_millis(),
        "next_actions": []
    });

    match result {
        Ok(snapshot) => {
            let session_created = snapshot.get("session_id").is_some();
            let command_run = snapshot.get("exit_code").and_then(Value::as_i64) == Some(0);
            let stdout_capture = snapshot
                .get("stdout")
                .and_then(Value::as_str)
                .is_some_and(|value| value.contains("exec-health"));
            let stderr_capture = snapshot
                .get("stderr")
                .and_then(Value::as_str)
                .is_some_and(|value| value.contains("exec-health-stderr"));
            let healthy = session_created && command_run && stdout_capture && stderr_capture;
            response["session_create"] = Value::Bool(session_created);
            response["command_run"] = Value::Bool(command_run);
            response["stdout_capture"] = Value::Bool(stdout_capture);
            response["stderr_capture"] = Value::Bool(stderr_capture);
            response["status"] = Value::String(if healthy { "success" } else { "error" }.into());
            response["summary"] = Value::String(if healthy {
                "exec worker、session、命令执行和 stdout/stderr 捕获均正常".into()
            } else {
                "exec health check 未通过，请查看 probe 结果".into()
            });
            response["probe"] = snapshot;
            if !healthy {
                response["next_actions"] = json!(["检查 exec worker 日志", "重启运行时"]);
            }
        }
        Err(error) => {
            response["status"] = Value::String("error".into());
            response["summary"] = Value::String("exec session 创建或探针执行失败".into());
            response["error"] = error.to_error_value();
            response["next_actions"] = json!(["检查 exec worker 日志", "重启运行时"]);
        }
    }
    response["duration_ms"] = json!(start.elapsed().as_millis());
    Ok(tool_ok(response))
}

fn execution_failure_result(error: &WorkspaceError, command: &str, cwd: &Path) -> Option<Value> {
    let code = match &error {
        WorkspaceError::Tool { code, .. } | WorkspaceError::ToolDetails { code, .. } => *code,
    };
    if !matches!(
        code,
        "COMMAND_REJECTED" | "COMMAND_SPAWN_FAILED" | "TIMEOUT"
    ) {
        return None;
    }

    let error_value = error.to_error_value();
    let details = error_value
        .get("details")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let mut result = details.get("session").cloned().unwrap_or_else(|| {
        json!({
            "status": "spawn_failed",
            "termination_reason": "spawn_failed",
            "recoverable": error_value["retryable"].as_bool().unwrap_or(false),
            "exit_code": Value::Null,
            "stdout": "",
            "stderr": "",
            "stdout_truncated": false,
            "stderr_truncated": false
        })
    });
    if let Some(object) = result.as_object_mut() {
        object.insert("command".into(), json!(command));
        object.insert("resolved_cwd".into(), json!(cwd.display().to_string()));
        object.insert("execution_mode".into(), json!("direct"));
        object.insert("filesystem_scope".into(), json!("workspace"));
        object.insert("sandbox_enforced".into(), Value::Bool(false));
        object.insert("execution_boundary".into(), json!("policy_only"));
        object.insert("child_process".into(), Value::Bool(true));
        object.insert("transport_ok".into(), Value::Bool(true));
        object.insert("command_ok".into(), Value::Bool(false));
        object.insert("error".into(), error_value);
        if code == "TIMEOUT" {
            object.insert("termination_reason".into(), json!("timeout"));
        } else {
            object.insert("status".into(), json!("spawn_failed"));
            object.insert("termination_reason".into(), json!("spawn_failed"));
        }
    }
    Some(result)
}

fn merge_exec_result(
    mut snapshot: Value,
    start: Instant,
    command: &str,
    cwd: &Path,
    keep_session: bool,
) -> Value {
    if let Some(obj) = snapshot.as_object_mut() {
        let duration_ms = start.elapsed().as_millis();
        obj.insert("command".into(), json!(command));
        obj.insert("resolved_cwd".into(), json!(cwd.display().to_string()));
        obj.insert("duration_ms".into(), json!(duration_ms));
        obj.insert("elapsed_ms".into(), json!(duration_ms));
        obj.insert("transport_ok".into(), Value::Bool(true));
        let command_ok = match obj
            .get("termination_reason")
            .and_then(Value::as_str)
            .unwrap_or("running")
        {
            "exited" => obj
                .get("exit_code")
                .and_then(Value::as_i64)
                .map(|exit_code| exit_code == 0)
                .or(Some(false)),
            "running" => None,
            _ => Some(false),
        };
        obj.insert(
            "command_ok".into(),
            command_ok.map(Value::Bool).unwrap_or(Value::Null),
        );
        obj.insert("execution_mode".into(), json!("direct"));
        obj.insert(
            "warnings".into(),
            json!(if keep_session {
                vec!["session retained for read_output/write_stdin/kill_session"]
            } else {
                vec!["direct execution without shell"]
            }),
        );
    }
    snapshot
}

fn parse_and_resolve(
    spec: &CommandSpec,
    cwd: &Path,
    workspace_root: &Path,
    policy: &crate::tools::policy::PolicySettings,
    search_path: Option<&OsStr>,
) -> Result<(String, Vec<String>), WorkspaceError> {
    let parts = spec.resolved_parts().map_err(spec_error)?;

    let program = resolve_program(&parts[0], cwd, workspace_root, policy, search_path)?;
    Ok((program, parts[1..].to_vec()))
}

fn resolve_program(
    raw: &str,
    cwd: &Path,
    workspace_root: &Path,
    policy: &crate::tools::policy::PolicySettings,
    search_path: Option<&OsStr>,
) -> Result<String, WorkspaceError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(WorkspaceError::invalid_argument("Empty program"));
    }

    let explicit_path = trimmed.contains(['/', '\\']);
    // **裸名先查 PATH。**以前是反过来的：工作区里放一个叫 `python` 的文件，
    // `python --version` 跑的就是它——而白名单批准的是"python 这个系统命令"。
    // 批的和跑的不是同一个东西，正是方案 B 要消除的那种错位。shell 也不把
    // `.` 放进 PATH，同一个道理。
    //
    // 工作区里的脚本没有因此失去入口：PATH 上查不到的名字仍然回落到工作区
    // （下面那段），写 `./build` 或 `scripts/build.sh` 则一直是明确指路。
    if !explicit_path && !Path::new(trimmed).is_absolute() {
        if let Some(found) = which_on_path(trimmed, cwd, search_path) {
            return Ok(found.to_string_lossy().into_owned());
        }
    }
    let candidate = if Path::new(trimmed).is_absolute() {
        Path::new(trimmed).to_path_buf()
    } else {
        cwd.join(trimmed)
    };
    if candidate.is_file() {
        let resolved = candidate.canonicalize().map_err(|_| WorkspaceError::Tool {
            code: "COMMAND_REJECTED",
            message: format!("Program not found: {trimmed}"),
            category: "runtime",
            retryable: false,
        })?;
        let canonical_workspace =
            workspace_root
                .canonicalize()
                .map_err(|_| WorkspaceError::Tool {
                    code: "COMMAND_REJECTED",
                    message: "Workspace root is unavailable".into(),
                    category: "runtime",
                    retryable: true,
                })?;
        if !resolved.starts_with(&canonical_workspace) {
            return Err(WorkspaceError::Tool {
                code: "EXECUTABLE_OUTSIDE_WORKSPACE",
                message: format!("Workspace 外可执行文件被拒绝: {trimmed}"),
                category: "security",
                retryable: false,
            });
        }
        let extension = resolved
            .extension()
            .and_then(|value| value.to_str())
            .map(|value| format!(".{}", value.to_ascii_lowercase()))
            .unwrap_or_default();
        if policy.workspace_local_entries
            && (extension.is_empty() || policy.workspace_script_extensions.contains(&extension))
        {
            return Ok(resolved.to_string_lossy().into_owned());
        }
        return Err(WorkspaceError::Tool {
            code: "COMMAND_REJECTED",
            message: format!("Workspace 本地入口未获允许: {trimmed}"),
            category: "policy",
            retryable: false,
        });
    }

    if explicit_path {
        return Err(WorkspaceError::Tool {
            code: "COMMAND_REJECTED",
            message: format!("Program not found: {trimmed}"),
            category: "runtime",
            retryable: false,
        });
    }

    which_on_path(trimmed, cwd, search_path)
        .map(|p| p.to_string_lossy().into_owned())
        .ok_or_else(|| WorkspaceError::Tool {
            code: "COMMAND_REJECTED",
            message: format!("Program not found on PATH: {trimmed}"),
            category: "runtime",
            retryable: false,
        })
}

fn which_on_path(
    program: &str,
    cwd: &Path,
    search_path: Option<&OsStr>,
) -> Option<std::path::PathBuf> {
    let Some(paths) = search_path else {
        return which::which(program).ok();
    };

    for directory in std::env::split_paths(paths) {
        let directory = if directory.is_absolute() {
            directory
        } else {
            cwd.join(directory)
        };
        for candidate in executable_candidates(directory.join(program)) {
            if is_executable_file(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

#[cfg(not(windows))]
fn executable_candidates(candidate: PathBuf) -> Vec<PathBuf> {
    vec![candidate]
}

#[cfg(windows)]
fn executable_candidates(candidate: PathBuf) -> Vec<PathBuf> {
    if candidate.extension().is_some() {
        return vec![candidate];
    }
    let extensions = std::env::var_os("PATHEXT")
        .map(|value| {
            value
                .to_string_lossy()
                .split(';')
                .filter_map(|item| {
                    let extension = item.trim().trim_start_matches('.');
                    (!extension.is_empty()).then(|| extension.to_ascii_lowercase())
                })
                .collect::<Vec<_>>()
        })
        .filter(|items| !items.is_empty())
        .unwrap_or_else(|| vec!["com".into(), "exe".into(), "bat".into(), "cmd".into()]);

    extensions
        .into_iter()
        .map(|extension| candidate.with_extension(extension))
        .collect()
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    std::fs::metadata(path)
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(windows)]
fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod tests {
    use super::*;
    use crate::tools::context::ToolContext;
    use crate::tools::dispatch::call_tool;
    use serde_json::json;
    use tempfile::tempdir;

    fn assert_failure_result(error: WorkspaceError, expected_code: &str) {
        let result = execution_failure_result(&error, "missing-command", Path::new("C:/workspace"))
            .expect("应转换为统一执行结果");
        assert_eq!(result["transport_ok"], true);
        assert_eq!(result["command_ok"], false);
        assert_eq!(result["status"], "spawn_failed");
        assert_eq!(result["error"]["code"], expected_code);
    }

    /// A workspace script that starts `sleep` in the background and waits:
    /// `sleep` is a grandchild of gld, not its child.
    #[cfg(unix)]
    fn tree_workspace() -> (tempfile::TempDir, tempfile::TempDir, ToolContext) {
        use std::os::unix::fs::PermissionsExt;
        let workspace = tempdir().expect("workspace");
        let harness = tempdir().expect("harness");
        let script = workspace.path().join("spawn-tree");
        std::fs::write(
            &script,
            "#!/bin/sh\nsleep 30 &\necho $! > grandchild.pid\nwait\n",
        )
        .expect("script");
        let mut permissions = std::fs::metadata(&script).expect("meta").permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script, permissions).expect("chmod");
        let ctx =
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("context");
        (workspace, harness, ctx)
    }

    #[cfg(unix)]
    fn grandchild_gone(workspace: &Path) -> Result<(), String> {
        let pid_file = workspace.join("grandchild.pid");
        let started = Instant::now();
        while !pid_file.exists() && started.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(20));
        }
        let pid: libc::pid_t = std::fs::read_to_string(&pid_file)
            .map_err(|e| format!("script never wrote its pid: {e}"))?
            .trim()
            .parse()
            .map_err(|e| format!("bad pid: {e}"))?;
        // The killed sleep is an orphan until init reaps it.
        let deadline = Instant::now() + Duration::from_secs(2);
        while unsafe { libc::kill(pid, 0) } == 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        if unsafe { libc::kill(pid, 0) } == 0 {
            unsafe { libc::kill(pid, libc::SIGKILL) };
            return Err(format!("grandchild {pid} outlived the command"));
        }
        Ok(())
    }

    /// 输出多到把保留缓冲（1 MiB）挤满时，分页必须能走到头。
    ///
    /// 原来走不到：`offset` 是保留缓冲里的位置，`next_offset` 却拿累计字节数
    /// 判断"还有没有"。读到缓冲末尾之后，每次都回一个空页、`next_offset` 和
    /// 传进去的 offset 一模一样——调用方照着它再读，就是死循环（审查 X01）。
    #[cfg(unix)]
    #[test]
    fn paging_output_bigger_than_the_retained_buffer_terminates() {
        use std::os::unix::fs::PermissionsExt;
        let workspace = tempdir().expect("workspace");
        let harness = tempdir().expect("harness");
        let script = workspace.path().join("flood");
        // 3 MiB，是保留缓冲的三倍。
        std::fs::write(
            &script,
            "#!/bin/sh\nhead -c 3145728 /dev/zero | tr '\\000' a\n",
        )
        .expect("script");
        let mut permissions = std::fs::metadata(&script).expect("meta").permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script, permissions).expect("chmod");
        let ctx =
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("context");

        let output = call_tool(
            &ctx,
            "exec_command",
            &json!({ "cmd": "./flood", "timeout_ms": 30_000, "yield_time_ms": 30_000, "max_output_bytes": 1024 }),
        );
        assert_eq!(output["command_ok"], true, "{output}");
        let stdout_ref = output["output_refs"]["stdout"]
            .as_str()
            .expect("stdout ref")
            .to_string();

        let mut offset = 0u64;
        let mut pages = 0;
        let mut read_bytes = 0usize;
        loop {
            let page = call_tool(
                &ctx,
                "read_output",
                &json!({ "output_ref": stdout_ref, "offset": offset, "limit": 262_144 }),
            );
            assert_eq!(page["ok"], true, "{page}");
            read_bytes += page["content"].as_str().unwrap_or_default().len();
            pages += 1;
            assert!(pages < 64, "分页没走到头，第 {pages} 页还在原地：{page}");
            match page["next_offset"].as_u64() {
                Some(next) => {
                    assert!(next > offset, "next_offset 没有前进：{page}");
                    offset = next;
                }
                None => break,
            }
        }
        // 缓冲只留最后 1 MiB，所以读到的不会是全部 3 MiB——但必须读得到
        // 留下来的那一段，而且要能停。
        assert!(read_bytes >= 1_000_000, "只读到 {read_bytes} 字节");
    }

    /// 缓冲已经把开头挤掉了，还照着旧 offset 来读：不能假装那些字节还在。
    #[cfg(unix)]
    #[test]
    fn an_offset_the_buffer_has_dropped_is_reported_as_a_gap() {
        use std::os::unix::fs::PermissionsExt;
        let workspace = tempdir().expect("workspace");
        let harness = tempdir().expect("harness");
        let script = workspace.path().join("flood");
        std::fs::write(
            &script,
            "#!/bin/sh\nhead -c 3145728 /dev/zero | tr '\\000' a\n",
        )
        .expect("script");
        let mut permissions = std::fs::metadata(&script).expect("meta").permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script, permissions).expect("chmod");
        let ctx =
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("context");
        let output = call_tool(
            &ctx,
            "exec_command",
            &json!({ "cmd": "./flood", "timeout_ms": 30_000, "yield_time_ms": 30_000, "max_output_bytes": 1024 }),
        );
        let stdout_ref = output["output_refs"]["stdout"]
            .as_str()
            .expect("stdout ref")
            .to_string();

        let page = call_tool(
            &ctx,
            "read_output",
            &json!({ "output_ref": stdout_ref, "offset": 0, "limit": 4096 }),
        );
        // 前 2 MiB 已经被挤掉了：说清楚从哪儿开始、丢了多少，而不是把
        // 缓冲里的第一个字节当成第 0 个字节。
        assert!(page["offset"].as_u64().unwrap_or(0) > 0, "{page}");
        assert!(page["dropped_bytes"].as_u64().unwrap_or(0) > 0, "{page}");
        assert!(
            page["warnings"]
                .as_array()
                .map(|w| w.iter().any(|item| item
                    .as_str()
                    .unwrap_or_default()
                    .contains("no longer retained")))
                .unwrap_or(false),
            "{page}"
        );
    }

    /// A command that finishes before yield_time, with more output than
    /// max_output_bytes: the result says "truncated" and hands out
    /// output_refs, so those refs have to be readable.
    #[cfg(unix)]
    #[test]
    fn output_refs_of_a_command_that_finished_inline_can_be_read() {
        use std::os::unix::fs::PermissionsExt;
        let workspace = tempdir().expect("workspace");
        let harness = tempdir().expect("harness");
        let script = workspace.path().join("noisy");
        std::fs::write(
            &script,
            "#!/bin/sh\nhead -c 5000 /dev/zero | tr '\\000' a\necho END\n",
        )
        .expect("script");
        let mut permissions = std::fs::metadata(&script).expect("meta").permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script, permissions).expect("chmod");
        let ctx =
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("context");

        let output = call_tool(
            &ctx,
            "exec_command",
            &json!({ "cmd": "./noisy", "timeout_ms": 10_000, "yield_time_ms": 10_000, "max_output_bytes": 64 }),
        );
        assert_eq!(output["command_ok"], true, "{output}");
        assert_eq!(output["stdout_truncated"], true, "{output}");
        let stdout_ref = output["output_refs"]["stdout"]
            .as_str()
            .expect("stdout ref");
        let read = call_tool(
            &ctx,
            "read_output",
            &json!({ "output_ref": stdout_ref, "limit": 10_000 }),
        );
        assert_eq!(read["total_stream_bytes"], 5004, "{read}");
        assert!(
            read["content"]
                .as_str()
                .unwrap_or_default()
                .ends_with("aEND\n"),
            "{read}"
        );
    }

    /// 同步等结果的命令占着工作区写权：这期间别人拿不到，也就打不了补丁。
    ///
    /// 时序：yield 窗口开了 1.5 秒，400 毫秒的时候命令必然还在里面等，那时
    /// 写权应该是拿不到的。
    #[cfg(unix)]
    #[test]
    fn a_foreground_command_holds_the_write_lock_while_it_waits() {
        let (_workspace, _harness, ctx) = tree_workspace();
        let runtime = Arc::clone(&ctx.runtime);
        let ctx = Arc::new(ctx);
        let runner = {
            let ctx = Arc::clone(&ctx);
            std::thread::spawn(move || {
                call_tool(
                    &ctx,
                    "exec_command",
                    &json!({ "cmd": "./spawn-tree", "timeout_ms": 3_000, "yield_time_ms": 1_500 }),
                )
            })
        };

        std::thread::sleep(Duration::from_millis(400));
        let taken = runtime.lock_commits_within(Duration::from_millis(200));
        let held_by_command = taken.is_none();
        drop(taken);
        let output = runner.join().expect("命令线程");
        assert!(
            held_by_command,
            "命令还在同步等，写权却被别人拿走了——那就是一边跑命令一边改文件：{output}"
        );
    }

    /// 后台命令**起来之前**也要拿到写权：起进程那一瞬间看到的文件树不能是
    /// 别人写到一半的。
    ///
    /// 这条以前断言的是反面（"写权被占着也照起"）。改掉是因为那条规则有个说不
    /// 通的地方：`yield_time_ms: 1` 要等锁、`yield_time_ms: 0` 完全不等，同一条
    /// 命令只因为参数差 1 就走了两套并发语义。而一个补丁改三个文件不是原子的，
    /// 正好落在中间起来的命令看到的就是改了一个半的文件树。
    ///
    /// 拿到之后立刻放：后台那段照旧没有互斥，见
    /// [`a_background_command_does_not_keep_the_write_lock`]。
    #[cfg(unix)]
    #[test]
    fn a_background_command_waits_for_the_write_lock_before_it_starts() {
        let (_workspace, _harness, ctx) = tree_workspace();
        let held = ctx
            .runtime
            .lock_commits_within(Duration::from_millis(50))
            .expect("没人占着却拿不到写权");

        let refused = call_tool(
            &ctx,
            "exec_command",
            &json!({ "cmd": "./spawn-tree", "timeout_ms": 1_000, "yield_time_ms": 0 }),
        );
        assert_eq!(
            refused["error"]["code"], "WORKSPACE_BUSY",
            "有人正在写，后台命令却起来了——它看到的可能是改了一半的文件树：{refused}"
        );

        drop(held);
        let started = call_tool(
            &ctx,
            "exec_command",
            &json!({ "cmd": "./spawn-tree", "timeout_ms": 1_000, "yield_time_ms": 0 }),
        );
        assert!(
            started.get("session_id").is_some(),
            "写权放开了还是起不来：{started}"
        );
    }

    /// 后台命令跑着的时候落了盘，命令那边看得见：快照里的
    /// `workspace_writes_since_start` 从 0 变成 1。
    ///
    /// 后台那段没有互斥，这个数就是事后判断"这条命令的结果还作不作数"的唯一
    /// 依据——它编译的可能是改之前的代码。
    #[cfg(unix)]
    #[test]
    fn a_running_command_sees_that_the_workspace_was_written() {
        let (_workspace, _harness, ctx) = tree_workspace();
        let started = call_tool(
            &ctx,
            "exec_command",
            &json!({ "cmd": "./spawn-tree", "timeout_ms": 5_000, "yield_time_ms": 0 }),
        );
        let session_id = started["session_id"].as_str().expect("session id");
        assert_eq!(
            started["workspace_writes_since_start"], 0,
            "还没人写就报写过了：{started}"
        );

        let patched = call_tool(
            &ctx,
            "apply_patch",
            &json!({ "patch": "*** Begin Patch\n*** Add File: while-running.txt\n+hello\n*** End Patch\n" }),
        );
        assert_eq!(patched["ok"], true, "{patched}");

        let after = call_tool(
            &ctx,
            "read_output",
            &json!({ "output_ref": format!("session:{session_id}:stdout") }),
        );
        assert_eq!(after["ok"], true, "{after}");
        let snapshot = call_tool(
            &ctx,
            "write_stdin",
            &json!({ "session_id": session_id, "chars": "", "yield_time_ms": 0 }),
        );
        assert_eq!(
            snapshot["workspace_writes_since_start"], 1,
            "命令跑着的时候工作区被改了，它却什么都不知道：{snapshot}"
        );
    }

    /// 反过来：命令还在跑的时候打补丁，补丁的结果里得说一声。
    ///
    /// 不是拦下来——后台命令没有终点，拦住等于"有 dev server 就不能改代码"。
    /// 提示要做到的是让模型能判断：自己起的命令直接给 `session_id`，停掉重跑
    /// 还是认下"那条结果基于旧代码"，由它决定。
    #[cfg(unix)]
    #[test]
    fn a_patch_says_which_of_my_commands_are_still_running() {
        let (_workspace, _harness, ctx) = tree_workspace();
        let quiet = call_tool(
            &ctx,
            "apply_patch",
            &json!({ "patch": "*** Begin Patch\n*** Add File: first.txt\n+hello\n*** End Patch\n" }),
        );
        assert_eq!(
            quiet["warnings"],
            json!([]),
            "没有命令在跑却警告了：{quiet}"
        );

        let started = call_tool(
            &ctx,
            "exec_command",
            &json!({ "cmd": "./spawn-tree", "timeout_ms": 5_000, "yield_time_ms": 0 }),
        );
        let session_id = started["session_id"].as_str().expect("session id");

        let noisy = call_tool(
            &ctx,
            "apply_patch",
            &json!({ "patch": "*** Begin Patch\n*** Add File: second.txt\n+hello\n*** End Patch\n" }),
        );
        assert_eq!(noisy["ok"], true, "{noisy}");
        let warnings = noisy["warnings"].to_string();
        assert!(
            warnings.contains(session_id),
            "警告里没给 session_id，模型想停都不知道停哪条：{warnings}"
        );
        assert!(
            warnings.contains("spawn-tree"),
            "警告里没说是哪条命令：{warnings}"
        );
    }

    /// 已经跑完的命令不算"还在跑"。
    ///
    /// 这条钉的是那次主动 refresh：命令自己结束时没人通知服务端，
    /// `has_exited` 要等有人来读或者超时监视器到点才翻。不主动问一遍的话，
    /// 一条早就结束的命令会一直被算进警告里，那条提示就成了狼来了——每次
    /// 打补丁都报，模型很快就不看了。
    #[cfg(unix)]
    #[test]
    fn a_command_that_already_finished_is_not_reported_as_running() {
        let (workspace, _harness, ctx) = tree_workspace();
        let quick = workspace.path().join("quick");
        std::fs::write(&quick, "#!/bin/sh\nexit 0\n").expect("script");
        let mut permissions = std::fs::metadata(&quick).expect("meta").permissions();
        {
            use std::os::unix::fs::PermissionsExt;
            permissions.set_mode(0o755);
        }
        std::fs::set_permissions(&quick, permissions).expect("chmod");

        // yield_time_ms: 0 = 起完就走，谁也没去读过它的状态。
        let started = call_tool(
            &ctx,
            "exec_command",
            &json!({ "cmd": "./quick", "timeout_ms": 5_000, "yield_time_ms": 0 }),
        );
        assert!(started.get("session_id").is_some(), "{started}");
        // 给它一点时间退出。退出这件事本身不会有人来记账。
        std::thread::sleep(Duration::from_millis(300));

        let patched = call_tool(
            &ctx,
            "apply_patch",
            &json!({ "patch": "*** Begin Patch\n*** Add File: after-exit.txt\n+hello\n*** End Patch\n" }),
        );
        assert_eq!(
            patched["warnings"],
            json!([]),
            "命令早就退了还被算成在跑：{patched}"
        );
    }

    /// 起完就放：`npm run dev` 转后台之后，别人照样改得了代码。
    ///
    /// 这是刻意的取舍——后台命令没有终点，一直占着等于把目录锁到天亮。代价
    /// （后台那段没有互斥）由两边的可见性来兜：命令这边看
    /// `workspace_writes_since_start`，写的那边看 `apply_patch` 的 warnings。
    #[cfg(unix)]
    #[test]
    fn a_background_command_does_not_keep_the_write_lock() {
        let (_workspace, _harness, ctx) = tree_workspace();
        let started = call_tool(
            &ctx,
            "exec_command",
            &json!({ "cmd": "./spawn-tree", "timeout_ms": 3_000, "yield_time_ms": 0 }),
        );
        assert!(started.get("session_id").is_some(), "{started}");

        let taken = ctx.runtime.lock_commits_within(Duration::from_millis(200));
        assert!(
            taken.is_some(),
            "命令转后台了还攥着写权，那有个 dev server 在跑就谁也改不了代码"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_timed_out_command_takes_its_background_children_with_it() {
        let (workspace, _harness, ctx) = tree_workspace();
        let output = call_tool(
            &ctx,
            "exec_command",
            &json!({ "cmd": "./spawn-tree", "timeout_ms": 500, "yield_time_ms": 10_000 }),
        );
        assert_eq!(output["termination_reason"], "timeout", "{output}");
        grandchild_gone(workspace.path()).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn kill_session_takes_the_background_children_with_it() {
        let (workspace, _harness, ctx) = tree_workspace();
        let started = call_tool(
            &ctx,
            "exec_command",
            &json!({ "cmd": "./spawn-tree", "timeout_ms": 60_000, "yield_time_ms": 300 }),
        );
        let session_id = started["session_id"]
            .as_str()
            .unwrap_or_else(|| panic!("no session: {started}"))
            .to_string();
        let killed = call_tool(
            &ctx,
            "kill_session",
            &json!({ "session_id": session_id, "wait_ms": 2000 }),
        );
        assert_eq!(killed["killed"], true, "{killed}");
        grandchild_gone(workspace.path()).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn configured_path_resolution_uses_declared_order() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempdir().expect("root");
        let first = root.path().join("first");
        let second = root.path().join("second");
        std::fs::create_dir_all(&first).expect("first dir");
        std::fs::create_dir_all(&second).expect("second dir");
        for directory in [&first, &second] {
            let executable = directory.join("path-probe");
            std::fs::write(&executable, "#!/bin/sh\nexit 0\n").expect("probe");
            let mut permissions = std::fs::metadata(&executable)
                .expect("metadata")
                .permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&executable, permissions).expect("permissions");
        }
        let search_path = std::env::join_paths([&first, &second]).expect("join path");

        let resolved = which_on_path("path-probe", root.path(), Some(search_path.as_os_str()))
            .expect("resolved executable");

        assert_eq!(resolved, first.join("path-probe"));
    }

    #[test]
    fn 程序不存在时返回统一执行结果() {
        assert_failure_result(
            WorkspaceError::Tool {
                code: "COMMAND_REJECTED",
                message: "Program not found on PATH: missing-command".into(),
                category: "runtime",
                retryable: false,
            },
            "COMMAND_REJECTED",
        );
    }

    #[test]
    fn 启动失败时返回统一执行结果() {
        assert_failure_result(
            WorkspaceError::ToolDetails {
                code: "COMMAND_SPAWN_FAILED",
                message: "Failed to start command".into(),
                category: "runtime",
                retryable: true,
                details: json!({"recoverable": true}),
            },
            "COMMAND_SPAWN_FAILED",
        );
    }

    #[test]
    fn resolves_an_arbitrarily_named_workspace_local_entry() {
        let workspace = tempdir().expect("workspace");
        let entry = workspace.path().join("scripts").join("anything.cmd");
        std::fs::create_dir_all(entry.parent().expect("parent")).expect("scripts");
        std::fs::write(&entry, "echo test").expect("entry");
        let resolved = resolve_program(
            "scripts/anything.cmd",
            workspace.path(),
            workspace.path(),
            &crate::tools::policy::PolicySettings::default(),
            None,
        )
        .expect("workspace entry resolves");
        assert_eq!(
            std::path::Path::new(&resolved),
            entry.canonicalize().unwrap()
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_hidden_creation_flags_match_frpc_no_window_pattern() {
        assert_eq!(
            windows_hidden_creation_flags(),
            0x0000_0200 | 0x0800_0000,
            "must keep CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_scripts_use_their_platform_runners() {
        let batch = command_for_program("C:/workspace/run-anything.cmd", &[]);
        assert_eq!(batch.as_std().get_program().to_string_lossy(), "cmd.exe");
        assert!(batch.as_std().get_args().any(|arg| arg == "/c"));
        assert_eq!(
            windows_batch_command_line(
                r"\\?\C:\workspace\Life Brain\run & tooling.cmd",
                &["argument & value".to_string()]
            ),
            r#"call "C:\workspace\Life Brain\run & tooling.cmd" "argument & value""#
        );

        let script = command_for_program("C:/workspace/run-anything.ps1", &[]);
        let runner = script
            .as_std()
            .get_program()
            .to_string_lossy()
            .to_ascii_lowercase();
        assert!(runner.contains("powershell") || runner.contains("pwsh"));
        assert!(script.as_std().get_args().any(|arg| arg == "-File"));

        // Ensure console-subsystem programs (python.exe) also go through the
        // hidden-window flag path; Command does not expose creation_flags for
        // direct assertion, so this only verifies construction still succeeds.
        let python =
            command_for_program("C:/Python312/python.exe", &["-c".into(), "print(1)".into()]);
        assert_eq!(
            python.as_std().get_program().to_string_lossy(),
            "C:/Python312/python.exe"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_workspace_scripts_and_python_unicode_execute_successfully() {
        let workspace = tempdir().expect("workspace");
        let harness = tempdir().expect("harness");
        std::fs::write(
            workspace.path().join("any-name.cmd"),
            "@echo tooling-cmd-ok\r\n",
        )
        .expect("cmd script");
        std::fs::write(
            workspace.path().join("any-name.ps1"),
            "Write-Output 'tooling-powershell-ok'\r\n",
        )
        .expect("powershell script");
        std::fs::write(
            workspace.path().join("workflow_probe.py"),
            "print('workflow-ok')\n",
        )
        .expect("python module");
        let ctx =
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("context");

        for command in [
            "any-name.cmd",
            "any-name.ps1",
            "cmd /c echo tooling-cmd-ok",
            "powershell -NoProfile -Command \"Write-Output tooling-powershell-ok\"",
            "python -c \"print('中文输出正常 ✅')\"",
        ] {
            let output = call_tool(
                &ctx,
                "exec_command",
                &json!({ "cmd": command, "timeout_ms": 10_000, "yield_time_ms": 10_000 }),
            );
            assert_eq!(output["ok"], true, "{command}: {output}");
            assert_eq!(output["command_ok"], true, "{command}: {output}");
        }

        for _ in 0..10 {
            let output = call_tool(
                &ctx,
                "exec_command",
                &json!({ "cmd": "python -m workflow_probe", "timeout_ms": 10_000 }),
            );
            assert_eq!(output["command_ok"], true, "{output}");
            assert!(output["stdout"]
                .as_str()
                .unwrap_or_default()
                .contains("workflow-ok"));
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_batch_scripts_preserve_space_paths_and_arguments() {
        let parent = tempdir().expect("workspace parent");
        let workspace = parent.path().join("Life Brain 中文");
        std::fs::create_dir_all(&workspace).expect("workspace");
        let harness = tempdir().expect("harness");
        let ctx = ToolContext::for_test(workspace.clone(), harness.path().to_path_buf())
            .expect("context");

        for extension in ["cmd", "bat"] {
            let script_name = format!("run & tooling.{extension}");
            std::fs::write(
                workspace.join(&script_name),
                "@echo off\r\nif not \"%~1\"==\"argument & value\" exit /b 7\r\necho tooling-space-path-ok\r\n",
            )
            .expect("batch script");

            let command = format!(r#""{script_name}" "argument & value""#);
            let output = call_tool(
                &ctx,
                "exec_command",
                &json!({ "cmd": command, "timeout_ms": 10_000, "yield_time_ms": 10_000 }),
            );
            assert_eq!(output["command_ok"], true, "{script_name}: {output}");
            let stdout = output["stdout"].as_str().unwrap_or_default();
            assert!(
                stdout.contains("tooling-space-path-ok"),
                "{script_name}: {output}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn unix_workspace_scripts_preserve_space_paths_and_arguments() {
        use std::os::unix::fs::PermissionsExt;

        let parent = tempdir().expect("workspace parent");
        let workspace = parent.path().join("Life Brain 中文");
        std::fs::create_dir_all(&workspace).expect("workspace");
        let harness = tempdir().expect("harness");
        let script_name = "run tooling";
        let script_path = workspace.join(script_name);
        std::fs::write(
            &script_path,
            "#!/bin/sh\nprintf 'tooling-space-path-ok\\n'\nprintf 'argument=[%s]\\n' \"$1\"\n",
        )
        .expect("shell script");
        let mut permissions = std::fs::metadata(&script_path)
            .expect("script metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script_path, permissions).expect("script executable");

        let ctx = ToolContext::for_test(workspace, harness.path().to_path_buf()).expect("context");
        let command = format!(r#""{script_name}" "argument with spaces""#);
        let output = call_tool(
            &ctx,
            "exec_command",
            &json!({ "cmd": command, "timeout_ms": 10_000, "yield_time_ms": 10_000 }),
        );
        assert_eq!(output["command_ok"], true, "{output}");
        let stdout = output["stdout"].as_str().unwrap_or_default();
        assert!(stdout.contains("tooling-space-path-ok"), "{output}");
        assert!(
            stdout.contains("argument=[argument with spaces]"),
            "{output}"
        );
    }
}

#[cfg(windows)]
fn windows_hidden_creation_flags() -> u32 {
    // Match frpc/cloudflared: hide console-subsystem children (python/cmd/powershell)
    // so remote exec_command does not flash a console or steal focus.
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x00000200;
    const CREATE_NO_WINDOW: u32 = 0x08000000;
    CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW
}

fn command_for_program(program: &str, args: &[String]) -> Command {
    #[cfg(windows)]
    {
        let extension = Path::new(program)
            .extension()
            .and_then(|value| value.to_str())
            .map(str::to_ascii_lowercase);
        match extension.as_deref() {
            Some("bat") | Some("cmd") => {
                let mut command = Command::new("cmd.exe");
                command.args(["/d", "/s", "/c"]);
                command
                    .as_std_mut()
                    .raw_arg(windows_batch_command_line(program, args));
                command.creation_flags(windows_hidden_creation_flags());
                return command;
            }
            Some("ps1") => {
                let shell = which::which("pwsh")
                    .or_else(|_| which::which("powershell"))
                    .unwrap_or_else(|_| std::path::PathBuf::from("powershell.exe"));
                let mut command = Command::new(shell);
                command
                    .args([
                        "-NoLogo",
                        "-NoProfile",
                        "-NonInteractive",
                        "-ExecutionPolicy",
                        "Bypass",
                        "-File",
                        windows_command_path(program).as_str(),
                    ])
                    .args(args);
                command.creation_flags(windows_hidden_creation_flags());
                return command;
            }
            _ => {}
        }
    }

    let mut command = Command::new(program);
    command.args(args);
    #[cfg(windows)]
    command.creation_flags(windows_hidden_creation_flags());
    command
}

#[cfg(windows)]
fn windows_batch_command_line(program: &str, args: &[String]) -> String {
    let mut command_line = String::from("call ");
    command_line.push_str(&windows_batch_token(&windows_command_path(program)));
    for arg in args {
        command_line.push(' ');
        command_line.push_str(&windows_batch_token(arg));
    }
    command_line
}

#[cfg(windows)]
fn windows_batch_token(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn platform_command_path(path: &Path) -> std::path::PathBuf {
    #[cfg(windows)]
    {
        std::path::PathBuf::from(windows_command_path(&path.to_string_lossy()))
    }
    #[cfg(not(windows))]
    path.to_path_buf()
}

#[cfg(windows)]
fn windows_command_path(path: &str) -> String {
    path.strip_prefix("\\\\?\\").unwrap_or(path).to_string()
}
