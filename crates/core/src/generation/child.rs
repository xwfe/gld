//! 执行代进程：`gld daemon generation`。
//!
//! 只听宿主的。不绑端口、不写配置、不看 HTTP 头，鉴权结果由宿主放在消息里给过来。

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufReader};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use serde_json::{json, Value};

use super::wire::{
    read_frame, FrameWriter, GenerationFrame, HostFrame, HostOp, Op, WireAuth, WireNative,
};
use super::BuildIdentity;
use crate::agents;
use crate::auth::AuthContext;
use crate::data::{forbid_writes, DataStore};
use crate::error::{AppError, AppResult};
use crate::hub::{context_fingerprint, Hub, NodeBridge};
use crate::managed_workspaces::startup::StartupSweep;
use crate::mcp::handle_request;
use crate::nodes::registry::NativeMember;
use crate::settings::{AppSettings, HubConfig};
use crate::tools::registry::{canonical_tool_name, exposed_tool_names};
use crate::tools::workspace_runtime;
use crate::tools::{self, build_tool_context, Caller, SharedToolContext, ToolContext};
use crate::usage::ServiceUsage;
use crate::workspace::{AuthConfig, WorkspaceProfile};

/// 起来之后等宿主打招呼的上限。有人手敲了这个隐藏子命令也不会一直挂着。
const HELLO_TIMEOUT: Duration = Duration::from_secs(10);
/// 反过来问宿主的上限；比节点调用自己的 60 秒多一点，超时由节点那层先报。
const NODE_CALL_TIMEOUT: Duration = Duration::from_secs(75);
const NODE_MEMBERS_TIMEOUT: Duration = Duration::from_secs(10);
/// 和宿主退出时等 Agent 收尾的时间一样。
const INTERRUPT_WAIT: Duration = Duration::from_secs(15);

type Writer = Arc<FrameWriter<File>>;

/// 进程入口。正常情况不返回：退出走 [`Generation::leave`]。
pub fn serve() -> AppResult<()> {
    let (input, output) = take_private_pipes()?;
    // 配置只有宿主能写。工具路径本来就不写，关掉是防以后有人不小心加上。
    forbid_writes();
    let writer: Writer = Arc::new(FrameWriter::new(output));
    let mut reader = BufReader::new(input);

    let (hello_seen, hello_wait) = mpsc::channel::<()>();
    std::thread::spawn(move || {
        if hello_wait.recv_timeout(HELLO_TIMEOUT).is_err() {
            eprintln!("执行代 {} 秒内没等到宿主，退出。", HELLO_TIMEOUT.as_secs());
            std::process::exit(2);
        }
    });
    let hello = read_frame(&mut reader)
        .map_err(|error| AppError::Message(format!("读宿主消息失败：{error}")))?;
    let _ = hello_seen.send(());
    match hello.as_deref().map(serde_json::from_str::<HostFrame>) {
        Some(Ok(HostFrame::Hello { .. })) => {}
        // 宿主没打招呼就走了，或者发的不是这个协议：没人要这一代。
        _ => return Err(AppError::Message("宿主没有发来可用的 hello".into())),
    }

    // 读得出数据文件才算活。宿主自己读得出但这里读不出，多半是格式变了，要拦在上线之前。
    if let Err(error) = DataStore::read_file(|_| Ok(())) {
        let _ = writer.send(&GenerationFrame::Refused {
            reason: error.to_string(),
        });
        return Err(error);
    }
    writer
        .send(&GenerationFrame::Ready {
            identity: BuildIdentity::current(),
        })
        .map_err(|error| AppError::Message(format!("回复宿主失败：{error}")))?;

    let generation = Arc::new(Generation {
        writer: Arc::clone(&writer),
        bridge: Arc::new(HostBridge {
            writer,
            pending: Mutex::new(HashMap::new()),
            next: AtomicU64::new(1),
        }),
        hub: RwLock::new(None),
        contexts: Mutex::new(HashMap::new()),
        leaving: AtomicBool::new(false),
    });
    spawn_signal_watch(Arc::clone(&generation));
    generation.read_loop(reader);
    // 管道读到头：宿主没了（包括被 kill -9）。这条命令没人能读、能停了，不留着。
    generation.leave();
    Ok(())
}

/// 把管道挪到带 close-on-exec 的新描述符，再把 0 和 1 指到别处。
///
/// 不挪的话项目命令一旦有哪条继承了 stdio，就能往通道里写伪造的消息、或者抢走宿主发来的消息；
/// 代码里误写一个 `println!` 也会把协议弄坏。
#[cfg(unix)]
fn take_private_pipes() -> AppResult<(File, File)> {
    use std::os::fd::FromRawFd;
    let moved = |fd: i32| -> io::Result<File> {
        // SAFETY: 0、1 是这个进程启动时就有的描述符，F_DUPFD_CLOEXEC 返回的新描述符归 File 独占。
        let copy = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3) };
        if copy < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(unsafe { File::from_raw_fd(copy) })
    };
    let input = moved(0)?;
    let output = moved(1)?;
    let null = c"/dev/null";
    // SAFETY: 只动本进程 0、1 两个描述符；此时还没有别的线程在用它们。
    unsafe {
        let fd = libc::open(null.as_ptr(), libc::O_RDONLY);
        if fd < 0 || libc::dup2(fd, 0) < 0 || libc::dup2(2, 1) < 0 {
            return Err(AppError::from(io::Error::last_os_error()));
        }
        libc::close(fd);
    }
    Ok((input, output))
}

/// Windows 没有 `dup2` 的等价用法。关掉继承之后，子进程拿到的只有显式指定的句柄（所有
/// 执行入口都显式指定 stdio）。这条路径只编译过，没有真机验证。
#[cfg(windows)]
fn take_private_pipes() -> AppResult<(File, File)> {
    use std::os::windows::io::{AsRawHandle, FromRawHandle};
    crate::platform::stop_std_handles_being_inherited();
    // SAFETY: 这两个句柄在进程存活期间一直有效，File 接手后也只在进程退出时关闭。
    let input = unsafe { File::from_raw_handle(io::stdin().as_raw_handle()) };
    let output = unsafe { File::from_raw_handle(io::stdout().as_raw_handle()) };
    Ok((input, output))
}

/// 宿主说停才停；另外接住 SIGTERM（`launchd` 或操作员单独杀这个进程）：收好命令再走。
fn spawn_signal_watch(generation: Arc<Generation>) {
    crate::async_rt::spawn(async move {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};
            let (Ok(mut term), Ok(mut int), Ok(mut hup)) = (
                signal(SignalKind::terminate()),
                signal(SignalKind::interrupt()),
                signal(SignalKind::hangup()),
            ) else {
                return;
            };
            tokio::select! {
                _ = term.recv() => {}
                _ = int.recv() => {}
                _ = hup.recv() => {}
            }
        }
        #[cfg(windows)]
        {
            let _ = tokio::signal::ctrl_c().await;
        }
        let _ = crate::async_rt::handle()
            .spawn_blocking(move || generation.leave())
            .await;
    });
}

struct HubSlot {
    hub: Arc<Hub>,
    sweep: Option<StartupSweep>,
}

struct Generation {
    writer: Writer,
    bridge: Arc<HostBridge>,
    hub: RwLock<Option<HubSlot>>,
    /// 单项目 MCP、Actions、`gld tool call` 用的上下文。指纹变了就重建：配置改在宿主，
    /// 这边收不到通知，每次按盘上配置算一遍。
    contexts: Mutex<HashMap<String, (String, SharedToolContext)>>,
    leaving: AtomicBool,
}

impl Generation {
    fn read_loop(self: &Arc<Self>, mut reader: BufReader<File>) {
        loop {
            let line = match read_frame(&mut reader) {
                Ok(Some(line)) => line,
                Ok(None) => return,
                Err(error) => {
                    eprintln!("执行代读宿主消息失败，退出：{error}");
                    return;
                }
            };
            match serde_json::from_str::<HostFrame>(&line) {
                Ok(HostFrame::Call { id, op }) => {
                    let generation = Arc::clone(self);
                    // 工具内核是同步的、里面有 block_on，和原来监听器一样放阻塞线程。
                    crate::async_rt::handle().spawn_blocking(move || generation.answer(id, op));
                }
                Ok(HostFrame::HostReply { id, ok, value }) => self.bridge.settle(id, ok, value),
                Ok(HostFrame::Hello { .. }) => {}
                Err(error) => eprintln!("执行代收到看不懂的消息，已忽略：{error}"),
            }
        }
    }

    fn answer(self: Arc<Self>, id: u64, op: String) {
        let parsed = serde_json::from_str::<Op>(&op);
        let leave_after = matches!(parsed, Ok(Op::Shutdown));
        // 工具 panic 只该影响这一条调用。没这层的话这个线程没了，宿主那头永远等不到回包。
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match parsed {
            Ok(op) => self.run(op),
            Err(error) => Err(format!("执行代读不懂这条操作，没有执行：{error}")),
        }))
        .unwrap_or_else(|_| Err("worker_panicked".to_string()));
        let (ok, value) = match outcome {
            Ok(value) => (true, value.to_string()),
            Err(message) => (false, message),
        };
        if let Err(error) = self.writer.send(&GenerationFrame::Reply { id, ok, value }) {
            // 回包超过上限这种情况，宿主那头还在等，得回它一句，不然那条请求永远挂着。
            let _ = self.writer.send(&GenerationFrame::Reply {
                id,
                ok: false,
                value: format!("回包发不出去：{error}"),
            });
        }
        if leave_after {
            self.exit_now();
        }
    }

    fn run(&self, op: Op) -> Result<Value, String> {
        match op {
            Op::Ping => Ok(json!({ "identity": BuildIdentity::current() })),
            Op::Status => Ok(status()),
            Op::ToolNames => Ok(json!({ "tools": all_tool_names() })),
            Op::HubConfigure { config, sweep } => {
                self.configure_hub(&config, sweep);
                Ok(json!({}))
            }
            Op::HubShutdown => {
                self.shutdown_hub();
                Ok(json!({}))
            }
            Op::ServedTools => {
                let hub = self.current_hub()?;
                Ok(json!({
                    "build": tools::exec::server_snapshot(),
                    "tools": hub.list_tools(),
                }))
            }
            Op::HubMcp { auth, body, route } => {
                let hub = self.current_hub()?;
                let (response, routed) = hub.handle_routed(&auth.into_auth(), &body, &route);
                let member = routed.as_ref().map(|routed| routed.workspace_id.clone());
                let audit = routed
                    .and_then(|routed| routed.context)
                    .map(|context| audit_digest(&context))
                    .unwrap_or(Value::Null);
                Ok(json!({ "response": response, "member": member, "audit": audit }))
            }
            Op::WorkspaceMcp {
                workspace_id,
                auth,
                body,
            } => {
                let context = self.context_for(&workspace_id, ContextKind::Mcp)?;
                let response = handle_request(&context, &caller_of(auth), &body);
                Ok(json!({ "response": response, "member": null, "audit": audit_digest(&context) }))
            }
            Op::ActionsCall {
                workspace_id,
                auth,
                tool,
                arguments,
            } => {
                let context = self.context_for(&workspace_id, ContextKind::Actions)?;
                Ok(tools::call_tool_as(
                    &context,
                    &caller_of(auth),
                    &tool,
                    &arguments,
                ))
            }
            Op::LocalTool {
                workspace_id,
                name,
                args,
            } => {
                let profile = profile_by_id(&workspace_id)?;
                let canonical =
                    check_tool_call(&profile, &name, &args).map_err(|e| e.to_string())?;
                let context = self.context_for(&workspace_id, ContextKind::Local)?;
                Ok(tools::call_tool(&context, canonical, &args))
            }
            Op::TerminateWorkspace { root } => {
                let stopped = workspace_runtime::runtime_for(&root).terminate_all_sessions();
                Ok(json!({ "stopped": stopped }))
            }
            Op::RevokeGrant { grant_id } => {
                let stopped_commands = workspace_runtime::terminate_grant_sessions(&grant_id);
                let stopped_agent_tasks = crate::home::data_home()
                    .map_or(0, |home| agents::stop_for_grant(&home, &grant_id));
                Ok(json!({
                    "stopped_commands": stopped_commands,
                    "stopped_agent_tasks": stopped_agent_tasks,
                }))
            }
            Op::ApproveAgent {
                task_id,
                agent,
                model,
                effort,
            } => {
                let home = crate::home::data_home().map_err(|e| e.to_string())?;
                let task = agents::approve_with(
                    &home,
                    &task_id,
                    agents::ApprovalChange {
                        agent: agent.as_deref(),
                        model: model.as_deref(),
                        effort: effort.as_deref(),
                    },
                )
                .map_err(agent_failure)?;
                serde_json::to_value(task).map_err(|e| e.to_string())
            }
            Op::StopAgent { task_id } => {
                let home = crate::home::data_home().map_err(|e| e.to_string())?;
                let task = agents::stop(&home, &task_id).map_err(agent_failure)?;
                serde_json::to_value(task).map_err(|e| e.to_string())
            }
            Op::InterruptAll => {
                interrupt_everything();
                Ok(json!({}))
            }
            Op::Shutdown => {
                self.stop_everything();
                Ok(json!({}))
            }
        }
    }

    fn current_hub(&self) -> Result<Arc<Hub>, String> {
        self.hub
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .map(|slot| Arc::clone(&slot.hub))
            .ok_or_else(|| "服务没在跑".to_string())
    }

    fn configure_hub(&self, config: &HubConfig, sweep: bool) {
        self.shutdown_hub();
        let bridge: Arc<dyn NodeBridge> = self.bridge.clone();
        let hub = Arc::new(Hub::new(config).with_node_bridge(bridge));
        let sweep = sweep.then(|| StartupSweep::launch(config)).flatten();
        *self
            .hub
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(HubSlot { hub, sweep });
    }

    /// 和原来 `hub::runtime::stop_runtime` 里的同一段：停启动检查、收 Hub 入口起的命令、关远端。
    fn shutdown_hub(&self) {
        let slot = self
            .hub
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(mut slot) = slot {
            if let Some(sweep) = slot.sweep.take() {
                sweep.cancel();
                crate::async_rt::block_on(sweep.join());
            }
            slot.hub.shutdown();
        }
    }

    fn context_for(
        &self,
        workspace_id: &str,
        kind: ContextKind,
    ) -> Result<SharedToolContext, String> {
        let (profile, settings) = DataStore::read_file(|data| {
            Ok((
                data.profiles
                    .iter()
                    .find(|profile| profile.id == workspace_id)
                    .cloned(),
                AppSettings::from_data(data),
            ))
        })
        .map_err(|e| e.to_string())?;
        let profile = profile.ok_or_else(|| format!("没有这个项目：{workspace_id}"))?;
        let fingerprint = format!(
            "{}|{}|{}",
            context_fingerprint(&profile, &settings),
            serde_json::to_string(&profile.auth).unwrap_or_default(),
            serde_json::to_string(&profile.actions).unwrap_or_default(),
        );
        let key = format!("{}:{workspace_id}", kind.label());
        let mut contexts = self
            .contexts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some((cached, context)) = contexts.get(&key) {
            if *cached == fingerprint {
                return Ok(Arc::clone(context));
            }
        }
        let context = Arc::new(kind.build(&profile, &settings)?);
        contexts.insert(key, (fingerprint, Arc::clone(&context)));
        Ok(context)
    }

    fn stop_everything(&self) {
        interrupt_everything();
        self.shutdown_hub();
        // 停服务的这段时间可能又有调用起了命令。
        workspace_runtime::terminate_all_sessions_everywhere();
    }

    /// 自己要走（宿主没了、收到信号）：收好再退。只走一次。
    fn leave(&self) {
        if !self.leaving.swap(true, Ordering::SeqCst) {
            self.stop_everything();
            self.exit_now();
        }
    }

    fn exit_now(&self) {
        std::process::exit(0);
    }
}

fn interrupt_everything() {
    workspace_runtime::terminate_all_sessions_everywhere();
    agents::interrupt_all(INTERRUPT_WAIT);
}

fn caller_of(auth: WireAuth) -> Caller {
    let auth: AuthContext = auth.into_auth();
    Caller::from_auth(&auth)
}

fn agent_failure(error: agents::AgentError) -> String {
    format!("{}（{}）", error.message, error.code)
}

fn profile_by_id(workspace_id: &str) -> Result<WorkspaceProfile, String> {
    DataStore::read_file(|data| Ok(data.profiles.iter().find(|p| p.id == workspace_id).cloned()))
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("没有这个项目：{workspace_id}"))
}

fn all_tool_names() -> Vec<String> {
    tools::list_tools()
        .iter()
        .filter_map(|tool| tool.get("name").and_then(Value::as_str).map(str::to_string))
        .collect()
}

/// 监听器只用这几个字段写日志。整份审计快照随调用次数增长，不值得每次过管道。
fn audit_digest(context: &SharedToolContext) -> Value {
    let audit = context.context_audit_snapshot();
    json!({
        "repeated_bytes": audit.get("repeated_bytes"),
        "total_bytes": audit.get("total_bytes"),
        "blocks": audit
            .get("blocks")
            .and_then(Value::as_array)
            .and_then(|blocks| blocks.last())
            .map(|block| vec![block.clone()])
            .unwrap_or_default(),
    })
}

/// 只报数量和目录，不带命令文本或参数。
fn status() -> Value {
    let commands: Vec<Value> = workspace_runtime::running_session_counts()
        .into_iter()
        .map(|(root, count): (PathBuf, usize)| json!({ "root": root, "commands": count }))
        .collect();
    let total: usize = commands
        .iter()
        .filter_map(|row| row["commands"].as_u64())
        .map(|count| count as usize)
        .sum();
    json!({
        "identity": BuildIdentity::current(),
        "active_commands": total,
        "active_agents": agents::live_count(),
        "commands_by_workspace": commands,
    })
}

/// 同一个 `gld tool call` 在直连和转发时做一样的校验，报一样的错。
pub(crate) fn check_tool_call<'a>(
    profile: &WorkspaceProfile,
    name: &'a str,
    args: &Value,
) -> AppResult<&'a str> {
    let canonical = canonical_tool_name(name);
    let exposed = exposed_tool_names(&profile.runtime.tool_profile);
    if !exposed.contains(&canonical) {
        return Err(AppError::Message(format!(
            "当前工具集「{}」没有暴露工具「{name}」。`gld tool list` 查看可用工具，\
             或用 `gld set <项目> tool-profile=advanced` 换一个工具集。",
            profile.runtime.tool_profile
        )));
    }
    if !args.is_object() {
        return Err(AppError::Message("工具参数必须是 JSON 对象".into()));
    }
    Ok(canonical)
}

#[derive(Clone, Copy)]
enum ContextKind {
    Mcp,
    Actions,
    Local,
}

impl ContextKind {
    fn label(self) -> &'static str {
        match self {
            Self::Mcp => "mcp",
            Self::Actions => "actions",
            Self::Local => "local",
        }
    }

    /// 和宿主那边各入口原来建上下文的参数一一对应，命令会话表不受影响（按目录和主体分）。
    fn build(
        self,
        profile: &WorkspaceProfile,
        settings: &AppSettings,
    ) -> Result<ToolContext, String> {
        match self {
            Self::Mcp | Self::Local => build_tool_context(
                PathBuf::from(&profile.path),
                &profile.name,
                profile.auth.clone(),
                &profile.runtime,
                settings,
                Arc::new(ServiceUsage::default()),
            ),
            Self::Actions => {
                let workspace = tools::Workspace::new(PathBuf::from(&profile.path))
                    .map_err(|error| error.message())?;
                let policy = tools::PolicySettings::from_actions_config(&profile.actions);
                let permission_mode = policy.permission_mode.clone();
                Ok(ToolContext::from_workspace(
                    workspace,
                    AuthConfig {
                        auth_type: profile.actions.auth_type.clone(),
                        ..AuthConfig::default()
                    },
                    policy,
                    "full".into(),
                    permission_mode,
                ))
            }
        }
    }
}

/// 节点连接在宿主手里，要用就经管道问。
struct HostBridge {
    writer: Writer,
    pending: Mutex<HashMap<u64, Sender<Result<Value, String>>>>,
    next: AtomicU64,
}

impl HostBridge {
    fn ask(&self, op: HostOp, timeout: Duration) -> Result<Value, String> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (reply, wait) = mpsc::channel();
        self.lock_pending().insert(id, reply);
        let op = serde_json::to_string(&op).map_err(|error| error.to_string())?;
        if let Err(error) = self.writer.send(&GenerationFrame::HostCall { id, op }) {
            self.lock_pending().remove(&id);
            return Err(format!("问宿主失败：{error}"));
        }
        let answer = wait.recv_timeout(timeout);
        self.lock_pending().remove(&id);
        answer.unwrap_or_else(|_| Err("等宿主回复超时".to_string()))
    }

    fn settle(&self, id: u64, ok: bool, value: String) {
        if let Some(reply) = self.lock_pending().remove(&id) {
            let _ = reply.send(if ok {
                serde_json::from_str(&value).map_err(|error| error.to_string())
            } else {
                Err(value)
            });
        }
    }

    fn lock_pending(
        &self,
    ) -> std::sync::MutexGuard<'_, HashMap<u64, Sender<Result<Value, String>>>> {
        self.pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl NodeBridge for HostBridge {
    fn members(&self) -> Result<Vec<NativeMember>, String> {
        let value = self.ask(HostOp::NodeMembers, NODE_MEMBERS_TIMEOUT)?;
        let members: Vec<WireNative> =
            serde_json::from_value(value).map_err(|error| error.to_string())?;
        Ok(members.into_iter().map(NativeMember::from).collect())
    }

    fn call(&self, auth: &AuthContext, member: &NativeMember, tool: &str, args: Value) -> Value {
        let asked = self.ask(
            HostOp::NodeCall {
                auth: WireAuth::from(auth),
                member: WireNative::from(member),
                tool: tool.to_string(),
                args,
            },
            NODE_CALL_TIMEOUT,
        );
        asked.unwrap_or_else(|message| crate::hub::node_bridge_failure(&message))
    }
}
