//! 宿主和执行代之间的纯消息与版本约定；进程探测、身份转换和管道 I/O 由调用侧负责。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::HubConfig;

/// 宿主和执行代之间消息的版本。形状有不兼容改动时加一；同时要决定 [`GENERATION_PROTOCOL_MIN`]。
/// 只加新操作不用加一：老二进制读不懂时回错误，不会执行，调用的一方自己要接得住（比如工作区搬不走就留着）。
pub const GENERATION_PROTOCOL: u32 = 2;
/// 这个宿主还能带的最老执行代协议。范围之外的二进制一律不让上线，失败发生在动路由之前。
pub const GENERATION_PROTOCOL_MIN: u32 = 2;
/// 盘上状态的格式版本：`profiles.json`、运行记录、任务和委派记录。
///
/// 两代要同时读同一份数据目录，所以必须相等。改了这些文件的不兼容格式就加一，
/// 不加的话新代会把旧代读不懂的东西写到盘上，而这种坏法回退也救不回来。
pub const STATE_FORMAT: u32 = 2;
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

/// 宿主验证后的网络身份。读取 DTO 本身不代表认证；core 转换时再校验主体与权限组合。
/// 本机操作员没有网络编码，只能使用单独的 LocalTool 操作。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WireAuth {
    Mcp {
        entry: String,
        subject: WireMcpSubject,
        credential_kind: WireCredentialKind,
        grant: WireGrant,
    },
    Actions {
        entry: String,
        workspace_id: String,
        subject: WireActionsSubject,
        write: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WireMcpSubject {
    #[serde(rename = "oauth")]
    OAuth {
        client_id: String,
    },
    Token,
    Url,
    Tunnel {
        binding_id: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WireActionsSubject {
    Anonymous,
    ApiKey,
    #[serde(rename = "oauth")]
    OAuth {
        client_id: String,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireCredentialKind {
    #[serde(rename = "oauth")]
    OAuth,
    Token,
    Url,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "ids",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum WireProjectScope {
    AllProjects,
    Selected(Vec<String>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireServiceCapabilities {
    pub user_prompts: bool,
    pub machine_mcp: bool,
    pub managed_workspaces: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireGrantPermissions {
    pub write: bool,
    pub service: WireServiceCapabilities,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireGrantStatus {
    Active,
    Revoked,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireGrant {
    pub id: String,
    pub name: String,
    pub scope: WireProjectScope,
    pub permissions: WireGrantPermissions,
    pub status: WireGrantStatus,
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
                    "generation_protocol": GENERATION_PROTOCOL, "state_format": STATE_FORMAT, "pid": 1, "exe": null
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

    fn url_auth() -> WireAuth {
        WireAuth::Mcp {
            entry: "hub".into(),
            subject: WireMcpSubject::Url,
            credential_kind: WireCredentialKind::Url,
            grant: WireGrant {
                id: "grant".into(),
                name: "browser".into(),
                scope: WireProjectScope::AllProjects,
                permissions: WireGrantPermissions {
                    write: true,
                    service: WireServiceCapabilities {
                        user_prompts: false,
                        machine_mcp: false,
                        managed_workspaces: false,
                    },
                },
                status: WireGrantStatus::Active,
            },
        }
    }

    #[test]
    fn network_identity_carries_explicit_scope_and_credential_purpose() {
        let call = Op::HubMcp {
            auth: url_auth(),
            body: serde_json::json!({"method": "tools/list"}),
            route: ExecutionRoute::Owned("workspace".into()),
        };
        let value = serde_json::to_value(call).unwrap();
        assert_eq!(value["auth"]["kind"], "mcp");
        assert_eq!(value["auth"]["credential_kind"], "url");
        assert_eq!(
            value["auth"]["grant"]["scope"],
            serde_json::json!({"kind":"all_projects"})
        );
        let Op::HubMcp { auth, .. } = serde_json::from_value(value).unwrap() else {
            panic!("expected routed call");
        };
        assert!(matches!(
            auth,
            WireAuth::Mcp {
                credential_kind: WireCredentialKind::Url,
                ..
            }
        ));
    }

    #[test]
    fn legacy_missing_or_unknown_authority_is_rejected() {
        let valid = serde_json::to_value(url_auth()).unwrap();
        for name in ["entry", "subject", "credential_kind", "grant"] {
            let mut value = valid.clone();
            value.as_object_mut().unwrap().remove(name);
            assert!(serde_json::from_value::<WireAuth>(value).is_err(), "{name}");
        }
        for name in ["scope", "permissions", "status"] {
            let mut value = valid.clone();
            value["grant"].as_object_mut().unwrap().remove(name);
            assert!(serde_json::from_value::<WireAuth>(value).is_err(), "{name}");
        }
        for value in [
            serde_json::json!({"principal":{"kind":"shared_secret"},"scope":"hub","grant":null}),
            serde_json::json!({"kind":"local_operator"}),
            serde_json::json!({"kind":"mcp","entry":"hub","subject":{"kind":"unknown"},"credential_kind":"url","grant":valid["grant"]}),
        ] {
            assert!(serde_json::from_value::<WireAuth>(value).is_err());
        }
        let mut extra = valid;
        extra["grant"]["browser"] = serde_json::json!(false);
        assert!(serde_json::from_value::<WireAuth>(extra).is_err());
    }

    #[test]
    fn actions_has_its_own_bounded_network_identity() {
        let auth = WireAuth::Actions {
            entry: "actions".into(),
            workspace_id: "workspace".into(),
            subject: WireActionsSubject::OAuth {
                client_id: "client".into(),
            },
            write: false,
        };
        let value = serde_json::to_value(auth).unwrap();
        assert_eq!(value["subject"]["kind"], "oauth");
        assert!(matches!(
            serde_json::from_value::<WireAuth>(value).unwrap(),
            WireAuth::Actions { write: false, .. }
        ));
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
