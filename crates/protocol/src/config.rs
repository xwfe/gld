//! Shared configuration data and deterministic defaults for IPC and execution generations.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrpProfile {
    pub id: String,
    pub name: String,
    pub server: String,
    #[serde(default = "default_frp_server_port", alias = "serverPort")]
    pub server_port: u16,
}

/// Global outbound proxy used by network-facing operations such as the
/// Cloudflare quick tunnel.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyConfig {
    /// "none" (no proxy) | "system" (env HTTP(S)_PROXY) | "manual".
    #[serde(default = "default_proxy_mode")]
    pub mode: String,
    /// Proxy URL used when `mode == "manual"` (e.g. http://127.0.0.1:7890).
    #[serde(default)]
    pub url: String,
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            mode: default_proxy_mode(),
            url: String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GlobalGatewayConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_global_gateway_port")]
    pub local_port: u16,
    /// none | cloudflare | frp
    #[serde(default = "default_global_gateway_tunnel_type")]
    pub tunnel_type: String,
    #[serde(default)]
    pub public_url: String,
    #[serde(default = "default_global_gateway_cloudflare_mode")]
    pub cloudflare_mode: String,
    #[serde(default)]
    pub frp_profile_id: String,
    #[serde(default)]
    pub frp_server: String,
    #[serde(default)]
    pub frp_subdomain: String,
    #[serde(default = "default_frp_server_port")]
    pub frp_server_port: u16,
    #[serde(default = "default_global_gateway_use_proxy")]
    pub use_proxy: bool,
}

impl Default for GlobalGatewayConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            local_port: default_global_gateway_port(),
            tunnel_type: default_global_gateway_tunnel_type(),
            public_url: String::new(),
            cloudflare_mode: default_global_gateway_cloudflare_mode(),
            frp_profile_id: String::new(),
            frp_server: String::new(),
            frp_subdomain: String::new(),
            frp_server_port: default_frp_server_port(),
            use_proxy: default_global_gateway_use_proxy(),
        }
    }
}

/// 托管任务的准入/保存预算，不是对子进程的文件系统硬配额。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct WorkSessionLimits {
    pub max_active: u32,
    pub max_records: u32,
    pub max_bytes: u64,
}

impl Default for WorkSessionLimits {
    fn default() -> Self {
        Self {
            max_active: 512,
            max_records: 8192,
            max_bytes: 10 * 1024 * 1024 * 1024,
        }
    }
}

impl WorkSessionLimits {
    pub const MAX_RECORDS: u32 = 8192;

    pub fn validate(&self) -> Result<(), String> {
        if self.max_active == 0
            || self.max_active > self.max_records
            || self.max_records > Self::MAX_RECORDS
            || self.max_bytes == 0
        {
            return Err("通用任务容量要求 1 <= maxActive <= maxRecords <= 8192，maxBytes > 0；降低上限不会删除已有数据。".into());
        }
        Ok(())
    }
}

/// 聚合入口（hub）：一条 MCP 连接访问多个工作区。
///
/// 它是什么、什么时候用、代价是什么见 docs/concepts.md 的「聚合入口」一节。
/// 这里只有配置；按 `workspace` 参数分发和隔离规则由宿主应用负责。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HubConfig {
    #[serde(default = "default_hub_port")]
    pub local_port: u16,
    /// oauth | bearer | noauth，含义和工作区的 `mcp.auth` 一样。
    #[serde(default = "default_hub_auth_type")]
    pub auth_type: String,
    /// hub 列给客户端的工具集。成员自己的工具集照样生效，两边取交集——
    /// 这里写 advanced 也放不开一个 read-only 的成员。
    #[serde(
        default = "default_hub_tool_profile",
        deserialize_with = "deserialize_tool_profile"
    )]
    pub tool_profile: String,
    /// 公网基地址（不带 `/mcp`）。`tunnel_type` 是 none 时它就是入口（自建反代）；
    /// Cloudflare 固定域名时它是那个域名。临时地址和 FRP 的地址是起隧道时现算的，
    /// 不存这里。
    #[serde(default)]
    pub public_url: String,
    /// 经全局入口暴露为 `<入口公网地址>/hub/mcp`。
    ///
    /// 老路子：服务有了自己的隧道（下面几项）之后命令行不再提供它，老配置照旧生效。
    #[serde(default)]
    pub use_global_gateway: bool,
    /// 服务自己的公网隧道：none | cloudflare | frp | openai（RFC-0004）。
    ///
    /// openai 是 OpenAI 的 Secure MCP Tunnel：本机 tunnel-client 只往外连 OpenAI，
    /// 没有公网地址，ChatGPT 按隧道 ID 找过来。
    #[serde(default = "default_hub_tunnel_type")]
    pub tunnel_type: String,
    /// openai 时的隧道 ID（Platform 上建隧道给的 `tunnel_…`）。
    #[serde(default)]
    pub openai_tunnel_id: String,
    /// cloudflare 时：quick（临时地址，每次重启都变）| named（固定域名，要
    /// Tunnel Token，存在服务凭据 `cloudflare_token` 里）。
    #[serde(default = "default_global_gateway_cloudflare_mode")]
    pub cloudflare_mode: String,
    /// frp 时用哪个 FRP 服务器配置（`gld frp list` 里的 id）。
    #[serde(default)]
    pub frp_profile_id: String,
    /// frp 时的子域名，公网地址是 `https://<子域名>.<frps 域名>`。
    #[serde(default)]
    pub frp_subdomain: String,
    /// 隧道进程是否套用全局出站代理（和工作区隧道的默认一样）。
    #[serde(default = "default_global_gateway_use_proxy")]
    pub use_proxy: bool,
    /// 成员工作区 id，按加入顺序。
    ///
    /// 存 id 不存名字：工作区改名不该让它悄悄掉出 hub。工作区被 destroy 时
    /// 会顺手从这里删掉，见 `App::delete_workspace`。
    #[serde(default)]
    pub members: Vec<String>,
    /// 非项目任务空间。默认关闭，只给完整鉴权主体，不扩大现有项目 grant。
    #[serde(default)]
    pub work_sessions: bool,
    #[serde(default)]
    pub work_session_limits: WorkSessionLimits,
    /// 启动 hub 时置为 true，用户主动停掉时置回 false；守护进程启动时据此恢复。
    ///
    /// 不跟全局的「启动时恢复」开关走：那个默认关，而 hub 是一条客户端里配死的
    /// 连接，守护进程重启一次它就没了，用户只会看到连接器报错。
    #[serde(default)]
    pub restore_on_launch: bool,
}

impl Default for HubConfig {
    fn default() -> Self {
        Self {
            local_port: default_hub_port(),
            auth_type: default_hub_auth_type(),
            tool_profile: default_hub_tool_profile(),
            public_url: String::new(),
            use_global_gateway: false,
            tunnel_type: default_hub_tunnel_type(),
            openai_tunnel_id: String::new(),
            cloudflare_mode: default_global_gateway_cloudflare_mode(),
            frp_profile_id: String::new(),
            frp_subdomain: String::new(),
            use_proxy: default_global_gateway_use_proxy(),
            members: Vec::new(),
            work_sessions: false,
            work_session_limits: WorkSessionLimits::default(),
            restore_on_launch: false,
        }
    }
}

fn default_frp_server_port() -> u16 {
    7000
}

fn default_proxy_mode() -> String {
    "system".to_string()
}

fn default_global_gateway_port() -> u16 {
    28765
}
fn default_global_gateway_tunnel_type() -> String {
    "none".to_string()
}
fn default_global_gateway_cloudflare_mode() -> String {
    "quick".to_string()
}
fn default_global_gateway_use_proxy() -> bool {
    true
}
/// 工作区 MCP 从 28766 往上分配、全局入口是 28765，往下错开一个就不会撞。
fn default_hub_port() -> u16 {
    28764
}
fn default_hub_auth_type() -> String {
    "oauth".to_string()
}
fn default_hub_tool_profile() -> String {
    "compact".to_string()
}
fn default_hub_tunnel_type() -> String {
    "none".to_string()
}

pub fn normalize_tool_profile(profile: &str) -> &'static str {
    match profile {
        "compact" => "compact",
        "advanced" => "advanced",
        "read-only" => "read-only",
        _ => "core",
    }
}

/// 已退役的工具集。它暴露的工具和 advanced 一样多（能写能执行），却把每个都标成
/// `readOnlyHint: true`——客户端据此不再弹确认框，等于替用户把那道真人确认关了
/// （审查 D08）。
pub const RETIRED_COMPAT_PROFILE: &str = "compat-readonly-all";

/// 读配置时把退役的 [`RETIRED_COMPAT_PROFILE`] 换成 advanced：工具一个不少，
/// 标注改回实话。不换的话 [`normalize_tool_profile`] 会把它当成认不出的值、
/// 悄悄降成 core，老配置升级后就少了一半工具。
///
/// 放在反序列化这一层，是因为配置只从这里进来（文件、命令行经 IPC 发回的整份配置），
/// 换过之后内存里就不再有这个值，下次保存也写成 advanced。
pub fn deserialize_tool_profile<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = <String as serde::Deserialize>::deserialize(deserializer)?;
    Ok(if value.trim() == RETIRED_COMPAT_PROFILE {
        "advanced".into()
    } else {
        value
    })
}

#[cfg(test)]
mod tests {
    use super::{FrpProfile, GlobalGatewayConfig, HubConfig, ProxyConfig, WorkSessionLimits};
    use serde_json::json;

    #[test]
    fn accepts_frontend_camel_case_server_port() {
        let profile: FrpProfile = serde_json::from_value(serde_json::json!({
            "id": "p1",
            "name": "公司 FRP",
            "server": "frp.example.com",
            "serverPort": 7004
        }))
        .expect("FRP profile should deserialize");

        assert_eq!(profile.server_port, 7004);
    }

    #[test]
    fn keeps_legacy_snake_case_server_port_compatible() {
        let profile: FrpProfile = serde_json::from_value(serde_json::json!({
            "id": "p1",
            "name": "公司 FRP",
            "server": "frp.example.com",
            "server_port": 7005
        }))
        .expect("legacy FRP profile should deserialize");

        assert_eq!(profile.server_port, 7005);
    }
    fn limits(active: u32, records: u32, bytes: u64) -> WorkSessionLimits {
        WorkSessionLimits {
            max_active: active,
            max_records: records,
            max_bytes: bytes,
        }
    }

    #[test]
    fn defaults_and_invalid_limits_have_explicit_boundaries() {
        let old: HubConfig = serde_json::from_value(json!({})).unwrap();
        assert_eq!(old.work_session_limits, WorkSessionLimits::default());
        for bad in [
            limits(0, 2, 1),
            limits(3, 2, 1),
            limits(1, 8193, 1),
            limits(1, 1, 0),
        ] {
            assert!(bad.validate().is_err());
        }
        assert!(limits(1, 1, 1).validate().is_ok());
    }

    #[test]
    fn empty_configs_keep_their_serialized_defaults() {
        let hub: HubConfig = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(
            serde_json::to_value(hub).unwrap(),
            serde_json::json!({
                "localPort": 28764,
                "authType": "oauth",
                "toolProfile": "compact",
                "publicUrl": "",
                "useGlobalGateway": false,
                "tunnelType": "none",
                "openaiTunnelId": "",
                "cloudflareMode": "quick",
                "frpProfileId": "",
                "frpSubdomain": "",
                "useProxy": true,
                "members": [],
                "workSessions": false,
                "workSessionLimits": {
                    "maxActive": 512,
                    "maxRecords": 8192,
                    "maxBytes": 10_737_418_240_u64
                },
                "restoreOnLaunch": false
            })
        );
        let gateway: GlobalGatewayConfig = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(
            serde_json::to_value(gateway).unwrap(),
            serde_json::json!({
                "enabled": false,
                "localPort": 28765,
                "tunnelType": "none",
                "publicUrl": "",
                "cloudflareMode": "quick",
                "frpProfileId": "",
                "frpServer": "",
                "frpSubdomain": "",
                "frpServerPort": 7000,
                "useProxy": true
            })
        );
        let proxy: ProxyConfig = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(
            serde_json::to_value(proxy).unwrap(),
            serde_json::json!({"mode": "system", "url": ""})
        );
        let profile: FrpProfile = serde_json::from_value(serde_json::json!({
            "id": "p1", "name": "test", "server": "frp.example.com"
        }))
        .unwrap();
        assert_eq!(
            serde_json::to_value(profile).unwrap(),
            serde_json::json!({
                "id": "p1", "name": "test", "server": "frp.example.com", "server_port": 7000
            })
        );
    }

    #[test]
    fn profile_deserialization_only_rewrites_the_retired_alias() {
        for (input, expected) in [
            ("compat-readonly-all", "advanced"),
            (" compat-readonly-all ", "advanced"),
            ("read-only", "read-only"),
            (" read-only ", " read-only "),
            ("ADVANCED", "ADVANCED"),
            ("custom", "custom"),
            ("", ""),
        ] {
            let hub: HubConfig = serde_json::from_value(serde_json::json!({
                "toolProfile": input
            }))
            .unwrap();
            assert_eq!(hub.tool_profile, expected);
        }
    }

    #[test]
    fn capacity_config_uses_camel_case_and_rejects_unknown_fields() {
        let limits: WorkSessionLimits = serde_json::from_value(serde_json::json!({
            "maxActive": 2
        }))
        .unwrap();
        assert_eq!(limits.max_active, 2);
        assert_eq!(limits.max_records, 8192);
        assert_eq!(limits.max_bytes, 10_737_418_240);
        for unknown in ["max_active", "maxRecord", "extra"] {
            assert!(
                serde_json::from_value::<WorkSessionLimits>(serde_json::json!({unknown: 2}))
                    .is_err(),
                "{unknown}"
            );
        }
    }
}
