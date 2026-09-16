//! 远端连接的生命周期：按需打开、一个成员一条、空闲了收掉。
//!
//! hub 路由只管「这次该找谁」，连接活多久是这里的事。
//!
//! 三条规矩来自 RFC-0002 5.4，改代码时哪条都别松：
//!
//! 1. **按需打开，不预热。**ccnm 的 bridge 在 MCP 握手之前就去拿 Runtime 的
//!    写锁了（coding 模式）。hub 一启动就把所有成员连上，等于替所有人先把
//!    远端的写锁占住。
//! 2. **一个成员一条连接，调用串行。**`read_output` 这类引用只在开出它的那条
//!    连接里有效；同一条连接上并发调用，远端的 writer guard 也不允许。
//! 3. **远端 I/O 绝不在全局锁里做。**连接表的锁只用来「拿到某个成员的槽位」，
//!    真正等远端回话时它已经放开了——否则一个 SSH 黑洞会把整个 hub 卡死
//!    （验收项 H05）。
//!
//! ## 预算是多少，为什么是这个数
//!
//! | 名字 | 值 | 为什么 |
//! | --- | --- | --- |
//! | [`HANDSHAKE_TIMEOUT`] | 30 秒 | bridge 要先 SSH 连过去、在对面把进程起起来。本机 ccnm 通常一秒内，跨机看网络 |
//! | [`CALL_TIMEOUT`] | 60 秒 | 只读工具在远端本身是毫秒级，这个预算基本全留给网络和一次读的传输量 |
//! | [`IDLE_AFTER`] | 5 分钟 | 一轮对话里模型连着读文件的间隔远小于它；超过就说明这轮结束了，不该白占一条 SSH |
//! | [`CLOSE_GRACE`] | 5 秒 | 等对面自己收尾释放写锁的时间，到点才动手杀 |
//!
//! coding 另有一个 [`CODING_IDLE_AFTER`]（2 分钟），比只读的短，因为它占着
//! 远端工作树的写锁。握手、单次调用和关闭宽限两种模式共用。
//!
//! ## coding 会话
//!
//! coding 不是"换个模式再调一次"，是一段**写租约**：
//!
//! - [`Connections::begin_coding`] 开一条 `--mode coding` 的连接并发一个随机
//!   句柄。开连接这一步就把远端的写锁拿了（ccnm 协议 4.4），所以别人占着时
//!   这里直接失败，不排队也不降级。
//! - 句柄**不是授权**：每次调用都要把主体、成员、连接代次重新对一遍
//!   （RFC 5.3）。对不上一律报同一个错，不帮人区分"存在但不属于你"。
//! - 传输层一断，会话就结束，**绝不偷偷重开**。重开是新会话，新的
//!   `output_ref` 空间（ccnm 协议 6.3「没有 resume」）；悄悄重开会让调用方
//!   手里的 ref 指向不存在的东西，而它看起来一切正常。
//! - read 和 coding 是**两条独立连接**（键里带模式）。让只读调用去挤 coding
//!   那条的话，一次 `remote_read_file` 就能把别人的写会话连同 `output_ref`
//!   一起弄没。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;

use super::member::{CcnmMember, Mode};
use super::peer::{ChildTransport, Peer, PeerError, Transport};

pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);
pub const CALL_TIMEOUT: Duration = Duration::from_secs(60);
pub const IDLE_AFTER: Duration = Duration::from_secs(5 * 60);
pub const CLOSE_GRACE: Duration = Duration::from_secs(5);

/// coding 会话闲多久就收掉。
///
/// 比只读的 5 分钟短，因为它**占着远端工作树的写锁**：闲着不放等于挡住那台
/// 机器上所有想写这个项目的人（包括 ccnm 自己的 Managed session）。一轮
/// patch → test → 看结果 从不会停两分钟；真停了就说明这轮结束了。
///
/// 跑着的 `exec_command` 不受它影响：调用在途时槽位的锁拿不到，
/// [`Slot::is_idle`] 一律判为"不闲"。
pub const CODING_IDLE_AFTER: Duration = Duration::from_secs(2 * 60);

/// 同一个 coding 会话上已经有调用在跑时，第二个调用等多久才报"忙"。
///
/// 会话内的调用必须串行（RFC-0002 5.4：不能在同一 writer 内并发 patch 与
/// exec），但**串行不等于无限排队**。远端的 `exec_command` 最长能跑 10 分钟，
/// 让第二个请求在锁上干等 10 分钟，Web 那头的 HTTP 早就断了——RFC 5.2 专门
/// 警告过这件事。等一小会儿盖住"前一个刚好要结束"，之后明确回一句"还在跑"，
/// 比挂着强。
const CODING_BUSY_GRACE: Duration = Duration::from_secs(2);

/// 握手时报给远端的客户端名，会进 ccnm 那边的日志。
const CLIENT_NAME: &str = "gld-hub";

/// 怎么把一条通往远端的通道开出来。
///
/// 抽出来是为了测试能塞合成通道进去：连接的开关、代次、空闲回收这些逻辑
/// 不需要真的起 SSH 才能验（验收项 H1「用合成 stdio peer 验证」）。
pub trait Open: Send + Sync {
    fn open(&self, member: &CcnmMember, mode: Mode) -> Result<Box<dyn Transport>, PeerError>;
}

/// 真起一个 `ccnm mcp bridge` 子进程。argv 全部来自成员配置，见
/// [`CcnmMember::bridge_argv`]。
pub struct Spawn;

impl Open for Spawn {
    fn open(&self, member: &CcnmMember, mode: Mode) -> Result<Box<dyn Transport>, PeerError> {
        let (program, args) = member.bridge_argv(mode);
        Ok(Box::new(ChildTransport::spawn(&program, &args)?))
    }
}

/// 一条开着的连接。
struct Live {
    peer: Peer<Box<dyn Transport>>,
    last_used: Instant,
}

/// 丢掉一条连接就等于关掉它：先让对面读到 EOF，再等它退出。
///
/// 写成 `Drop` 而不是到处显式调 close，是因为回收的路径有好几条——空闲到期、
/// 配置变了、成员被移出 hub、hub 关掉——每条都记得关一次迟早会漏一条，
/// 漏掉的后果是远端的写锁留着 `held` 标记，要人工恢复。
impl Drop for Live {
    fn drop(&mut self) {
        self.peer.shutdown(CLOSE_GRACE);
    }
}

/// 连接表的键：哪个成员、哪种模式。
///
/// **模式进键**，所以 read 和 coding 是两条独立的连接，互相不挤掉。只读工具
/// 走 read 那条，coding 工具走 coding 那条。ccnm 的 read 模式不碰写锁，所以
/// 多一条读连接不会挡住谁；反过来如果让只读调用去挤 coding 那条，一次
/// `remote_read_file` 就能把别人的写会话连同 `output_ref` 一起弄没。
type Key = (String, Mode);

/// 一个成员在某个模式下的槽位。
///
/// 里面是 `Option`：槽位先占上、连接后开。这样两个请求同时打到一个还没连上的
/// 成员时，第二个会在槽位的锁上等第一个连完，而不是各起一个 bridge。
struct Slot {
    /// 开这条连接时的主体 + 成员配置 + 模式。变了就是新一代，旧连接作废。
    generation: String,
    connection: Arc<Mutex<Option<Live>>>,
}

impl Slot {
    /// 闲了这么久没人用。**正在用的算不闲**（RFC 5.4：不能因为 `last_used`
    /// 是老的就把 writer 判成空闲）。
    ///
    /// 两道判断都是「有没有别人在用」：
    /// - 除了连接表自己还有人持有这个槽位，说明有请求刚拿走它、马上要用；
    /// - 锁拿不到，说明有调用正在途中。
    fn is_idle(&self, after: Duration) -> bool {
        if Arc::strong_count(&self.connection) > 1 {
            return false;
        }
        match self.connection.try_lock() {
            Ok(guard) => match guard.as_ref() {
                Some(live) => live.last_used.elapsed() >= after,
                // 槽位占着但连接没开起来，留着没用。
                None => true,
            },
            Err(_) => false,
        }
    }
}

/// hub 手里所有远端连接。
pub struct Connections {
    opener: Box<dyn Open>,
    idle_after: Duration,
    coding_idle_after: Duration,
    coding_busy_grace: Duration,
    handshake_timeout: Duration,
    call_timeout: Duration,
    live: Mutex<HashMap<Key, Slot>>,
    /// 还开着的 coding 会话：句柄 → 它属于谁。
    leases: Mutex<HashMap<String, Lease>>,
}

/// 一个 coding 会话。句柄之外的每一项，每次调用都要重新对一遍
/// （RFC-0002 5.3「每次调用重新检查授权与归属」）。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Lease {
    /// 开它的那个主体。**换一个主体拿着同一个句柄也不认。**
    principal: String,
    member_id: String,
    /// 开它时的连接代次。成员配置一变，旧句柄作废。
    generation: String,
}

/// coding 会话出的岔子。
///
/// 跟 [`PeerError`] 分开，因为「句柄不认」和「远端断了」对调用方是两件事：
/// 前者重新 begin 一次就行，后者要先搞清楚远端到底做了没有。
#[derive(Debug)]
pub enum CodingError {
    /// 这个句柄在这里没用：不存在、不是你的、不是这个成员的，或者配置已经变了。
    /// **四种情况报同一个错**，不帮人区分「存在但不属于你」。
    NoSuchSession,
    /// 句柄本来是对的，但它那条连接已经没了（空闲回收、断线、成员被摘掉）。
    /// 没有 resume：要接着写就重新 begin，旧的 `output_ref` 全部作废。
    SessionEnded,
    /// 这个会话上还有一个调用没跑完。会话内串行，不并发。
    Busy,
    Peer(PeerError),
}

impl std::fmt::Display for CodingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CodingError::NoSuchSession => write!(
                f,
                "that coding handle is not open on this workspace; call remote_coding_begin first"
            ),
            CodingError::SessionEnded => write!(
                f,
                "that coding session has ended, so its output_ref values are gone too; \
                 call remote_coding_begin for a new one"
            ),
            CodingError::Busy => write!(
                f,
                "another call is still running on this coding session; remote sessions run one call at a time, so wait for it to finish"
            ),
            CodingError::Peer(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for CodingError {}

impl Connections {
    /// 真连远端用这个。
    pub fn new() -> Self {
        Self::with_opener(Box::new(Spawn))
    }

    pub fn with_opener(opener: Box<dyn Open>) -> Self {
        Connections {
            opener,
            idle_after: IDLE_AFTER,
            coding_idle_after: CODING_IDLE_AFTER,
            coding_busy_grace: CODING_BUSY_GRACE,
            handshake_timeout: HANDSHAKE_TIMEOUT,
            call_timeout: CALL_TIMEOUT,
            live: Mutex::new(HashMap::new()),
            leases: Mutex::new(HashMap::new()),
        }
    }

    /// 改预算。给测试用，也给以后做成配置留个口子。
    pub fn with_budgets(mut self, idle_after: Duration, call_timeout: Duration) -> Self {
        self.idle_after = idle_after;
        self.coding_idle_after = idle_after;
        self.handshake_timeout = call_timeout;
        self.call_timeout = call_timeout;
        self
    }

    /// 会话内第二个调用等多久才报"忙"。默认 [`CODING_BUSY_GRACE`]；
    /// 测试把它调到几十毫秒，免得为了验这条真的等两秒。
    pub fn with_coding_busy_grace(mut self, grace: Duration) -> Self {
        self.coding_busy_grace = grace;
        self
    }

    /// 在这个成员的连接上调一个远端工具，没连就先连。
    ///
    /// `principal` 是调用方的主体标识（`AuthContext::tag()`）。它进代次，
    /// 所以**两个不同的主体拿到的是两条连接**，不是共用一条按成员 id 缓存的
    /// ——RFC-0002 5.3「不能只按 workspace ID 缓存长期 bridge」。它是已经脱敏
    /// 的短标识，不含任何凭据。
    ///
    /// 返回的是远端 MCP result 原样，包括 `isError: true`——那是工具自己说
    /// 这次没成，不是连接出问题（验收项 H04）。
    pub fn call(
        &self,
        principal: &str,
        member: &CcnmMember,
        tool: &str,
        arguments: Value,
    ) -> Result<Value, PeerError> {
        // 只读调用永远走 read 那条连接，哪怕这个成员同时开着 coding 会话。
        // 挤掉 coding 那条的代价是别人的写会话和 output_ref 一起没。
        self.run(principal, member, Mode::Read, tool, arguments)
            .map(|(value, _)| value)
    }

    /// 在指定模式的连接上跑一次调用。
    ///
    /// 第二个返回值是「这条连接还在不在」：传输层出岔子时是 `false`，调用方
    /// 据此决定要不要把依赖这条连接的东西（coding 句柄）一起作废。
    fn run(
        &self,
        principal: &str,
        member: &CcnmMember,
        mode: Mode,
        tool: &str,
        arguments: Value,
    ) -> Result<(Value, bool), PeerError> {
        let slot = self.slot(
            &(member.id.clone(), mode),
            &generation_of(principal, member, mode),
        );
        let guard = slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        self.run_locked(guard, member, mode, tool, arguments)
    }

    /// 已经拿到槽位锁之后的那一半。
    fn run_locked(
        &self,
        mut guard: std::sync::MutexGuard<'_, Option<Live>>,
        member: &CcnmMember,
        mode: Mode,
        tool: &str,
        arguments: Value,
    ) -> Result<(Value, bool), PeerError> {
        if guard.is_none() {
            *guard = Some(Live {
                peer: self.handshake(member, mode)?,
                last_used: Instant::now(),
            });
        }
        let live = guard.as_mut().expect("刚刚连上");
        let result = live.peer.call_tool(tool, arguments, self.call_timeout);
        match result {
            // 远端明确回了 JSON-RPC error 也算这条连接好着，下次还能用。
            Ok(value) => {
                live.last_used = Instant::now();
                Ok((value, true))
            }
            Err(PeerError::Remote {
                method,
                code,
                message,
            }) => {
                live.last_used = Instant::now();
                Err(PeerError::Remote {
                    method,
                    code,
                    message,
                })
            }
            // 超时、写不进去、对面关了、回了看不懂的东西——传输层已经不可信，
            // 留着它下一次调用会读到上一次的残留回复。丢掉，下次重连。
            Err(other) => {
                guard.take();
                Err(other)
            }
        }
    }

    /// 开一个 coding 会话，返回句柄。
    ///
    /// **这一步就把远端工作树的写锁拿了**——ccnm 的 coding server 在 MCP 握手
    /// 之前就去抢那把锁（协议 4.4）。所以别人占着的时候这里直接失败，不排队、
    /// 不静默降级成只读。
    ///
    /// 句柄是随机的 UUID：模型猜不出别人的句柄，但**它不是授权**——每次调用
    /// 还要把主体、成员和代次重新对一遍（RFC 5.3）。
    pub fn begin_coding(&self, principal: &str, member: &CcnmMember) -> Result<String, PeerError> {
        let generation = generation_of(principal, member, Mode::Coding);
        let key = (member.id.clone(), Mode::Coding);
        // 先把连接建起来：拿不到远端写锁就在这里失败，不会留下一个空句柄。
        let slot = self.slot(&key, &generation);
        {
            let mut guard = slot.lock().unwrap_or_else(|p| p.into_inner());
            if guard.is_none() {
                *guard = Some(Live {
                    peer: self.handshake(member, Mode::Coding)?,
                    last_used: Instant::now(),
                });
            }
        }
        let handle = format!("rc-{}", uuid::Uuid::new_v4().simple());
        self.leases
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(
                handle.clone(),
                Lease {
                    principal: principal.to_string(),
                    member_id: member.id.clone(),
                    generation,
                },
            );
        Ok(handle)
    }

    /// 在一个 coding 会话上跑一次调用。
    ///
    /// 句柄对不上就是 [`CodingError::NoSuchSession`]——**不去猜、不去新开一个**。
    /// 新开是新会话：新的保留输出目录、新的 `output_ref` 空间（ccnm 协议 6.3
    /// 「没有 resume」）。悄悄重开会让调用方手里的 `output_ref` 指向不存在的
    /// 东西，而它看起来一切正常。
    pub fn call_coding(
        &self,
        principal: &str,
        member: &CcnmMember,
        handle: &str,
        tool: &str,
        arguments: Value,
    ) -> Result<Value, CodingError> {
        let generation = generation_of(principal, member, Mode::Coding);
        self.check_lease(principal, member, handle, &generation)?;
        let slot = self.slot(&(member.id.clone(), Mode::Coding), &generation);
        // 会话内串行：拿不到锁说明上一个调用还在跑。等一小会儿，之后明确
        // 回"忙"，不无限排队——见 [`CODING_BUSY_GRACE`]。
        let Some(guard) = lock_within(&slot, self.coding_busy_grace) else {
            return Err(CodingError::Busy);
        };
        match self.run_locked(guard, member, Mode::Coding, tool, arguments) {
            Ok((value, _)) => Ok(value),
            Err(error) => {
                // 传输层断了：这个会话没了，不能让句柄继续看着有效。
                if !matches!(error, PeerError::Remote { .. }) {
                    self.forget_lease(handle);
                }
                Err(CodingError::Peer(error))
            }
        }
    }

    /// 关掉一个 coding 会话，释放远端写锁。
    ///
    /// 幂等：已经没了的句柄不算错——调用方重试一次关闭不该拿到失败。
    pub fn end_coding(&self, principal: &str, member: &CcnmMember, handle: &str) {
        let generation = generation_of(principal, member, Mode::Coding);
        if self
            .check_lease(principal, member, handle, &generation)
            .is_err()
        {
            return;
        }
        self.forget_lease(handle);
        let dropped = self
            .live
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&(member.id.clone(), Mode::Coding));
        // 关闭（等子进程退出）在放开全局锁之后。
        drop(dropped);
    }

    /// 这个句柄现在还认不认。
    fn check_lease(
        &self,
        principal: &str,
        member: &CcnmMember,
        handle: &str,
        generation: &str,
    ) -> Result<(), CodingError> {
        let leases = self.leases.lock().unwrap_or_else(|p| p.into_inner());
        let Some(lease) = leases.get(handle) else {
            return Err(CodingError::NoSuchSession);
        };
        // 三项逐一对。任何一项不符都报同一个错：告诉调用方"这个句柄在这里
        // 没用"，不告诉它"存在但不属于你"——那是在帮人枚举别人的会话。
        if lease.principal != principal
            || lease.member_id != member.id
            || lease.generation != generation
        {
            return Err(CodingError::NoSuchSession);
        }
        drop(leases);
        // 连接被空闲回收或者代次变过：句柄还在表里，但它指的那条连接没了。
        let live = self.live.lock().unwrap_or_else(|p| p.into_inner());
        match live.get(&(member.id.clone(), Mode::Coding)) {
            Some(slot) if slot.generation == generation => Ok(()),
            _ => {
                drop(live);
                self.forget_lease(handle);
                Err(CodingError::SessionEnded)
            }
        }
    }

    fn forget_lease(&self, handle: &str) {
        self.leases
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(handle);
    }

    /// 现在开着几个 coding 会话。给测试和诊断用。
    pub fn coding_count(&self) -> usize {
        self.leases.lock().unwrap_or_else(|p| p.into_inner()).len()
    }

    /// 只留这些成员的连接，其余全收掉。
    ///
    /// 成员被移出 hub、被删掉、权限收紧之后要调它——RFC 5.1「成员删除/权限
    /// 收紧/配置变化须使对应旧会话失效」。
    pub fn retain(&self, keep: &[String]) {
        let dropped: Vec<Slot> = {
            let mut live = self.live.lock().unwrap_or_else(|p| p.into_inner());
            let gone: Vec<Key> = live
                .keys()
                .filter(|(id, _)| !keep.iter().any(|kept| kept == id))
                .cloned()
                .collect();
            gone.iter().filter_map(|key| live.remove(key)).collect()
        };
        // 被摘掉的成员，它的 coding 句柄也不能再认——RFC 5.1「成员删除/权限
        // 收紧须使对应旧会话失效」。
        self.leases
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .retain(|_, lease| keep.iter().any(|kept| kept == &lease.member_id));
        // 关闭动作（等子进程退出）在放开全局锁之后才发生。
        drop(dropped);
    }

    /// hub 停掉时收掉全部连接。
    pub fn close_all(&self) {
        self.retain(&[]);
    }

    /// 现在有几个成员占着槽位。给测试和诊断用。
    pub fn open_count(&self) -> usize {
        self.live.lock().unwrap_or_else(|p| p.into_inner()).len()
    }

    /// 拿到这个成员的槽位，顺手把别人的空闲连接收掉。
    ///
    /// 不另起后台线程扫：hub 本来就是有请求才醒，没请求的时候留着一条 SSH
    /// 也没人会被它挡住；有请求了就是回收的时机。
    fn slot(&self, want: &Key, generation: &str) -> Arc<Mutex<Option<Live>>> {
        let mut detached: Vec<Slot> = Vec::new();
        let mut reclaimed: Vec<String> = Vec::new();
        let slot = {
            let mut live = self.live.lock().unwrap_or_else(|p| p.into_inner());
            let idle: Vec<Key> = live
                .iter()
                .filter(|(key, slot)| *key != want && slot.is_idle(self.idle_for(key.1)))
                .map(|(key, _)| key.clone())
                .collect();
            for key in idle {
                if key.1 == Mode::Coding {
                    reclaimed.push(key.0.clone());
                }
                detached.extend(live.remove(&key));
            }
            match live.get(want) {
                Some(slot) if slot.generation == generation => slot.connection.clone(),
                _ => {
                    // 配置或主体变了：旧连接是按旧权限开的，不能接着用。
                    detached.extend(live.remove(want));
                    let connection = Arc::new(Mutex::new(None));
                    live.insert(
                        want.clone(),
                        Slot {
                            generation: generation.to_string(),
                            connection: connection.clone(),
                        },
                    );
                    connection
                }
            }
        };
        // 回收掉的 coding 连接，它的句柄也跟着作废：没有 resume，句柄留着
        // 只会让调用方以为自己的 output_ref 还在。
        if !reclaimed.is_empty() {
            self.leases
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .retain(|_, lease| !reclaimed.contains(&lease.member_id));
        }
        drop(detached);
        slot
    }

    /// 这种模式的连接闲多久算闲。coding 短一些，它占着远端写锁。
    fn idle_for(&self, mode: Mode) -> Duration {
        match mode {
            Mode::Read => self.idle_after,
            Mode::Coding => self.coding_idle_after,
        }
    }

    /// 开一条连接并握手。握手不成就把刚起的进程收掉，不留孤儿。
    fn handshake(
        &self,
        member: &CcnmMember,
        mode: Mode,
    ) -> Result<Peer<Box<dyn Transport>>, PeerError> {
        let transport = self.opener.open(member, mode)?;
        let mut peer = Peer::new(transport);
        match peer.initialize(CLIENT_NAME, self.handshake_timeout) {
            Ok(_) => Ok(peer),
            Err(error) => {
                peer.shutdown(CLOSE_GRACE);
                Err(error)
            }
        }
    }
}

impl Default for Connections {
    fn default() -> Self {
        Self::new()
    }
}

/// 在 `grace` 之内拿到锁，拿不到就返回 `None`。
///
/// `std::sync::Mutex` 没有带超时的 lock，所以是轮询。间隔 20 毫秒：一次远端
/// 调用最快也要几十毫秒，比这更密只是白转 CPU。
fn lock_within(
    slot: &Arc<Mutex<Option<Live>>>,
    grace: Duration,
) -> Option<std::sync::MutexGuard<'_, Option<Live>>> {
    let deadline = Instant::now() + grace;
    loop {
        match slot.try_lock() {
            Ok(guard) => return Some(guard),
            // 上一个持有者 panic 了。锁里的东西可能是半截状态，但这里唯一
            // 的"半截"是那条连接，而它坏了会在下一次调用时被丢掉。
            Err(std::sync::TryLockError::Poisoned(poisoned)) => return Some(poisoned.into_inner()),
            Err(std::sync::TryLockError::WouldBlock) => {
                if Instant::now() >= deadline {
                    return None;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
}

/// 一条连接的「代次」：开它时的主体、成员配置和模式。
///
/// 三样里任何一样变了，旧连接就不能再用——它是按旧主体、旧配置、旧权限
/// 开出来的。
///
/// 把整个成员序列化进去而不是只取几个字段：以后给 `CcnmMember` 加字段时，
/// 忘了同步这里的后果是操作员改了配置、hub 却还用着按旧配置开的连接。
fn generation_of(principal: &str, member: &CcnmMember, mode: Mode) -> String {
    serde_json::to_string(&(principal, member, mode.as_str())).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc::{self, RecvTimeoutError};

    /// 合成通道：握手照答，`tools/call` 回一句话，记下自己被关过没有。
    struct Fake {
        opened_with: Vec<String>,
        pending: Option<String>,
        closed: Arc<AtomicUsize>,
        /// 调用一律超时，用来验「传输层坏了就丢连接」。
        black_hole: bool,
        hold: Option<Hold>,
        tool_calls: Arc<AtomicUsize>,
    }

    /// 通道这头：第一次工具调用进门先报一声，然后停住，等测试放行才回。
    struct Hold {
        entered: mpsc::Sender<()>,
        release: mpsc::Receiver<()>,
    }

    /// 测试那头：`entered` 收到信号时，第一个调用正拿着会话锁停在远端；
    /// 丢掉 `release` 就放它回来。
    struct Held {
        entered: mpsc::Receiver<()>,
        release: mpsc::Sender<()>,
    }

    /// 停住的调用最多等这么久。只有测试本身出问题时才会等满：比如实现退化成
    /// 排队，第二个调用会一直等锁，到点放行后它拿到锁、断言失败，而不是让整个
    /// `cargo test` 挂住。
    const HOLD_AT_MOST: Duration = Duration::from_secs(10);

    impl Transport for Fake {
        fn send_line(&mut self, line: &str) -> std::io::Result<()> {
            self.opened_with.push(line.to_string());
            let request: Value = serde_json::from_str(line).expect("请求是 JSON");
            let Some(id) = request.get("id").cloned() else {
                return Ok(()); // 通知，没有回复
            };
            let method = request["method"].as_str().unwrap_or("");
            if method == "tools/call" {
                self.tool_calls.fetch_add(1, Ordering::SeqCst);
                if let Some(hold) = self.hold.take() {
                    let _ = hold.entered.send(());
                    let _ = hold.release.recv_timeout(HOLD_AT_MOST);
                }
                if self.black_hole {
                    return Ok(());
                }
            }
            let result = match method {
                "initialize" => json!({
                    "protocolVersion": super::super::peer::PROTOCOL_VERSION,
                    "serverInfo": { "name": "ccnm" }
                }),
                _ => json!({
                    "content": [{ "type": "text", "text": request["params"]["name"] }],
                    "isError": false
                }),
            };
            self.pending =
                Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string());
            Ok(())
        }

        fn recv_line(&mut self, timeout: Duration) -> Result<Option<String>, RecvTimeoutError> {
            match self.pending.take() {
                Some(line) => Ok(Some(line)),
                None => {
                    std::thread::sleep(timeout.min(Duration::from_millis(20)));
                    Err(RecvTimeoutError::Timeout)
                }
            }
        }

        fn shutdown(&mut self, _grace: Duration) {
            self.closed.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// 记下开了几次、每次拿到的 argv 是什么。
    struct Recording {
        argv: Mutex<Vec<(String, Vec<String>)>>,
        closed: Arc<AtomicUsize>,
        black_hole: bool,
        /// 交给下一条开出来的连接，用来制造"上一个还在跑"。
        hold: Mutex<Option<Hold>>,
        /// 真正到达远端的工具调用次数。
        tool_calls: Arc<AtomicUsize>,
    }

    impl Recording {
        fn new() -> Arc<Self> {
            Arc::new(Recording {
                argv: Mutex::new(Vec::new()),
                closed: Arc::new(AtomicUsize::new(0)),
                black_hole: false,
                hold: Mutex::new(None),
                tool_calls: Arc::new(AtomicUsize::new(0)),
            })
        }
        /// 让下一条连接上的第一次工具调用停在远端，直到测试放行。
        fn hold_first_call(&self) -> Held {
            let (entered_tx, entered_rx) = mpsc::channel();
            let (release_tx, release_rx) = mpsc::channel();
            *self.hold.lock().expect("hold") = Some(Hold {
                entered: entered_tx,
                release: release_rx,
            });
            Held {
                entered: entered_rx,
                release: release_tx,
            }
        }
        fn opens(&self) -> usize {
            self.argv.lock().expect("argv").len()
        }
        fn closes(&self) -> usize {
            self.closed.load(Ordering::SeqCst)
        }
        fn tool_calls(&self) -> usize {
            self.tool_calls.load(Ordering::SeqCst)
        }
    }

    impl Open for Arc<Recording> {
        fn open(&self, member: &CcnmMember, mode: Mode) -> Result<Box<dyn Transport>, PeerError> {
            self.argv
                .lock()
                .expect("argv")
                .push(member.bridge_argv(mode));
            Ok(Box::new(Fake {
                opened_with: Vec::new(),
                pending: None,
                closed: self.closed.clone(),
                black_hole: self.black_hole,
                hold: self.hold.lock().expect("hold").take(),
                tool_calls: self.tool_calls.clone(),
            }))
        }
    }

    /// 测试里统一用这一个主体；只有专门验"按主体分连接"的那条会换。
    const ANYONE: &str = "bearer:hub";

    fn member(id: &str) -> CcnmMember {
        CcnmMember {
            id: id.into(),
            name: id.into(),
            ccnm_bin: "ccnm".into(),
            node: "work".into(),
            workspace: "proj".into(),
            max_mode: Mode::Read,
        }
    }

    fn connections(recorder: &Arc<Recording>) -> Connections {
        Connections::with_opener(Box::new(recorder.clone()))
            .with_budgets(IDLE_AFTER, Duration::from_millis(200))
    }

    /// 第一次调用才连；第二次复用同一条，不重连。
    #[test]
    fn a_connection_opens_on_the_first_call_and_is_reused() {
        let recorder = Recording::new();
        let pool = connections(&recorder);
        assert_eq!(recorder.opens(), 0, "还没人调用就不该连");

        for _ in 0..3 {
            let result = pool
                .call(ANYONE, &member("m1"), "read_file", json!({}))
                .expect("调用");
            assert_eq!(result["content"][0]["text"], json!("read_file"));
        }
        assert_eq!(recorder.opens(), 1, "三次调用只该连一次");
        assert_eq!(pool.open_count(), 1);
    }

    /// 两个成员各连各的：一个成员的连接不会被拿去服务另一个。
    #[test]
    fn each_member_gets_its_own_connection() {
        let recorder = Recording::new();
        let pool = connections(&recorder);
        pool.call(ANYONE, &member("m1"), "read_file", json!({}))
            .expect("m1");
        pool.call(ANYONE, &member("m2"), "read_file", json!({}))
            .expect("m2");
        assert_eq!(recorder.opens(), 2);
        assert_eq!(pool.open_count(), 2);
    }

    /// 成员配置改了，旧连接必须作废重开——它是按旧配置、旧权限开的。
    #[test]
    fn changing_the_member_config_opens_a_new_generation() {
        let recorder = Recording::new();
        let pool = connections(&recorder);
        let mut m = member("m1");
        pool.call(ANYONE, &m, "read_file", json!({})).expect("先");

        m.workspace = "another-project".into();
        pool.call(ANYONE, &m, "read_file", json!({})).expect("后");

        assert_eq!(recorder.opens(), 2, "换了 workspace 却在复用旧连接");
        assert_eq!(recorder.closes(), 1, "旧连接没被关掉");
        let argv = recorder.argv.lock().expect("argv");
        assert_eq!(argv[0].1[2], "proj");
        assert_eq!(argv[1].1[2], "another-project");
    }

    /// 换一个主体也是新一代：一条 bridge 属于某个主体，不是属于某个成员 id
    /// （RFC-0002 5.3「不能只按 workspace ID 缓存长期 bridge」）。
    #[test]
    fn another_principal_does_not_inherit_the_connection() {
        let recorder = Recording::new();
        let pool = connections(&recorder);
        let m = member("m1");
        pool.call("oauth:hub:chatgpt", &m, "read_file", json!({}))
            .expect("第一个主体");
        pool.call("oauth:hub:claude", &m, "read_file", json!({}))
            .expect("另一个主体");
        assert_eq!(recorder.opens(), 2, "两个主体共用了一条 bridge");
        assert_eq!(recorder.closes(), 1, "旧主体那条没被关掉");

        // 同一个主体再来还是复用，不是每次都重开。
        pool.call("oauth:hub:claude", &m, "read_file", json!({}))
            .expect("同一个主体");
        assert_eq!(recorder.opens(), 2);
    }

    /// read 和 coding 是**两条**连接，不是一条换模式。
    ///
    /// 合成一条的代价：一次只读调用就会把别人的写会话连同 output_ref 一起
    /// 挤掉，而调用方看起来一切正常。
    #[test]
    fn reading_and_coding_are_two_separate_connections() {
        let recorder = Recording::new();
        let pool = connections(&recorder);
        let mut m = member("m1");
        m.max_mode = Mode::Coding;

        pool.call(ANYONE, &m, "read_file", json!({})).expect("读");
        let handle = pool.begin_coding(ANYONE, &m).expect("开写会话");
        assert_eq!(recorder.opens(), 2, "该是两条连接");

        // 再读一次：走的还是那条 read 连接，不重开，也不碰写会话。
        pool.call(ANYONE, &m, "read_file", json!({})).expect("再读");
        assert_eq!(recorder.opens(), 2, "只读调用不该动 coding 那条");
        pool.call_coding(ANYONE, &m, &handle, "apply_patch", json!({}))
            .expect("写会话还在");
        assert_eq!(recorder.closes(), 0, "谁也不该被挤掉");
    }

    /// 闲过期限的连接在下一次请求时被收掉，不是等到进程退出。
    #[test]
    fn an_idle_connection_is_closed_on_the_next_request() {
        let recorder = Recording::new();
        let pool = Connections::with_opener(Box::new(recorder.clone()))
            .with_budgets(Duration::from_millis(30), Duration::from_millis(200));
        pool.call(ANYONE, &member("idle"), "read_file", json!({}))
            .expect("idle 成员");
        assert_eq!(pool.open_count(), 1);

        std::thread::sleep(Duration::from_millis(60));
        pool.call(ANYONE, &member("busy"), "read_file", json!({}))
            .expect("另一个成员");

        assert_eq!(
            pool.open_count(),
            1,
            "过期的那条该被收掉：{}",
            pool.open_count()
        );
        assert_eq!(recorder.closes(), 1, "收掉了却没关");
    }

    /// 超时之后这条连接不能再用：下一次调用要重连，不能接着读上一次的残留。
    #[test]
    fn a_transport_failure_throws_the_connection_away() {
        let mut recorder = Recording::new();
        Arc::get_mut(&mut recorder).expect("独占").black_hole = true;
        let pool = connections(&recorder);

        let err = pool
            .call(ANYONE, &member("m1"), "read_file", json!({}))
            .expect_err("应该超时");
        assert!(matches!(err, PeerError::Timeout { .. }), "{err}");
        assert_eq!(recorder.closes(), 1, "坏掉的连接没被关");

        let _ = pool.call(ANYONE, &member("m1"), "read_file", json!({}));
        assert_eq!(recorder.opens(), 2, "没重连，还在用那条坏的");
    }

    /// 成员被移出 hub：连接立刻收掉，不等它自己闲过期。
    #[test]
    fn a_member_that_left_the_hub_loses_its_connection() {
        let recorder = Recording::new();
        let pool = connections(&recorder);
        pool.call(ANYONE, &member("stays"), "read_file", json!({}))
            .expect("stays");
        pool.call(ANYONE, &member("leaves"), "read_file", json!({}))
            .expect("leaves");

        pool.retain(&["stays".to_string()]);

        assert_eq!(pool.open_count(), 1);
        assert_eq!(recorder.closes(), 1);
        pool.close_all();
        assert_eq!(pool.open_count(), 0);
        assert_eq!(recorder.closes(), 2);
    }

    // ---- coding 会话 ----

    fn coding_member(id: &str) -> CcnmMember {
        let mut m = member(id);
        m.max_mode = Mode::Coding;
        m
    }

    /// 句柄是随机的，两次开出来不一样——模型猜不到别人的。
    #[test]
    fn every_session_gets_its_own_unguessable_handle() {
        let recorder = Recording::new();
        let pool = connections(&recorder);
        let m = coding_member("m1");
        let first = pool.begin_coding(ANYONE, &m).expect("第一次");
        pool.end_coding(ANYONE, &m, &first);
        let second = pool.begin_coding(ANYONE, &m).expect("第二次");
        assert_ne!(first, second);
        assert!(first.starts_with("rc-") && first.len() > 20, "{first}");
    }

    /// 句柄不是授权：主体、成员、代次每次都重对一遍（RFC-0002 5.3）。
    /// 三种不符报**同一个**错，不帮人区分"存在但不属于你"。
    #[test]
    fn a_handle_is_not_authority_on_its_own() {
        let recorder = Recording::new();
        let pool = connections(&recorder);
        let m = coding_member("m1");
        let other = coding_member("m2");
        let handle = pool.begin_coding("bearer:hub", &m).expect("开会话");

        for (label, principal, member) in [
            ("换个主体", "oauth:hub:someone-else", &m),
            ("换个成员", "bearer:hub", &other),
        ] {
            let err = pool
                .call_coding(principal, member, &handle, "apply_patch", json!({}))
                .expect_err(label);
            assert!(matches!(err, CodingError::NoSuchSession), "{label}: {err}");
        }
        // 编一个句柄同样不认。
        let err = pool
            .call_coding("bearer:hub", &m, "rc-made-up", "apply_patch", json!({}))
            .expect_err("编的句柄");
        assert!(matches!(err, CodingError::NoSuchSession), "{err}");

        // 原主原成员照常能用。
        pool.call_coding("bearer:hub", &m, &handle, "apply_patch", json!({}))
            .expect("自己的句柄还好着");
    }

    /// 成员配置改了，旧句柄作废——它是按旧配置、旧权限开的。
    #[test]
    fn changing_the_member_config_invalidates_the_handle() {
        let recorder = Recording::new();
        let pool = connections(&recorder);
        let mut m = coding_member("m1");
        let handle = pool.begin_coding(ANYONE, &m).expect("开会话");

        m.workspace = "another-project".into();
        let err = pool
            .call_coding(ANYONE, &m, &handle, "apply_patch", json!({}))
            .expect_err("该不认了");
        assert!(matches!(err, CodingError::NoSuchSession), "{err}");
    }

    /// 传输层断了：会话就此结束，**不偷偷重开**。
    ///
    /// 重开是新会话（ccnm 协议 6.3「没有 resume」）：新的保留输出目录、新的
    /// output_ref 空间。悄悄重开会让调用方手里的 output_ref 指向不存在的东西，
    /// 而它看起来一切正常。
    #[test]
    fn a_broken_connection_ends_the_session_instead_of_silently_reopening() {
        let mut recorder = Recording::new();
        Arc::get_mut(&mut recorder).expect("独占").black_hole = true;
        let pool = connections(&recorder);
        let m = coding_member("m1");
        let handle = pool.begin_coding(ANYONE, &m).expect("开会话");
        assert_eq!(pool.coding_count(), 1);

        let err = pool
            .call_coding(ANYONE, &m, &handle, "exec_command", json!({}))
            .expect_err("该超时");
        assert!(
            matches!(err, CodingError::Peer(PeerError::Timeout { .. })),
            "{err}"
        );

        // 同一个句柄再用：不认了，而且没有偷偷开第二条连接。
        let opens_before = recorder.opens();
        let again = pool
            .call_coding(ANYONE, &m, &handle, "exec_command", json!({}))
            .expect_err("句柄该没了");
        assert!(matches!(again, CodingError::NoSuchSession), "{again}");
        assert_eq!(recorder.opens(), opens_before, "不该偷偷重开");
        assert_eq!(pool.coding_count(), 0);
    }

    /// 关掉会话要真的把连接关掉，写锁得还回去。幂等：重复关不报错。
    #[test]
    fn ending_a_session_closes_the_connection_and_is_idempotent() {
        let recorder = Recording::new();
        let pool = connections(&recorder);
        let m = coding_member("m1");
        let handle = pool.begin_coding(ANYONE, &m).expect("开会话");
        assert_eq!(recorder.closes(), 0);

        pool.end_coding(ANYONE, &m, &handle);
        assert_eq!(recorder.closes(), 1, "连接没关，远端写锁还占着");
        assert_eq!(pool.coding_count(), 0);

        pool.end_coding(ANYONE, &m, &handle);
        assert_eq!(recorder.closes(), 1, "重复关不该再动一次");

        let err = pool
            .call_coding(ANYONE, &m, &handle, "apply_patch", json!({}))
            .expect_err("关了就不能再用");
        assert!(matches!(err, CodingError::NoSuchSession), "{err}");
    }

    /// 会话闲过期限被回收之后，句柄报的是"会话结束了"，不是"句柄不对"——
    /// 这两句话对调用方的意思不一样：后者是它拿错了，前者是它得重新开一个。
    #[test]
    fn an_idle_session_is_reclaimed_and_says_so() {
        let recorder = Recording::new();
        let pool = Connections::with_opener(Box::new(recorder.clone()))
            .with_budgets(Duration::from_millis(30), Duration::from_millis(200));
        let m = coding_member("m1");
        let handle = pool.begin_coding(ANYONE, &m).expect("开会话");

        std::thread::sleep(Duration::from_millis(60));
        // 另一个成员的请求触发回收扫描。
        pool.call(ANYONE, &member("someone-else"), "read_file", json!({}))
            .expect("别人的调用");

        let err = pool
            .call_coding(ANYONE, &m, &handle, "apply_patch", json!({}))
            .expect_err("该被回收了");
        assert!(
            matches!(err, CodingError::NoSuchSession | CodingError::SessionEnded),
            "{err}"
        );
        assert_eq!(pool.coding_count(), 0, "句柄不能留着");
        assert!(recorder.closes() >= 1, "连接该关掉，写锁该还回去");
    }

    /// 成员被移出 hub：连接和句柄一起作废。
    #[test]
    fn removing_the_member_also_kills_its_coding_handle() {
        let recorder = Recording::new();
        let pool = connections(&recorder);
        let m = coding_member("m1");
        let handle = pool.begin_coding(ANYONE, &m).expect("开会话");

        pool.retain(&[]);

        assert_eq!(pool.coding_count(), 0);
        let err = pool
            .call_coding(ANYONE, &m, &handle, "apply_patch", json!({}))
            .expect_err("成员都没了");
        assert!(matches!(err, CodingError::NoSuchSession), "{err}");
    }

    /// 开会话用的是 `--mode coding` 的 argv，跟只读那条分得清清楚楚。
    #[test]
    fn a_coding_session_asks_the_bridge_for_coding_mode() {
        let recorder = Recording::new();
        let pool = connections(&recorder);
        let m = coding_member("m1");
        pool.begin_coding(ANYONE, &m).expect("开会话");
        let argv = recorder.argv.lock().expect("argv");
        assert!(
            argv[0]
                .1
                .ends_with(&["--mode".to_string(), "coding".to_string()]),
            "{:?}",
            argv[0].1
        );
    }

    /// 成员上限是只读时，连接层照样只会开 read——`bridge_argv` 那道取低还在。
    /// hub 会更早一步拦掉，这里钉的是"就算漏过来了也升不了权"。
    #[test]
    fn a_read_only_member_cannot_be_opened_for_coding() {
        let recorder = Recording::new();
        let pool = connections(&recorder);
        let m = member("m1"); // max_mode = Read
        pool.begin_coding(ANYONE, &m).expect("连接本身能建");
        let argv = recorder.argv.lock().expect("argv");
        assert!(
            argv[0]
                .1
                .ends_with(&["--mode".to_string(), "read".to_string()]),
            "只读成员绝不能被开成 coding：{:?}",
            argv[0].1
        );
    }

    /// 同一个会话上的两个调用**串行**：第二个等一小会儿之后明确回"忙"，
    /// 而且**根本没发到远端**（RFC-0002 5.4：不能在同一 writer 内并发
    /// patch 与 exec）。
    ///
    /// 先后顺序靠 `entered` 信号定，不能靠睡。这里原来是第一个调用在远端睡
    /// 300 毫秒、测试睡 60 毫秒再发第二个，赌的是"60 毫秒后第一个已经拿着锁、
    /// 而且还没放"。CI 的 macOS runner 核少、调度抖动大，线程晚两三百毫秒被
    /// 调度就会错开：第一个起晚了，第二个先跑完；或者第二个起晚了，第一个已经
    /// 放锁。两个调用都成功，挂在"后到的该被挡住"上。
    #[test]
    fn a_second_call_on_one_session_waits_briefly_then_says_busy() {
        let recorder = Recording::new();
        let held = recorder.hold_first_call();
        let pool = Connections::with_opener(Box::new(recorder.clone()))
            .with_coding_busy_grace(Duration::from_millis(40));
        let m = coding_member("m1");
        let handle = pool.begin_coding(ANYONE, &m).expect("开会话");

        let (first, second) = std::thread::scope(|scope| {
            let a =
                scope.spawn(|| pool.call_coding(ANYONE, &m, &handle, "exec_command", json!({})));
            held.entered
                .recv_timeout(HOLD_AT_MOST)
                .expect("第一个调用没到远端");
            let b = pool.call_coding(ANYONE, &m, &handle, "apply_patch", json!({}));
            drop(held.release);
            (a.join().expect("第一个"), b)
        });

        assert!(first.is_ok(), "先到的该正常跑完：{first:?}");
        let err = second.expect_err("后到的该被挡住");
        assert!(matches!(err, CodingError::Busy), "{err}");
        assert_eq!(recorder.tool_calls(), 1, "被挡住的那个不该也发到远端");

        // "忙"是暂时的，不是会话作废了：前一个完事之后照样能用。
        pool.call_coding(ANYONE, &m, &handle, "apply_patch", json!({}))
            .expect("前一个跑完就该通了");
        assert_eq!(recorder.tool_calls(), 2);
    }

    /// 等的是**一小会儿**，不是无限排队。远端 exec 最长 10 分钟，挂在锁上
    /// 那么久，Web 那头的 HTTP 早断了。
    ///
    /// 顺序同样靠信号定，原因见上一条。两头都不用墙钟上限：
    /// - 不排队：第一个调用停在远端，直到第二个返回才放行。第二个能回来"忙"，
    ///   就说明它是自己放弃的；排队的话它得等放行、拿到锁、成功返回。
    /// - 真等了一小会儿：至少等满配置的宽限，盖住"前一个刚好要结束"。
    ///   `lock_within` 过了期限才放弃，这个下限多慢的机器上都成立。
    ///
    /// 原来断言"等了不到 400 毫秒"，想借时间证明没排在睡 600 毫秒的第一个调用
    /// 后面；机器一卡，40 毫秒的轮询就能被拖过 400 毫秒，照样误报。
    #[test]
    fn the_busy_wait_is_bounded() {
        let grace = Duration::from_millis(40);
        let recorder = Recording::new();
        let held = recorder.hold_first_call();
        let pool =
            Connections::with_opener(Box::new(recorder.clone())).with_coding_busy_grace(grace);
        let m = coding_member("m1");
        let handle = pool.begin_coding(ANYONE, &m).expect("开会话");

        std::thread::scope(|scope| {
            scope.spawn(|| {
                let _ = pool.call_coding(ANYONE, &m, &handle, "exec_command", json!({}));
            });
            held.entered
                .recv_timeout(HOLD_AT_MOST)
                .expect("第一个调用没到远端");
            let started = Instant::now();
            let second = pool.call_coding(ANYONE, &m, &handle, "apply_patch", json!({}));
            let waited = started.elapsed();
            drop(held.release);
            let err = second.expect_err("第一个还停在远端，第二个该自己放弃而不是排队");
            assert!(matches!(err, CodingError::Busy), "{err}");
            assert!(waited >= grace, "只等了 {waited:?} 就报忙，没等满宽限");
        });
    }

    /// 开的是公开的 bridge 子命令，argv 全来自配置。
    #[test]
    fn the_bridge_is_started_with_the_configured_argv_only() {
        let recorder = Recording::new();
        let pool = connections(&recorder);
        pool.call(ANYONE, &member("m1"), "read_file", json!({}))
            .expect("调用");
        let argv = recorder.argv.lock().expect("argv");
        assert_eq!(argv[0].0, "ccnm");
        assert_eq!(
            argv[0].1,
            vec!["mcp", "bridge", "proj", "--node", "work", "--mode", "read"]
        );
    }
}
