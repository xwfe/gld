//! 工作区命令执行、进程输出与预检所需的程序解析机制。
//!
//! 应用授权与任务编排由调用侧完成；本层只接收具体资源和解析配置。

use std::collections::HashSet;
use std::ffi::OsString;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::command_spec::CommandSpec;
use crate::session::SessionStore;
use crate::workspace::{tool_ok, Workspace, WorkspaceError};
use crate::workspace_runtime::WorkspaceRuntime;

mod native;
mod platform;
mod process;
mod resolve;

pub use native::native_builtin;
use native::run_native_diagnostic;
use process::run_command;
pub use resolve::probe_program;

/// 程序解析仅需这两个应用已决定的本地入口设置。
#[derive(Clone, Copy)]
pub struct ExecutableConfig<'a> {
    pub workspace_local_entries: bool,
    pub workspace_script_extensions: &'a HashSet<String>,
}

/// 一次执行使用的实际资源与环境；不携带授权、任务或应用服务。
///
/// 调用侧负责策略与 Planning 检查，并传入已经按项目配置合成的 PATH。
/// 会话表单独传入，必须是调用侧按已认证主体取得的那一张。
pub struct Execution<'a> {
    pub workspace: &'a Workspace,
    pub runtime: &'a WorkspaceRuntime,
    pub executables: ExecutableConfig<'a>,
    pub search_path: Option<OsString>,
    /// 应用数据目录等变量不应由项目命令继承；密钥名过滤由 runtime 统一执行。
    pub remove_env: &'a [&'a str],
}

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

/// 应用层先报告策略原因；直接执行入口保留原来的参数错误映射。
fn spec_error(error: crate::command_spec::CommandSpecError) -> WorkspaceError {
    WorkspaceError::invalid_argument(error.message)
}

pub fn exec_command(
    execution: &Execution<'_>,
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
    let workdir = execution.workspace.resolve_existing(workdir_raw)?;
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
    validate_child_process_scope(args)?;
    if let Some(result) = run_native_diagnostic(execution, &spec, &workdir.path)? {
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
    let service_port = crate::service::port_arg(args)?;
    // 服务模式能开到 1 小时，默认半小时；普通命令最多 10 分钟。策略层已经按同一个口径拒过超长的，
    // 这里再夹一次，免得哪条不经策略的路径拿到 1 小时。
    let timeout_ms = match service_port {
        Some(_) => args
            .get("timeout_ms")
            .and_then(Value::as_u64)
            .unwrap_or(crate::service::DEFAULT_TIMEOUT_MS)
            .clamp(1, crate::service::MAX_TIMEOUT_MS),
        None => gld_protocol::tool_args::bounded(args, "exec_command", "timeout_ms")
            .min(crate::service::PLAIN_MAX_TIMEOUT_MS),
    };
    let max_output =
        gld_protocol::tool_args::bounded(args, "exec_command", "max_output_bytes") as usize;
    // 服务答话了就提前返回，所以没给 yield_time_ms 时等满上限，省得 AI 起完服务还要来回轮询。
    let yield_ms = match (service_port, args.get("yield_time_ms")) {
        (Some(_), None) => crate::service::DEFAULT_WAIT_MS,
        _ => gld_protocol::tool_args::bounded(args, "exec_command", "yield_time_ms"),
    };
    let stdin_plan = StdinPlan::from_args(args)?;
    if let Some(port) = service_port {
        let listening = crate::service::answering(port);
        if !listening.is_empty() {
            let own = sessions
                .serving(port)
                .map(|session| (session.session_id.clone(), session.command.clone()));
            return Err(crate::service::port_in_use(port, listening, own));
        }
    }

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
    let write_guard = match execution.runtime.lock_commits() {
        Some(guard) => guard,
        None => return Err(crate::workspace_runtime::write_lock_busy()),
    };

    let result = crate::async_rt::block_on(async {
        run_command(
            execution,
            sessions,
            &spec,
            &workdir.path,
            Duration::from_millis(timeout_ms),
            Duration::from_millis(yield_ms),
            max_output,
            &stdin_plan,
            service_port,
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

fn validate_child_process_scope(args: &Value) -> Result<(), WorkspaceError> {
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

/// 探针命令记在**调用方自己的**会话表里：它跟普通命令一样会起进程，超时时
/// 也一样留下一条可读的会话，只有主人看得见。
pub fn exec_health_check(
    execution: &Execution<'_>,
    sessions: &Arc<SessionStore>,
) -> Result<Value, WorkspaceError> {
    let start = Instant::now();
    let cwd = execution.workspace.root().to_path_buf();
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
        execution,
        sessions,
        &probe,
        &cwd,
        Duration::from_secs(5),
        Duration::from_secs(5),
        16_384,
        &StdinPlan::CloseImmediately,
        None,
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
        if obj
            .get("stderr")
            .and_then(Value::as_str)
            .is_some_and(|stderr| stderr.contains(SCREEN_CAPTURE_REFUSED))
        {
            if let Some(warnings) = obj.get_mut("warnings").and_then(Value::as_array_mut) {
                warnings.push(json!(SCREEN_CAPTURE_HINT));
            }
        }
    }
    snapshot
}

/// macOS 的 `screencapture` 不管是不在图形会话还是没授权，都只报这一句，后面接 `display` 或
/// 截区域时的 `rect`（2026-11-01 实测）。
const SCREEN_CAPTURE_REFUSED: &str = "could not create image from ";
const SCREEN_CAPTURE_HINT: &str = "screencapture says this for two different reasons, and retrying fixes neither: the daemon is not in a graphical login session (it inherits that from whatever started it, and Screen Recording permission does not help), or this binary has no valid Screen Recording grant (on an unsigned binary the grant stops applying when the binary is replaced, while the switch still looks on). Ask the user to run `gld desktop authorize`, which finds out which one and walks them through it.";

/// 不交给项目命令的环境变量：守护进程环境里名字像密钥的那些，按名字排。
/// 判据见 [`crate::redaction::looks_like_secret_name`]。
pub fn withheld_env_names() -> Vec<String> {
    let mut names: Vec<String> = std::env::vars_os()
        .filter_map(|(name, _)| name.into_string().ok())
        .filter(|name| crate::redaction::looks_like_secret_name(name))
        .collect();
    names.sort();
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_failure_result(error: WorkspaceError, expected_code: &str) {
        let result = execution_failure_result(&error, "missing-command", Path::new("C:/workspace"))
            .expect("应转换为统一执行结果");
        assert_eq!(result["transport_ok"], true);
        assert_eq!(result["command_ok"], false);
        assert_eq!(result["status"], "spawn_failed");
        assert_eq!(result["error"]["code"], expected_code);
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
}
