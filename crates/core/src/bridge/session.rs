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
//! 这四个数是 **read 模式**的。coding 模式是写租约，规则另定（RFC 5.4），
//! 不要顺手复用这里的数字。

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

/// 一个成员的槽位。
///
/// 里面是 `Option`：槽位先占上、连接后开。这样两个请求同时打到一个还没连上的
/// 成员时，第二个会在槽位的锁上等第一个连完，而不是各起一个 bridge。
struct Slot {
    /// 开这条连接时的成员配置 + 模式。变了就是新一代，旧连接作废。
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
    handshake_timeout: Duration,
    call_timeout: Duration,
    live: Mutex<HashMap<String, Slot>>,
}

impl Connections {
    /// 真连远端用这个。
    pub fn new() -> Self {
        Self::with_opener(Box::new(Spawn))
    }

    pub fn with_opener(opener: Box<dyn Open>) -> Self {
        Connections {
            opener,
            idle_after: IDLE_AFTER,
            handshake_timeout: HANDSHAKE_TIMEOUT,
            call_timeout: CALL_TIMEOUT,
            live: Mutex::new(HashMap::new()),
        }
    }

    /// 改预算。给测试用，也给以后做成配置留个口子。
    pub fn with_budgets(mut self, idle_after: Duration, call_timeout: Duration) -> Self {
        self.idle_after = idle_after;
        self.handshake_timeout = call_timeout;
        self.call_timeout = call_timeout;
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
        mode: Mode,
        tool: &str,
        arguments: Value,
    ) -> Result<Value, PeerError> {
        let slot = self.slot(&member.id, &generation_of(principal, member, mode));
        let mut guard = slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if guard.is_none() {
            *guard = Some(Live {
                peer: self.handshake(member, mode)?,
                last_used: Instant::now(),
            });
        }
        let live = guard.as_mut().expect("刚刚连上");
        let result = live.peer.call_tool(tool, arguments, self.call_timeout);
        match &result {
            // 远端明确回了 JSON-RPC error 也算这条连接好着，下次还能用。
            Ok(_) | Err(PeerError::Remote { .. }) => live.last_used = Instant::now(),
            // 超时、写不进去、对面关了、回了看不懂的东西——传输层已经不可信，
            // 留着它下一次调用会读到上一次的残留回复。丢掉，下次重连。
            Err(_) => {
                guard.take();
            }
        }
        result
    }

    /// 只留这些成员的连接，其余全收掉。
    ///
    /// 成员被移出 hub、被删掉、权限收紧之后要调它——RFC 5.1「成员删除/权限
    /// 收紧/配置变化须使对应旧会话失效」。
    pub fn retain(&self, keep: &[String]) {
        let dropped: Vec<Slot> = {
            let mut live = self.live.lock().unwrap_or_else(|p| p.into_inner());
            let gone: Vec<String> = live
                .keys()
                .filter(|id| !keep.iter().any(|kept| kept == *id))
                .cloned()
                .collect();
            gone.iter().filter_map(|id| live.remove(id)).collect()
        };
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
    fn slot(&self, member_id: &str, generation: &str) -> Arc<Mutex<Option<Live>>> {
        let mut detached: Vec<Slot> = Vec::new();
        let slot = {
            let mut live = self.live.lock().unwrap_or_else(|p| p.into_inner());
            let idle: Vec<String> = live
                .iter()
                .filter(|(id, slot)| id.as_str() != member_id && slot.is_idle(self.idle_after))
                .map(|(id, _)| id.clone())
                .collect();
            for id in idle {
                detached.extend(live.remove(&id));
            }
            match live.get(member_id) {
                Some(slot) if slot.generation == generation => slot.connection.clone(),
                _ => {
                    // 配置或模式变了：旧连接是按旧权限开的，不能接着用。
                    detached.extend(live.remove(member_id));
                    let connection = Arc::new(Mutex::new(None));
                    live.insert(
                        member_id.to_string(),
                        Slot {
                            generation: generation.to_string(),
                            connection: connection.clone(),
                        },
                    );
                    connection
                }
            }
        };
        drop(detached);
        slot
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
    use std::sync::mpsc::RecvTimeoutError;

    /// 合成通道：握手照答，`tools/call` 回一句话，记下自己被关过没有。
    struct Fake {
        opened_with: Vec<String>,
        pending: Option<String>,
        closed: Arc<AtomicUsize>,
        /// 调用一律超时，用来验「传输层坏了就丢连接」。
        black_hole: bool,
    }

    impl Transport for Fake {
        fn send_line(&mut self, line: &str) -> std::io::Result<()> {
            self.opened_with.push(line.to_string());
            let request: Value = serde_json::from_str(line).expect("请求是 JSON");
            let Some(id) = request.get("id").cloned() else {
                return Ok(()); // 通知，没有回复
            };
            let method = request["method"].as_str().unwrap_or("");
            if self.black_hole && method == "tools/call" {
                return Ok(());
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
    }

    impl Recording {
        fn new() -> Arc<Self> {
            Arc::new(Recording {
                argv: Mutex::new(Vec::new()),
                closed: Arc::new(AtomicUsize::new(0)),
                black_hole: false,
            })
        }
        fn opens(&self) -> usize {
            self.argv.lock().expect("argv").len()
        }
        fn closes(&self) -> usize {
            self.closed.load(Ordering::SeqCst)
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
                .call(ANYONE, &member("m1"), Mode::Read, "read_file", json!({}))
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
        pool.call(ANYONE, &member("m1"), Mode::Read, "read_file", json!({}))
            .expect("m1");
        pool.call(ANYONE, &member("m2"), Mode::Read, "read_file", json!({}))
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
        pool.call(ANYONE, &m, Mode::Read, "read_file", json!({}))
            .expect("先");

        m.workspace = "another-project".into();
        pool.call(ANYONE, &m, Mode::Read, "read_file", json!({}))
            .expect("后");

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
        pool.call("oauth:hub:chatgpt", &m, Mode::Read, "read_file", json!({}))
            .expect("第一个主体");
        pool.call("oauth:hub:claude", &m, Mode::Read, "read_file", json!({}))
            .expect("另一个主体");
        assert_eq!(recorder.opens(), 2, "两个主体共用了一条 bridge");
        assert_eq!(recorder.closes(), 1, "旧主体那条没被关掉");

        // 同一个主体再来还是复用，不是每次都重开。
        pool.call("oauth:hub:claude", &m, Mode::Read, "read_file", json!({}))
            .expect("同一个主体");
        assert_eq!(recorder.opens(), 2);
    }

    /// 模式变了也是新一代。read 的连接不能被拿去当 coding 用。
    #[test]
    fn changing_the_mode_opens_a_new_generation() {
        let recorder = Recording::new();
        let pool = connections(&recorder);
        let mut m = member("m1");
        m.max_mode = Mode::Coding;
        pool.call(ANYONE, &m, Mode::Read, "read_file", json!({}))
            .expect("read");
        pool.call(ANYONE, &m, Mode::Coding, "read_file", json!({}))
            .expect("coding");
        assert_eq!(recorder.opens(), 2);
    }

    /// 闲过期限的连接在下一次请求时被收掉，不是等到进程退出。
    #[test]
    fn an_idle_connection_is_closed_on_the_next_request() {
        let recorder = Recording::new();
        let pool = Connections::with_opener(Box::new(recorder.clone()))
            .with_budgets(Duration::from_millis(30), Duration::from_millis(200));
        pool.call(ANYONE, &member("idle"), Mode::Read, "read_file", json!({}))
            .expect("idle 成员");
        assert_eq!(pool.open_count(), 1);

        std::thread::sleep(Duration::from_millis(60));
        pool.call(ANYONE, &member("busy"), Mode::Read, "read_file", json!({}))
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
            .call(ANYONE, &member("m1"), Mode::Read, "read_file", json!({}))
            .expect_err("应该超时");
        assert!(matches!(err, PeerError::Timeout { .. }), "{err}");
        assert_eq!(recorder.closes(), 1, "坏掉的连接没被关");

        let _ = pool.call(ANYONE, &member("m1"), Mode::Read, "read_file", json!({}));
        assert_eq!(recorder.opens(), 2, "没重连，还在用那条坏的");
    }

    /// 成员被移出 hub：连接立刻收掉，不等它自己闲过期。
    #[test]
    fn a_member_that_left_the_hub_loses_its_connection() {
        let recorder = Recording::new();
        let pool = connections(&recorder);
        pool.call(ANYONE, &member("stays"), Mode::Read, "read_file", json!({}))
            .expect("stays");
        pool.call(
            ANYONE,
            &member("leaves"),
            Mode::Read,
            "read_file",
            json!({}),
        )
        .expect("leaves");

        pool.retain(&["stays".to_string()]);

        assert_eq!(pool.open_count(), 1);
        assert_eq!(recorder.closes(), 1);
        pool.close_all();
        assert_eq!(pool.open_count(), 0);
        assert_eq!(recorder.closes(), 2);
    }

    /// 开的是公开的 bridge 子命令，argv 全来自配置。
    #[test]
    fn the_bridge_is_started_with_the_configured_argv_only() {
        let recorder = Recording::new();
        let pool = connections(&recorder);
        pool.call(ANYONE, &member("m1"), Mode::Read, "read_file", json!({}))
            .expect("调用");
        let argv = recorder.argv.lock().expect("argv");
        assert_eq!(argv[0].0, "ccnm");
        assert_eq!(
            argv[0].1,
            vec!["mcp", "bridge", "proj", "--node", "work", "--mode", "read"]
        );
    }
}
