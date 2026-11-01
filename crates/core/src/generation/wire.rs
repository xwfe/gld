//! 宿主和执行代之间的消息：一行一条 JSON，走子进程的 stdin/stdout。
//!
//! 选匿名管道的理由写在 RFC-0008 §3.4：不开端口、不进文件系统，也就没有第二个入口。
//! 一行一条是因为 `serde_json` 不会输出裸换行，假候选二进制（测试里）用 `printf` 就能写。

use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::BuildIdentity;
use crate::auth::{AuthContext, Grant, Principal};
use crate::hub::ExecutionRoute;
use crate::nodes::registry::NativeMember;
use crate::settings::HubConfig;

/// 单条消息上限。工具回包本身有界（图片预览、命令输出都有上限），这里只防对端发疯。
pub const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;

/// 宿主发给执行代的。
///
/// `op`、`value` 都是编码好的 JSON 文本，不是嵌套的对象。客户端发来的请求和工具回包可以嵌套得很深，
/// 再包两层就超过 `serde_json` 的递归上限（128 层）：一整条帧解析失败，连 `id` 都拿不到，
/// 对面没法回错误，请求就永远挂着。外层固定一层，里层解析失败时还能按 `id` 回一句。
/// `ok` 为假时 `value` 是给人看的错误文字。
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "frame", rename_all = "snake_case")]
pub enum HostFrame {
    Hello { generation_protocol: u32 },
    Call { id: u64, op: String },
    HostReply { id: u64, ok: bool, value: String },
}

/// 执行代发给宿主的。内容的编码方式见 [`HostFrame`]。
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "frame", rename_all = "snake_case")]
pub enum GenerationFrame {
    Ready {
        identity: BuildIdentity,
    },
    /// 起来了但用不了（比如数据文件读不出来）。
    Refused {
        reason: String,
    },
    Reply {
        id: u64,
        ok: bool,
        value: String,
    },
    HostCall {
        id: u64,
        op: String,
    },
}

/// 宿主让执行代做的事。
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    Ping,
    Status,
    /// 这个构建认识的全部工具名，切换前核对用。
    ToolNames,
    /// 按这份配置建 Hub。`sweep` 为真才跑 work_session 启动检查：每次监听器启动只该跑一轮。
    HubConfigure {
        config: Box<HubConfig>,
        sweep: bool,
    },
    HubShutdown,
    /// 收掉已经离开的成员，见 [`crate::hub::Hub::reconcile`]。
    HubReconcile,
    ServedTools,
    HubMcp {
        auth: WireAuth,
        body: Value,
        /// 宿主认定的归属，执行代要核对，见 [`crate::hub::Hub::handle_routed`]。
        route: ExecutionRoute,
    },
    WorkspaceMcp {
        workspace_id: String,
        auth: WireAuth,
        body: Value,
    },
    ActionsCall {
        workspace_id: String,
        auth: WireAuth,
        tool: String,
        arguments: Value,
    },
    /// `gld tool call`：本机主体，不经任何入口。
    LocalTool {
        workspace_id: String,
        name: String,
        args: Value,
    },
    TerminateWorkspace {
        root: PathBuf,
    },
    RevokeGrant {
        grant_id: String,
    },
    ApproveAgent {
        task_id: String,
        agent: Option<String>,
        model: Option<String>,
        effort: Option<String>,
    },
    StopAgent {
        task_id: String,
    },
    /// 这个工作区能搬走吗：能就回 `{"state": …}`，不能回 `{"blocked": [原因]}`。只读，
    /// 新代没接住时它还归这一代。比这个操作老的二进制读不懂，回错误，工作区就留在原地。
    ReleaseWorkspace {
        key: String,
    },
    /// 接住上一代 `ReleaseWorkspace` 给的状态。
    AdoptWorkspace {
        key: String,
        state: Value,
    },
    /// 宿主要退了：先停命令和 Agent，记成 `interrupted`，但人还留着等最后一步。
    InterruptAll,
    Shutdown,
}

/// 执行代反过来问宿主的。节点连接在宿主手里（RFC-0008 §3.4），执行代摸不到。
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum HostOp {
    NodeMembers,
    NodeCall {
        auth: WireAuth,
        member: WireNative,
        tool: String,
        args: Value,
    },
}

/// 鉴权结果在通道上的样子。
///
/// 只由宿主从验完鉴权的 [`AuthContext`] 生成，不读 HTTP 请求里的任何东西，所以客户端伪造不了。
/// 这里没直接给 `AuthContext` 派生序列化，是因为 `Grant::browser` 标了 `serde(skip)`（它不落盘）：
/// 直接序列化会丢掉它，浏览器地址进来的 grant 到了执行代就变成了能委派 Agent 的 grant。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireAuth {
    principal: WirePrincipal,
    scope: String,
    grant: Option<WireGrant>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum WirePrincipal {
    Anonymous,
    SharedSecret,
    OauthClient { client_id: String },
    OpenAiTunnel,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WireGrant {
    id: String,
    name: String,
    workspaces: Vec<String>,
    read_only: bool,
    browser: bool,
}

impl From<&AuthContext> for WireAuth {
    fn from(auth: &AuthContext) -> Self {
        Self {
            principal: match auth.principal() {
                Principal::Anonymous => WirePrincipal::Anonymous,
                Principal::SharedSecret => WirePrincipal::SharedSecret,
                Principal::OAuthClient { client_id } => WirePrincipal::OauthClient {
                    client_id: client_id.clone(),
                },
                Principal::OpenAiTunnel => WirePrincipal::OpenAiTunnel,
            },
            scope: auth.scope().to_string(),
            grant: auth.grant().map(|grant| WireGrant {
                id: grant.id.clone(),
                name: grant.name.clone(),
                workspaces: grant.workspaces.clone(),
                read_only: grant.read_only,
                browser: grant.browser,
            }),
        }
    }
}

impl WireAuth {
    pub fn into_auth(self) -> AuthContext {
        let principal = match self.principal {
            WirePrincipal::Anonymous => Principal::Anonymous,
            WirePrincipal::SharedSecret => Principal::SharedSecret,
            WirePrincipal::OauthClient { client_id } => Principal::OAuthClient { client_id },
            WirePrincipal::OpenAiTunnel => Principal::OpenAiTunnel,
        };
        AuthContext::new(principal, self.scope).with_grant(self.grant.map(|grant| Grant {
            id: grant.id,
            name: grant.name,
            workspaces: grant.workspaces,
            read_only: grant.read_only,
            browser: grant.browser,
        }))
    }
}

/// [`NativeMember`] 在通道上的样子。它在 `nodes/` 里，那边没有序列化，这边单列一份，不去动它。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireNative {
    pub id: String,
    pub name: String,
    pub machine_id: String,
    pub remote_workspace_id: String,
    pub online: bool,
    pub revoked: bool,
    pub tool_profile: String,
}

impl From<&NativeMember> for WireNative {
    fn from(member: &NativeMember) -> Self {
        Self {
            id: member.id.clone(),
            name: member.name.clone(),
            machine_id: member.machine_id.clone(),
            remote_workspace_id: member.remote_workspace_id.clone(),
            online: member.online,
            revoked: member.revoked,
            tool_profile: member.tool_profile.clone(),
        }
    }
}

impl From<WireNative> for NativeMember {
    fn from(member: WireNative) -> Self {
        Self {
            id: member.id,
            name: member.name,
            machine_id: member.machine_id,
            remote_workspace_id: member.remote_workspace_id,
            online: member.online,
            revoked: member.revoked,
            tool_profile: member.tool_profile,
        }
    }
}

/// 一行一条地发。多个线程共用一条管道，一整行要在一把锁里写完，否则两条消息会交错。
pub struct FrameWriter<W> {
    inner: Mutex<W>,
}

impl<W: Write> FrameWriter<W> {
    pub fn new(writer: W) -> Self {
        Self {
            inner: Mutex::new(writer),
        }
    }

    #[cfg(test)]
    pub fn into_inner(self) -> W {
        self.inner
            .into_inner()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn send<T: Serialize>(&self, frame: &T) -> io::Result<()> {
        let mut line = serde_json::to_vec(frame).map_err(io::Error::other)?;
        if line.len() >= MAX_FRAME_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("消息 {} 字节，超过上限 {MAX_FRAME_BYTES}", line.len()),
            ));
        }
        line.push(b'\n');
        let mut writer = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        writer.write_all(&line)?;
        writer.flush()
    }
}

/// 读一行。对端关了返回 `Ok(None)`。超过上限当错误：继续读只会把内存吃光。
pub fn read_frame<R: BufRead>(reader: &mut R) -> io::Result<Option<String>> {
    let mut line = Vec::new();
    loop {
        let buffer = reader.fill_buf()?;
        if buffer.is_empty() {
            return Ok((!line.is_empty()).then(|| String::from_utf8_lossy(&line).into_owned()));
        }
        let (chunk, done) = match buffer.iter().position(|byte| *byte == b'\n') {
            Some(index) => (&buffer[..index], true),
            None => (buffer, false),
        };
        if line.len() + chunk.len() > MAX_FRAME_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("消息超过上限 {MAX_FRAME_BYTES} 字节"),
            ));
        }
        line.extend_from_slice(chunk);
        let consumed = chunk.len() + usize::from(done);
        reader.consume(consumed);
        if done {
            return Ok(Some(String::from_utf8_lossy(&line).into_owned()));
        }
    }
}
