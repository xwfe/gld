//! 宿主和执行代之间的纯消息与版本约定；进程探测、身份转换和管道 I/O 由调用侧负责。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::HubConfig;

/// 宿主和执行代之间消息的版本。形状有不兼容改动时加一；同时要决定 [`GENERATION_PROTOCOL_MIN`]。
/// 只加新操作不用加一：老二进制读不懂时回错误，不会执行，调用的一方自己要接得住（比如工作区搬不走就留着）。
pub const GENERATION_PROTOCOL: u32 = 1;
/// 这个宿主还能带的最老执行代协议。范围之外的二进制一律不让上线，失败发生在动路由之前。
pub const GENERATION_PROTOCOL_MIN: u32 = 1;
/// 盘上状态的格式版本：`profiles.json`、运行记录、任务和委派记录。
///
/// 两代要同时读同一份数据目录，所以必须相等。改了这些文件的不兼容格式就加一，
/// 不加的话新代会把旧代读不懂的东西写到盘上，而这种坏法回退也救不回来。
pub const STATE_FORMAT: u32 = 1;
/// 一代是什么构建。每个二进制启动时自报，宿主据此判断能不能用。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildIdentity {
    pub version: String,
    /// 构建提交；不知道就是 `None`，不拿版本号顶替。
    pub build_commit: Option<String>,
    pub generation_protocol: u32,
    pub state_format: u32,
    pub pid: u32,
    pub exe: Option<PathBuf>,
}

impl BuildIdentity {
    /// 这个宿主不能用它的原因；能用是 `None`。
    pub fn incompatibility(&self) -> Option<String> {
        if !(GENERATION_PROTOCOL_MIN..=GENERATION_PROTOCOL).contains(&self.generation_protocol) {
            return Some(format!(
                "执行代协议 {} 不在这个守护进程支持的 {GENERATION_PROTOCOL_MIN}–{GENERATION_PROTOCOL} 范围内",
                self.generation_protocol
            ));
        }
        if self.state_format != STATE_FORMAT {
            return Some(format!(
                "盘上状态格式 {} 和这个守护进程的 {STATE_FORMAT} 不一样",
                self.state_format
            ));
        }
        None
    }
}

/// 宿主认定的一条请求在执行代之间的归属。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ExecutionRoute {
    /// 工具表这类发现请求：走最老的一代。
    Discovery,
    /// 不碰工作区状态（`list_workspaces`、节点调用、选不中成员的报错）：走当前代。
    Unbound,
    /// 碰这个工作区：走它的主人，没主人就绑给当前代。
    Owned(String),
}

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
    /// 收掉已经离开的成员。
    HubReconcile,
    ServedTools,
    HubMcp {
        auth: WireAuth,
        body: Value,
        /// 宿主认定的归属，执行代要核对。
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
/// 由宿主从验完鉴权的结果生成；读取这个 DTO 本身不代表完成认证。
/// 独立保留 grant 的 browser 字段，因为持久化授权模型不会保存它：
/// 直接序列化会丢掉它，浏览器地址进来的 grant 到了执行代就变成了能委派 Agent 的 grant。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireAuth {
    pub principal: WirePrincipal,
    pub scope: String,
    pub grant: Option<WireGrant>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WirePrincipal {
    Anonymous,
    SharedSecret,
    OauthClient { client_id: String },
    OpenAiTunnel,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireGrant {
    pub id: String,
    pub name: String,
    pub workspaces: Vec<String>,
    pub read_only: bool,
    pub browser: bool,
}

/// 节点成员在通道上的数据；运行时节点登记与它的转换由调用侧负责。
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

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> BuildIdentity {
        BuildIdentity {
            version: "0.0.0".into(),
            build_commit: None,
            generation_protocol: GENERATION_PROTOCOL,
            state_format: STATE_FORMAT,
            pid: 1,
            exe: None,
        }
    }

    #[test]
    fn only_a_matching_protocol_and_state_format_is_usable() {
        assert_eq!(identity().incompatibility(), None);
        let newer = BuildIdentity {
            generation_protocol: GENERATION_PROTOCOL + 1,
            ..identity()
        };
        assert!(newer.incompatibility().unwrap().contains("协议"));
        let older = BuildIdentity {
            generation_protocol: GENERATION_PROTOCOL_MIN - 1,
            ..identity()
        };
        assert!(older.incompatibility().is_some());
        let other_state = BuildIdentity {
            state_format: STATE_FORMAT + 1,
            ..identity()
        };
        assert!(other_state.incompatibility().unwrap().contains("状态格式"));
    }

    /// 内容嵌套到 serde 的递归上限也不能让帧本身解析失败：解析不了就拿不到 id，对面只能永远等。
    #[test]
    fn deep_content_never_breaks_the_frame_itself() {
        let depth = 200;
        let deep = format!("{}{}", "[".repeat(depth), "]".repeat(depth));
        let line = serde_json::to_string(&HostFrame::Call {
            id: 9,
            op: deep.clone(),
        })
        .unwrap();
        let HostFrame::Call { id, op } = serde_json::from_str(&line).unwrap() else {
            panic!("{line}");
        };
        assert_eq!((id, op), (9, deep));
        let line = serde_json::to_string(&GenerationFrame::Reply {
            id: 9,
            ok: true,
            value: "[".repeat(depth),
        })
        .unwrap();
        assert!(serde_json::from_str::<GenerationFrame>(&line).is_ok());
    }

    #[test]
    fn generation_frames_keep_their_wire_shape() {
        assert_eq!(
            serde_json::to_value(HostFrame::Hello {
                generation_protocol: 1
            })
            .unwrap(),
            serde_json::json!({"frame": "hello", "generation_protocol": 1})
        );
        assert_eq!(
            serde_json::to_value(HostFrame::Call {
                id: 7,
                op: r#"{"op":"ping"}"#.into()
            })
            .unwrap(),
            serde_json::json!({"frame": "call", "id": 7, "op": r#"{"op":"ping"}"#})
        );
        assert_eq!(
            serde_json::to_value(GenerationFrame::Ready {
                identity: identity()
            })
            .unwrap(),
            serde_json::json!({
                "frame": "ready",
                "identity": {
                    "version": "0.0.0", "build_commit": null,
                    "generation_protocol": 1, "state_format": 1, "pid": 1, "exe": null
                }
            })
        );
        assert_eq!(
            serde_json::to_value(GenerationFrame::Reply {
                id: 7,
                ok: false,
                value: "unknown operation".into()
            })
            .unwrap(),
            serde_json::json!({"frame": "reply", "id": 7, "ok": false, "value": "unknown operation"})
        );
    }

    #[test]
    fn principal_tags_keep_their_wire_shape() {
        for (principal, expected) in [
            (
                WirePrincipal::Anonymous,
                serde_json::json!({"kind": "anonymous"}),
            ),
            (
                WirePrincipal::SharedSecret,
                serde_json::json!({"kind": "shared_secret"}),
            ),
            (
                WirePrincipal::OauthClient {
                    client_id: "client".into(),
                },
                serde_json::json!({"kind": "oauth_client", "client_id": "client"}),
            ),
            (
                WirePrincipal::OpenAiTunnel,
                serde_json::json!({"kind": "open_ai_tunnel"}),
            ),
        ] {
            assert_eq!(serde_json::to_value(principal).unwrap(), expected);
            assert!(serde_json::from_value::<WirePrincipal>(expected).is_ok());
        }
    }

    #[test]
    fn routed_calls_keep_explicit_browser_permissions() {
        let call = Op::HubMcp {
            auth: WireAuth {
                principal: WirePrincipal::SharedSecret,
                scope: "hub".into(),
                grant: Some(WireGrant {
                    id: "grant".into(),
                    name: "browser".into(),
                    workspaces: vec!["workspace".into()],
                    read_only: false,
                    browser: true,
                }),
            },
            body: serde_json::json!({"method": "tools/list"}),
            route: ExecutionRoute::Owned("workspace".into()),
        };
        let expected = serde_json::json!({
            "op": "hub_mcp",
            "auth": {
                "principal": {"kind": "shared_secret"}, "scope": "hub",
                "grant": {
                    "id": "grant", "name": "browser", "workspaces": ["workspace"],
                    "read_only": false, "browser": true
                }
            },
            "body": {"method": "tools/list"}, "route": {"Owned": "workspace"}
        });
        assert_eq!(serde_json::to_value(call).unwrap(), expected);
        let Op::HubMcp { auth, .. } = serde_json::from_value(expected).unwrap() else {
            panic!("expected a routed call");
        };
        assert!(auth.grant.unwrap().browser);
    }

    #[test]
    fn execution_routes_keep_their_wire_shape() {
        for (route, expected) in [
            (ExecutionRoute::Discovery, serde_json::json!("Discovery")),
            (ExecutionRoute::Unbound, serde_json::json!("Unbound")),
            (
                ExecutionRoute::Owned("workspace".into()),
                serde_json::json!({"Owned": "workspace"}),
            ),
        ] {
            assert_eq!(serde_json::to_value(&route).unwrap(), expected);
            assert_eq!(
                serde_json::from_value::<ExecutionRoute>(expected).unwrap(),
                route
            );
        }
    }
}
