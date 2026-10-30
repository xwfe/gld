//! 宿主这头的路由：请求进哪一代、工作区归谁、怎么准备和切换新代、现在是什么状态。
//!
//! 所有“看一眼状态再动手”都在 [`State`] 这一把锁里做完。分成两步做的话，
//! 查完“这个工作区没人”和真正绑定之间可能又进来一条请求，这正是 RFC-0008 §6 说不能算数的那种快照。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tokio::runtime::Handle;

use super::host::Link;
use super::wire::{Op, WireAuth, WireNative};
use super::{BuildIdentity, GENERATION_PROTOCOL, MAX_LIVE_GENERATIONS, STATE_FORMAT};
use crate::auth::AuthContext;
use crate::hub::{ExecutionRoute, Hub};
use crate::nodes::registry::{NativeMember, Nodes};
use crate::settings::HubConfig;

/// 启动和替换时等子进程就绪的上限。
pub const READY_TIMEOUT: Duration = Duration::from_secs(15);
/// 当前代刚挂、替代的还没起来时，新请求等这么久；再久就如实报不可用。
const ADMIT_WAIT: Duration = Duration::from_secs(10);
/// Agent 收尾最多 15 秒，再留一点给命令。
const INTERRUPT_TIMEOUT: Duration = Duration::from_secs(30);
const CONTROL_TIMEOUT: Duration = Duration::from_secs(5);
/// 有旧代时，多久叫它们对一次成员名单。每次要读配置和跨进程问一轮，不用每条请求都做。
const RECONCILE_EVERY: Duration = Duration::from_secs(5);
const RESTART_BACKOFF: [Duration; 5] = [
    Duration::from_millis(200),
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(5),
    Duration::from_secs(10),
];

/// 转发失败的三种情况，调用方靠它决定告诉客户端什么。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallError {
    /// 请求没发到执行代，确定没执行。
    Unavailable(String),
    /// 发出去之后执行代没了：结果未知，可能已经产生副作用，绝不能自动重放。
    Lost,
    /// 控制类操作等超时。
    TimedOut,
    /// 执行代回了错误。
    Refused(String),
}

impl CallError {
    /// 确定没有执行，客户端可以放心重试。其余情况工具可能已经跑过（`Lost`，或者跑完了回包发不出来、
    /// panic 的 `Refused`），重试有可能把副作用做两遍，只能让调用方自己查。
    pub fn surely_not_executed(&self) -> bool {
        matches!(self, Self::Unavailable(_))
    }
}

impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable(reason) => write!(f, "执行代不可用：{reason}"),
            Self::Lost => write!(f, "执行代在处理这次调用时退出了，结果未知"),
            Self::TimedOut => write!(f, "等执行代回复超时"),
            Self::Refused(reason) => write!(f, "{reason}"),
        }
    }
}

/// 一次 MCP 请求的结果，监听器拿它写日志和回包。
pub struct McpReply {
    pub response: Value,
    pub member: Option<String>,
    pub audit: Value,
}

struct OwnedWorkspace {
    generation: u64,
    in_flight: usize,
}

struct State {
    /// 活着的代，老的在前。
    generations: Vec<Arc<Link>>,
    current: Option<Arc<Link>>,
    /// 准备好了、还没切过去的。除了核对用的请求，什么都不路由给它。
    prepared: Option<Arc<Link>>,
    preparing: bool,
    /// 正在替挂掉的当前代找替代。只开一个线程，第二次丢失不再多开。
    replacing: bool,
    owners: HashMap<String, OwnedWorkspace>,
    /// 服务在跑时的配置。新起的代按它建 Hub，起不来也不丢。
    hub_config: Option<HubConfig>,
    next_id: u64,
    restarts: u64,
    retired: u64,
    last_failure: Option<String>,
    last_reconcile: Option<Instant>,
    shutting_down: bool,
}

pub struct Router {
    state: Mutex<State>,
    changed: Condvar,
    handle: Handle,
    me: Weak<Router>,
    on_generations_changed: Box<dyn Fn(&Router) + Send + Sync>,
    nodes: Mutex<Option<Arc<Nodes>>>,
    started: Instant,
}

/// 一条在途请求。活着就算数，丢掉时才减——客户端断开不减，因为执行代还在跑它。
struct Admission {
    router: Arc<Router>,
    link: Arc<Link>,
    key: Option<String>,
}

impl Drop for Admission {
    fn drop(&mut self) {
        self.link.in_flight.fetch_sub(1, Ordering::SeqCst);
        if let Some(key) = &self.key {
            let mut state = self.router.lock();
            if let Some(owned) = state.owners.get_mut(key) {
                // 崩溃后这个键可能已经绑给了别的代，那边的计数不归这条请求管。
                if owned.generation == self.link.id {
                    owned.in_flight = owned.in_flight.saturating_sub(1);
                }
            }
        }
        self.router.retire_idle();
    }
}

impl Router {
    /// 起第一代。起不来就报错：守护进程还没开始对外，直接退比带病运行好。
    pub fn start(
        exe: PathBuf,
        on_generations_changed: Box<dyn Fn(&Router) + Send + Sync>,
    ) -> Result<Arc<Self>, String> {
        let router = Arc::new_cyclic(|me: &Weak<Router>| Self {
            state: Mutex::new(State {
                generations: Vec::new(),
                current: None,
                prepared: None,
                preparing: false,
                replacing: false,
                owners: HashMap::new(),
                hub_config: None,
                next_id: 1,
                restarts: 0,
                retired: 0,
                last_failure: None,
                last_reconcile: None,
                shutting_down: false,
            }),
            changed: Condvar::new(),
            handle: Handle::current(),
            me: me.clone(),
            on_generations_changed,
            nodes: Mutex::new(None),
            started: Instant::now(),
        });
        let link = Link::spawn(1, &exe, Arc::downgrade(&router), READY_TIMEOUT)
            .map_err(|reason| format!("第一个执行代起不来：{reason}"))?;
        if let Some(reason) = link.identity().incompatibility() {
            link.kill();
            return Err(format!("第一个执行代不能用：{reason}"));
        }
        {
            let mut state = router.lock();
            state.next_id = 2;
            state.generations.push(Arc::clone(&link));
            state.current = Some(Arc::clone(&link));
        }
        router.publish_generations();
        router.notice_if_already_gone(&link);
        Ok(router)
    }

    pub(super) fn handle(&self) -> &Handle {
        &self.handle
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// 活着的执行代的 pid。`gld daemon restart` 查运行记录时要用，它们才是命令的 owner。
    pub fn generation_pids(&self) -> Vec<u32> {
        self.lock()
            .generations
            .iter()
            .map(|link| link.pid())
            .collect()
    }

    fn publish_generations(&self) {
        (self.on_generations_changed)(self);
    }

    /// 刚起来、还没进表就退了的话，读线程那次 [`Self::on_lost`] 查无此代，什么都没做。
    /// 进表之后补问一次，不然路由手里就是一条再也没人清的死连接。
    fn notice_if_already_gone(&self, link: &Link) {
        if !link.is_alive() {
            self.on_lost(link.id, "刚起来就退出了");
        }
    }

    fn arc(&self) -> Arc<Router> {
        self.me.upgrade().expect("Router 活着才走得到这里")
    }

    /// 选一代、记账。判断和记账在同一把锁里，切换也拿这把锁。
    fn admit(&self, key: Option<String>, discovery: bool) -> Result<Admission, CallError> {
        let deadline = Instant::now() + ADMIT_WAIT;
        let mut state = self.lock();
        loop {
            if state.shutting_down {
                return Err(CallError::Unavailable("守护进程正在退出".into()));
            }
            if let Some(link) = pick(&mut state, key.as_deref(), discovery) {
                link.in_flight.fetch_add(1, Ordering::SeqCst);
                if let Some(owned) = key.as_deref().and_then(|key| state.owners.get_mut(key)) {
                    owned.in_flight += 1;
                }
                return Ok(Admission {
                    router: self.arc(),
                    link,
                    key,
                });
            }
            let now = Instant::now();
            if now >= deadline {
                let reason = state
                    .last_failure
                    .clone()
                    .unwrap_or_else(|| "正在重启".to_string());
                return Err(CallError::Unavailable(reason));
            }
            let (next, _) = self
                .changed
                .wait_timeout(state, deadline - now)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state = next;
        }
    }

    fn forward(&self, key: Option<String>, discovery: bool, op: &Op) -> Result<Value, CallError> {
        let admission = self.admit(key, discovery)?;
        admission.link.call(op, None)
    }

    // ---- 工具入口 ----

    /// Hub 上的一次 MCP 请求。必须在阻塞线程里调。
    pub fn hub_mcp(
        &self,
        hub: &Hub,
        auth: &AuthContext,
        body: &Value,
    ) -> Result<McpReply, CallError> {
        let route = hub.execution_route(auth, body);
        let (key, discovery) = match &route {
            ExecutionRoute::Discovery => (None, true),
            ExecutionRoute::Unbound => (None, false),
            ExecutionRoute::Owned(key) => (Some(key.clone()), false),
        };
        let op = Op::HubMcp {
            auth: WireAuth::from(auth),
            body: body.clone(),
            route,
        };
        let reply = self.forward(key, discovery, &op);
        self.reconcile_draining();
        mcp_reply(reply?)
    }

    /// 单项目监听器上的一次 MCP 请求。
    pub fn workspace_mcp(
        &self,
        workspace_id: &str,
        root: &Path,
        auth: &AuthContext,
        body: &Value,
    ) -> Result<McpReply, CallError> {
        let op = Op::WorkspaceMcp {
            workspace_id: workspace_id.to_string(),
            auth: WireAuth::from(auth),
            body: body.clone(),
        };
        mcp_reply(self.forward(Some(root_key(root)), false, &op)?)
    }

    pub fn actions_call(
        &self,
        workspace_id: &str,
        root: &Path,
        auth: &AuthContext,
        tool: &str,
        arguments: &Value,
    ) -> Result<Value, CallError> {
        let op = Op::ActionsCall {
            workspace_id: workspace_id.to_string(),
            auth: WireAuth::from(auth),
            tool: tool.to_string(),
            arguments: arguments.clone(),
        };
        self.forward(Some(root_key(root)), false, &op)
    }

    /// `gld tool call`。
    pub fn local_tool(
        &self,
        workspace_id: &str,
        root: &Path,
        name: &str,
        args: &Value,
    ) -> Result<Value, CallError> {
        let op = Op::LocalTool {
            workspace_id: workspace_id.to_string(),
            name: name.to_string(),
            args: args.clone(),
        };
        self.forward(Some(root_key(root)), false, &op)
    }

    pub fn served_tools(&self) -> Result<Value, CallError> {
        self.forward(None, true, &Op::ServedTools)
    }

    pub fn approve_agent(
        &self,
        root: &Path,
        task_id: &str,
        agent: Option<&str>,
        model: Option<&str>,
        effort: Option<&str>,
    ) -> Result<Value, CallError> {
        let op = Op::ApproveAgent {
            task_id: task_id.to_string(),
            agent: agent.map(str::to_string),
            model: model.map(str::to_string),
            effort: effort.map(str::to_string),
        };
        // Agent 和这个工作区的命令、写入归同一代，那边的写入拦截才看得见它。
        self.forward(Some(root_key(root)), false, &op)
    }

    /// 在跑的只有起它的那一代停得了，挨个问。
    pub fn stop_agent(&self, task_id: &str) -> Result<Value, CallError> {
        let op = Op::StopAgent {
            task_id: task_id.to_string(),
        };
        let mut last = CallError::Unavailable("没有活着的执行代".into());
        for link in self.routable() {
            match link.call(&op, Some(CONTROL_TIMEOUT)) {
                Ok(value) => return Ok(value),
                Err(error) => last = error,
            }
        }
        Err(last)
    }

    /// 只发给这个工作区的主人：它没被绑过就没有命令可停。
    pub fn terminate_workspace(&self, root: &Path) -> Result<usize, CallError> {
        let link = {
            let state = self.lock();
            state
                .owners
                .get(&root_key(root))
                .and_then(|owned| state.link_by_id(owned.generation))
        };
        let Some(link) = link else {
            return Ok(0);
        };
        let reply = link.call(
            &Op::TerminateWorkspace {
                root: root.to_path_buf(),
            },
            Some(INTERRUPT_TIMEOUT),
        )?;
        Ok(reply["stopped"].as_u64().unwrap_or(0) as usize)
    }

    /// 撤销 grant：它在哪一代起过命令都要停，全发。
    pub fn revoke_grant(&self, grant_id: &str) -> (usize, usize) {
        let mut totals = (0, 0);
        for link in self.routable() {
            let op = Op::RevokeGrant {
                grant_id: grant_id.to_string(),
            };
            match link.call(&op, Some(INTERRUPT_TIMEOUT)) {
                Ok(reply) => {
                    totals.0 += reply["stopped_commands"].as_u64().unwrap_or(0) as usize;
                    totals.1 += reply["stopped_agent_tasks"].as_u64().unwrap_or(0) as usize;
                }
                Err(error) => eprintln!("g{} 没停掉 grant 的命令：{error}", link.id),
            }
        }
        totals
    }

    /// 平时每次工具调用都会顺手收掉离开的成员，可切换之后旧代可能再也收不到调用。
    /// 所以有旧代时由这边定期叫它做，收完把已经不存在的工作区从它名下摘掉，它才退得了。
    fn reconcile_draining(&self) {
        let draining: Vec<Arc<Link>> = {
            let mut state = self.lock();
            let recent = state
                .last_reconcile
                .is_some_and(|last| last.elapsed() < RECONCILE_EVERY);
            if state.generations.len() < 2 || recent {
                return;
            }
            state.last_reconcile = Some(Instant::now());
            state
                .generations
                .iter()
                .filter(|link| !state.is_current(link.id) && !state.is_prepared(link.id))
                .cloned()
                .collect()
        };
        if draining.is_empty() {
            return;
        }
        let router = self.arc();
        self.handle.spawn_blocking(move || {
            for link in draining {
                if let Err(error) = link.call(&Op::HubReconcile, Some(INTERRUPT_TIMEOUT)) {
                    eprintln!("g{} 没能收掉离开的成员：{error}", link.id);
                    continue;
                }
                router.release_stale_owners(&link);
            }
            router.retire_idle();
        });
    }

    fn release_stale_owners(&self, link: &Link) {
        let Ok(known) = crate::hub::known_owner_keys() else {
            return;
        };
        let Ok(status) = link.call(&Op::Status, Some(CONTROL_TIMEOUT)) else {
            return;
        };
        // 项目删了但命令还没停（比如 `gld tool call` 起的，不归 Hub 收）的不摘：
        // 同一个目录再登记回来时，命令和写入还得在同一代。
        let busy: HashSet<String> = status["commands_by_workspace"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|row| row["root"].as_str().map(str::to_string))
            .collect();
        self.lock().owners.retain(|key, owned| {
            owned.generation != link.id
                || owned.in_flight > 0
                || known.contains(key)
                || busy.contains(key)
        });
    }

    fn routable(&self) -> Vec<Arc<Link>> {
        let state = self.lock();
        state
            .generations
            .iter()
            .filter(|link| !state.is_prepared(link.id))
            .cloned()
            .collect()
    }

    // ---- Hub 生命周期 ----

    pub fn set_nodes(&self, nodes: Option<Arc<Nodes>>) {
        *self
            .nodes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = nodes;
    }

    fn nodes(&self) -> Option<Arc<Nodes>> {
        self.nodes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub(super) fn node_members(&self) -> Result<Value, String> {
        let members = match self.nodes() {
            Some(nodes) => nodes.members()?,
            None => Vec::new(),
        };
        let members: Vec<WireNative> = members.iter().map(WireNative::from).collect();
        serde_json::to_value(members).map_err(|error| error.to_string())
    }

    pub(super) fn node_call(
        &self,
        auth: &AuthContext,
        member: NativeMember,
        tool: &str,
        args: Value,
    ) -> Value {
        match self.nodes() {
            Some(nodes) => nodes.call(auth, &member, tool, args),
            None => crate::hub::node_bridge_failure("服务没在跑"),
        }
    }

    /// 服务（重新）起来：所有代都按新配置建 Hub。启动检查每次监听器启动只跑一轮，只给当前代。
    pub fn hub_configure(&self, config: &HubConfig) {
        let (links, current) = {
            let mut state = self.lock();
            state.hub_config = Some(config.clone());
            (
                state.generations.clone(),
                state.current.as_ref().map(|link| link.id),
            )
        };
        for link in links {
            let op = Op::HubConfigure {
                config: Box::new(config.clone()),
                sweep: Some(link.id) == current,
            };
            if let Err(error) = link.call(&op, Some(INTERRUPT_TIMEOUT)) {
                eprintln!("g{} 没能按新配置建 Hub：{error}", link.id);
            }
        }
    }

    pub fn hub_shutdown(&self) {
        let links = {
            let mut state = self.lock();
            state.hub_config = None;
            state.generations.clone()
        };
        for link in links {
            if let Err(error) = link.call(&Op::HubShutdown, Some(INTERRUPT_TIMEOUT)) {
                eprintln!("g{} 没能停 Hub：{error}", link.id);
            }
        }
    }

    // ---- 退出 ----

    /// 第一步：命令和 Agent 收掉，记成 `interrupted`。这时监听器还开着。
    pub fn interrupt_all(&self) {
        let links = self.lock().generations.clone();
        std::thread::scope(|scope| {
            for link in &links {
                scope.spawn(|| {
                    if let Err(error) = link.call(&Op::InterruptAll, Some(INTERRUPT_TIMEOUT)) {
                        eprintln!("g{} 收命令失败：{error}", link.id);
                    }
                });
            }
        });
    }

    /// 最后一步：所有代退出。之后这个进程就要退了。
    pub fn shutdown(&self) {
        let links = {
            let mut state = self.lock();
            state.shutting_down = true;
            state.prepared = None;
            state.current = None;
            std::mem::take(&mut state.generations)
        };
        self.changed.notify_all();
        std::thread::scope(|scope| {
            for link in &links {
                scope.spawn(|| link.shutdown(INTERRUPT_TIMEOUT));
            }
        });
    }

    // ---- 新代：准备、切换、放弃 ----

    /// 起一个新代并验证它，全程不碰在线路由。失败时只影响它自己。
    pub fn prepare(&self, exe: &Path, ready_timeout: Duration) -> Result<Value, String> {
        let id = {
            let mut state = self.lock();
            if state.shutting_down {
                return Err("守护进程正在退出".into());
            }
            if state.preparing {
                return Err("正在准备另一个新代".into());
            }
            if state.current.is_none() {
                return Err(
                    "当前执行代刚退出，正在重启，这时没有东西可以核对新版本。等它恢复再准备，看 gld daemon upgrade status"
                        .into(),
                );
            }
            if let Some(prepared) = &state.prepared {
                return Err(format!(
                    "已经准备好了 g{}（{}）。先 gld daemon upgrade switch 切过去，或者 abort 放弃它",
                    prepared.id,
                    prepared.exe.display()
                ));
            }
            if state.generations.len() >= MAX_LIVE_GENERATIONS {
                return Err(format!(
                    "已经有 {} 代在跑（上限 {MAX_LIVE_GENERATIONS}），旧代还占着工作区或命令。\
                     不会为了腾位置杀掉它们；工作区交接还没做，全换成新版本只能 gld daemon restart，\
                     现状看 gld daemon upgrade status",
                    state.generations.len()
                ));
            }
            state.preparing = true;
            let id = state.next_id;
            state.next_id += 1;
            id
        };
        let outcome = self.bring_up_and_verify(id, exe, ready_timeout);
        let applied = outcome.as_ref().ok().and_then(|(_, config)| config.clone());
        let outcome = outcome.map(|(link, _)| link);
        let mut state = self.lock();
        state.preparing = false;
        match outcome {
            Ok(link) if !state.shutting_down => {
                state.generations.push(Arc::clone(&link));
                state.prepared = Some(Arc::clone(&link));
                state.last_failure = None;
                drop(state);
                self.catch_up_hub(&link, &applied);
                self.publish_generations();
                self.notice_if_already_gone(&link);
                Ok(generation_row(&link, "prepared"))
            }
            Ok(link) => {
                drop(state);
                link.kill();
                Err("守护进程正在退出".into())
            }
            Err(reason) => {
                state.last_failure = Some(format!("准备 {} 失败：{reason}", exe.display()));
                Err(reason)
            }
        }
    }

    fn bring_up_and_verify(
        &self,
        id: u64,
        exe: &Path,
        ready_timeout: Duration,
    ) -> Result<(Arc<Link>, Option<HubConfig>), String> {
        let exe = exe
            .canonicalize()
            .map_err(|error| format!("找不到 {}：{error}", exe.display()))?;
        let link = Link::spawn(id, &exe, self.me.clone(), ready_timeout)?;
        match self.verify(&link) {
            Ok(applied) => Ok((link, applied)),
            Err(reason) => {
                link.kill();
                Err(reason)
            }
        }
    }

    /// 严格就绪：身份和格式对得上、真能处理请求、工具一个不少、能按当前配置建好 Hub。
    /// 能连上或者回了什么状态码都不算数。
    /// 返回建 Hub 时用的配置。
    fn verify(&self, link: &Arc<Link>) -> Result<Option<HubConfig>, String> {
        if let Some(reason) = link.identity().incompatibility() {
            return Err(reason);
        }
        let pong = link
            .call(&Op::Ping, Some(CONTROL_TIMEOUT))
            .map_err(|error| format!("不应答：{error}"))?;
        if pong["identity"]["pid"].as_u64() != Some(u64::from(link.pid())) {
            return Err("应答里的身份和它启动时报的不一致".into());
        }
        // 先建 Hub：服务自己的工具（`work_session`、`remote_*`）要它建好了才列得出来。
        let config = self.lock().hub_config.clone();
        if let Some(config) = &config {
            link.call(
                &Op::HubConfigure {
                    config: Box::new(config.clone()),
                    sweep: false,
                },
                Some(CONTROL_TIMEOUT),
            )
            .map_err(|error| format!("按当前配置建不起 Hub：{error}"))?;
        }
        let names = link
            .call(&Op::ToolNames, Some(CONTROL_TIMEOUT))
            .map_err(|error| format!("问不到工具表：{error}"))?;
        let current = {
            let state = self.lock();
            state.current.clone()
        };
        if let Some(current) = current {
            let have = tool_names(
                &current
                    .call(&Op::ToolNames, Some(CONTROL_TIMEOUT))
                    .map_err(|error| format!("问不到当前代的工具表：{error}"))?,
            );
            let offered = tool_names(&names);
            let mut missing: Vec<&String> = have.difference(&offered).collect();
            missing.sort();
            if !missing.is_empty() {
                return Err(format!(
                    "它少了当前代有的工具：{}。连接器手里的工具表还有它们，切过去会调不通",
                    missing
                        .iter()
                        .map(|name| name.as_str())
                        .collect::<Vec<_>>()
                        .join("、")
                ));
            }
        }
        Ok(config)
    }

    /// 新代进表之前服务可能正好重启过，那次广播没发给它。进表之后的变化广播都收得到，
    /// 所以进表后补对一次就够。
    fn catch_up_hub(&self, link: &Link, applied: &Option<HubConfig>) {
        let latest = self.lock().hub_config.clone();
        if latest == *applied {
            return;
        }
        let op = match latest {
            Some(config) => Op::HubConfigure {
                config: Box::new(config),
                sweep: false,
            },
            None => Op::HubShutdown,
        };
        if let Err(error) = link.call(&op, Some(INTERRUPT_TIMEOUT)) {
            eprintln!("g{} 没能跟上服务配置：{error}", link.id);
        }
    }

    /// 切过去：之后没绑过的工作区进新代，已经绑过的还留在原来那一代。
    pub fn switch(&self) -> Result<Value, String> {
        let prepared = self
            .lock()
            .prepared
            .clone()
            .ok_or("没有准备好的新代。先 gld daemon upgrade prepare <二进制路径>")?;
        // 准备好之后可能过了很久，切之前再问一次，这一步不拿锁：等应答时不能挡住在线请求。
        if let Err(error) = prepared.call(&Op::Ping, Some(CONTROL_TIMEOUT)) {
            self.discard_prepared(prepared.id);
            return Err(format!("新代已经不应答了，已放弃，路由没动：{error}"));
        }
        let previous = {
            let mut state = self.lock();
            let still_there = state
                .prepared
                .as_ref()
                .is_some_and(|link| link.id == prepared.id && link.is_alive());
            if !still_there {
                return Err("新代刚刚退出了，路由没动".into());
            }
            state.prepared = None;
            state.current.replace(Arc::clone(&prepared))
        };
        self.changed.notify_all();
        self.retire_idle();
        Ok(json!({
            "current": format!("g{}", prepared.id),
            "previous": previous.map(|link| format!("g{}", link.id)),
        }))
    }

    /// 放弃准备好的新代。路由本来就没指向它，什么也不用恢复。
    pub fn abort(&self) -> Result<Value, String> {
        let prepared = self.lock().prepared.take().ok_or("没有准备好的新代")?;
        self.remove_generation(prepared.id);
        prepared.shutdown(CONTROL_TIMEOUT);
        self.publish_generations();
        Ok(json!({ "aborted": format!("g{}", prepared.id) }))
    }

    fn discard_prepared(&self, id: u64) {
        let taken = {
            let mut state = self.lock();
            if state.is_prepared(id) {
                state.prepared.take()
            } else {
                None
            }
        };
        if let Some(link) = taken {
            self.remove_generation(link.id);
            link.kill();
            self.publish_generations();
        }
    }

    fn remove_generation(&self, id: u64) {
        self.lock().generations.retain(|link| link.id != id);
    }

    /// 旧代没有在途调用、没有工作区、没有命令，才让它走。
    ///
    /// 工作区交接是下一期（RFC-0008 §5）：这一期旧代只要被绑过工作区就一直留着，
    /// 这不是漏了，是不能在没做受保护交接时把工作区挪走（写入序号会漏记）。
    fn retire_idle(&self) {
        let idle: Vec<Arc<Link>> = {
            let state = self.lock();
            state
                .generations
                .iter()
                .filter(|link| !state.is_current(link.id) && !state.is_prepared(link.id))
                .filter(|link| link.in_flight.load(Ordering::SeqCst) == 0)
                .filter(|link| {
                    !state
                        .owners
                        .values()
                        .any(|owned| owned.generation == link.id)
                })
                .cloned()
                .collect()
        };
        if idle.is_empty() {
            return;
        }
        let router = self.arc();
        self.handle.spawn_blocking(move || {
            for link in idle {
                router.retire(link);
            }
        });
    }

    fn retire(&self, link: Arc<Link>) {
        // 问状态不拿锁；问完在锁里再确认一次，这中间不会有新路由指向它，
        // 因为它既不是当前代、也不再拥有工作区——只有发现类请求可能挑中它，所以还要看在途数。
        let status = link.call(&Op::Status, Some(CONTROL_TIMEOUT));
        let quiet = status.is_ok_and(|status| {
            status["active_commands"].as_u64() == Some(0)
                && status["active_agents"].as_u64() == Some(0)
        });
        if !quiet {
            return;
        }
        {
            let mut state = self.lock();
            if state.is_current(link.id)
                || link.in_flight.load(Ordering::SeqCst) != 0
                || state.link_by_id(link.id).is_none()
            {
                return;
            }
            state.generations.retain(|other| other.id != link.id);
            state.retired += 1;
        }
        link.shutdown(INTERRUPT_TIMEOUT);
        self.publish_generations();
    }

    /// 读线程发现子进程没了。它手里的工作区清掉：里面的命令已经没人管得了，
    /// 记录按 owner 不在判 `unknown`，下次调用绑给活着的那一代。
    pub(super) fn on_lost(&self, id: u64, reason: &str) {
        let replace = {
            let mut state = self.lock();
            if !state.generations.iter().any(|link| link.id == id) {
                return; // 正常退出的，已经摘掉了。
            }
            state.generations.retain(|link| link.id != id);
            state.owners.retain(|_, owned| owned.generation != id);
            if state.is_prepared(id) {
                state.prepared = None;
                state.last_failure = Some(format!("准备好的新代 g{id} 退出了：{reason}"));
                None
            } else if state.is_current(id) {
                let lost = state.current.take();
                state.last_failure = Some(format!("当前执行代 g{id} 退出了：{reason}"));
                eprintln!("当前执行代 g{id} 退出了（{reason}），重启一个。");
                let start = !state.shutting_down && !state.replacing;
                if start {
                    state.replacing = true;
                }
                start.then_some(lost).flatten()
            } else {
                eprintln!("旧执行代 g{id} 退出了（{reason}）。");
                None
            }
        };
        self.publish_generations();
        if let Some(lost) = replace {
            self.spawn_replacement(lost.exe.clone());
        }
    }

    /// 一直试到成功或者守护进程退出。服务不能因为一次起不来就永远没有执行代。
    fn spawn_replacement(&self, exe: PathBuf) {
        let router = self.arc();
        let spawned = std::thread::Builder::new()
            .name("gld-gen-restart".into())
            .spawn(move || {
                let mut attempt = 0usize;
                loop {
                    std::thread::sleep(RESTART_BACKOFF[attempt.min(RESTART_BACKOFF.len() - 1)]);
                    attempt += 1;
                    {
                        let mut state = router.lock();
                        // 操作员这期间切到了准备好的新代：不用替了。
                        if state.shutting_down || state.current.is_some() {
                            state.replacing = false;
                            return;
                        }
                    }
                    match router.bring_up_replacement(&exe) {
                        Ok(()) => return,
                        Err(reason) => {
                            eprintln!("重启执行代失败（第 {attempt} 次）：{reason}");
                            router.lock().last_failure = Some(reason);
                        }
                    }
                }
            });
        if let Err(error) = spawned {
            eprintln!("起不了重启线程：{error}");
        }
    }

    fn bring_up_replacement(&self, exe: &Path) -> Result<(), String> {
        let id = {
            let mut state = self.lock();
            let id = state.next_id;
            state.next_id += 1;
            id
        };
        let link = Link::spawn(id, exe, self.me.clone(), READY_TIMEOUT)?;
        // 二进制可能已经被换掉（换完还没 restart 时当前代挂了）。兼容就用，不兼容如实说，
        // 不能带着对不上的格式继续写。
        if let Some(reason) = link.identity().incompatibility() {
            link.kill();
            return Err(format!(
                "{} 现在是另一个不兼容的构建（{reason}）。gld daemon restart 换到新版本",
                exe.display()
            ));
        }
        let applied = self.lock().hub_config.clone();
        if let Some(config) = &applied {
            link.call(
                &Op::HubConfigure {
                    config: Box::new(config.clone()),
                    sweep: false,
                },
                Some(CONTROL_TIMEOUT),
            )
            .map_err(|error| {
                link.kill();
                format!("新执行代按当前配置建不起 Hub：{error}")
            })?;
        }
        {
            let mut state = self.lock();
            // 起进程要时间，这中间操作员可能已经切到了新代，这一个不能把路由抢回去。
            if state.shutting_down || state.current.is_some() {
                state.replacing = false;
                drop(state);
                link.kill();
                return Ok(());
            }
            state.replacing = false;
            state.generations.push(Arc::clone(&link));
            state.current = Some(Arc::clone(&link));
            state.restarts += 1;
            state.last_failure = None;
        }
        self.catch_up_hub(&link, &applied);
        self.changed.notify_all();
        self.publish_generations();
        self.notice_if_already_gone(&link);
        Ok(())
    }

    // ---- 状态 ----

    /// 只读。只有计数和标识：没有凭据、token URL、工具参数或命令文本。
    pub fn status(&self) -> Value {
        let (links, owners, current, prepared, restarts, retired, last_failure, hub_running) = {
            let state = self.lock();
            (
                state.generations.clone(),
                state
                    .owners
                    .iter()
                    .map(|(key, owned)| (key.clone(), owned.generation, owned.in_flight))
                    .collect::<Vec<_>>(),
                state.current.as_ref().map(|link| link.id),
                state.prepared.as_ref().map(|link| link.id),
                state.restarts,
                state.retired,
                state.last_failure.clone(),
                state.hub_config.is_some(),
            )
        };
        let generations: Vec<Value> = links
            .iter()
            .map(|link| {
                let role = if Some(link.id) == current {
                    "current"
                } else if Some(link.id) == prepared {
                    "prepared"
                } else {
                    "draining"
                };
                let mut row = generation_row(link, role);
                let mut workspaces: Vec<Value> = owners
                    .iter()
                    .filter(|(_, generation, _)| *generation == link.id)
                    .map(|(key, _, in_flight)| json!({ "workspace": key, "in_flight": in_flight }))
                    .collect();
                workspaces.sort_by_key(|row| row["workspace"].as_str().map(str::to_string));
                // 问不到不算错误：报告本身不能因为一代卡住而卡住。
                match link.call(&Op::Status, Some(CONTROL_TIMEOUT)) {
                    Ok(status) => {
                        row["active_commands"] = status["active_commands"].clone();
                        row["active_agents"] = status["active_agents"].clone();
                        row["commands_by_workspace"] = status["commands_by_workspace"].clone();
                    }
                    Err(error) => row["status_error"] = json!(error.to_string()),
                }
                row["blocked_by"] = json!(blocked_by(role, &row, &workspaces));
                row["workspaces"] = json!(workspaces);
                row
            })
            .collect();
        json!({
            "capability": {
                "generations": true,
                "generation_protocol": GENERATION_PROTOCOL,
                "state_format": STATE_FORMAT,
                "max_generations": MAX_LIVE_GENERATIONS,
                // 下面这几个是这一期还没做到的，别让人从“能切换”推出“零中断”（RFC-0008 §5）。
                "workspace_handoff": false,
                "host_handoff": false,
                "zero_interruption": false,
            },
            "host": {
                "pid": std::process::id(),
                "uptime_secs": self.started.elapsed().as_secs(),
                "identity": BuildIdentity::current(),
                "service_running": hub_running,
            },
            "current": current.map(|id| format!("g{id}")),
            "prepared": prepared.map(|id| format!("g{id}")),
            "generations": generations,
            "restarts": restarts,
            "retired": retired,
            "last_failure": last_failure,
        })
    }
}

impl State {
    fn is_current(&self, id: u64) -> bool {
        self.current.as_ref().is_some_and(|link| link.id == id)
    }

    fn is_prepared(&self, id: u64) -> bool {
        self.prepared.as_ref().is_some_and(|link| link.id == id)
    }

    fn link_by_id(&self, id: u64) -> Option<Arc<Link>> {
        self.generations
            .iter()
            .find(|link| link.id == id && link.is_alive())
            .cloned()
    }
}

/// 这一条请求该进哪一代；要绑就绑上。
fn pick(state: &mut State, key: Option<&str>, discovery: bool) -> Option<Arc<Link>> {
    if discovery {
        // 工具表由最老的一代给：不能先发布新工具，再把调用送给不认识它的旧代。
        return state
            .generations
            .iter()
            .find(|link| link.is_alive() && !state.is_prepared(link.id))
            .cloned();
    }
    let current = state.current.clone().filter(|link| link.is_alive());
    let Some(key) = key else {
        return current;
    };
    if let Some(link) = state
        .owners
        .get(key)
        .and_then(|owned| state.link_by_id(owned.generation))
    {
        return Some(link);
    }
    let current = current?;
    state.owners.insert(
        key.to_string(),
        OwnedWorkspace {
            generation: current.id,
            in_flight: 0,
        },
    );
    Some(current)
}

/// 工作区所有权的键：和 [`crate::tools::workspace_runtime::runtime_for`] 一样规范化路径，
/// 同一个目录不管从哪个入口进来都是同一个主人。
pub fn root_key(root: &Path) -> String {
    root.canonicalize()
        .unwrap_or_else(|_| root.to_path_buf())
        .display()
        .to_string()
}

fn mcp_reply(value: Value) -> Result<McpReply, CallError> {
    let mut value = value;
    Ok(McpReply {
        response: value
            .get_mut("response")
            .map(Value::take)
            .unwrap_or(Value::Null),
        member: value
            .get("member")
            .and_then(Value::as_str)
            .map(str::to_string),
        audit: value
            .get_mut("audit")
            .map(Value::take)
            .unwrap_or(Value::Null),
    })
}

fn tool_names(reply: &Value) -> HashSet<String> {
    reply["tools"]
        .as_array()
        .map(|tools| {
            tools
                .iter()
                .filter_map(|name| name.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn generation_row(link: &Link, role: &str) -> Value {
    let identity = link.identity();
    json!({
        "id": format!("g{}", link.id),
        "role": role,
        "pid": identity.pid,
        "version": identity.version,
        "build_commit": identity.build_commit,
        "generation_protocol": identity.generation_protocol,
        "state_format": identity.state_format,
        "exe": link.exe,
        "in_flight": link.in_flight.load(Ordering::SeqCst),
    })
}

/// 这一代为什么还不能退：给人看的，不包含命令内容。
fn blocked_by(role: &str, row: &Value, workspaces: &[Value]) -> Vec<String> {
    if role != "draining" {
        return Vec::new();
    }
    let mut reasons = Vec::new();
    if row["in_flight"].as_u64().unwrap_or(0) > 0 {
        reasons.push("in_flight_calls".to_string());
    }
    if !workspaces.is_empty() {
        reasons.push("owned_workspaces".to_string());
    }
    if row["active_commands"].as_u64().unwrap_or(0) > 0 {
        reasons.push("running_commands".to_string());
    }
    if row["active_agents"].as_u64().unwrap_or(0) > 0 {
        reasons.push("running_agents".to_string());
    }
    reasons
}
