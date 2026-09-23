//! 配置体检。
//!
//! 目标是把“连不上 / 起不来”这类模糊问题，变成一条条能照着做的修复命令。
//! 每一项检查都要能回答两个问题：**哪里不对**（detail）、**怎么办**（fix）。
//! 只报现象不给做法的检查不要加。
//!
//! 检查分两类：
//!
//! - **配置一致性**（[`service_checks`]、[`config_checks`]）：只看 profiles +
//!   settings + 密钥是否存在，纯函数，可单测；
//! - **环境**：数据目录权限、端口占用、外部二进制是否就位，依赖操作系统状态。

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::App;
use crate::error::AppResult;
use crate::hub::{
    self,
    runtime::{HubState, TunnelSnapshot},
    HUB_SCOPE,
};
use crate::platform::platform;
use crate::runtime::{is_own_process, ServiceKind};
use crate::settings::{AppSettings, HubConfig};
use crate::workspace::WorkspaceProfile;

/// 服务那几条检查的归属。
pub const SERVICE_SCOPE: &str = "MCP 服务";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DoctorLevel {
    /// 正常。
    Ok,
    /// 现在能用，但有风险或将来会出问题。
    Warn,
    /// 已经坏了或一定起不来。
    Fail,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoctorCheck {
    /// 归属：`环境`、`MCP 服务`、`全局入口` 或项目名称。
    pub scope: String,
    pub label: String,
    pub level: DoctorLevel,
    pub detail: String,
    /// 怎么修；正常项为空。
    #[serde(default)]
    pub fix: String,
}

impl DoctorCheck {
    fn ok(scope: &str, label: &str, detail: impl Into<String>) -> Self {
        Self {
            scope: scope.into(),
            label: label.into(),
            level: DoctorLevel::Ok,
            detail: detail.into(),
            fix: String::new(),
        }
    }

    fn warn(scope: &str, label: &str, detail: impl Into<String>, fix: impl Into<String>) -> Self {
        Self {
            scope: scope.into(),
            label: label.into(),
            level: DoctorLevel::Warn,
            detail: detail.into(),
            fix: fix.into(),
        }
    }

    fn fail(scope: &str, label: &str, detail: impl Into<String>, fix: impl Into<String>) -> Self {
        Self {
            scope: scope.into(),
            label: label.into(),
            level: DoctorLevel::Fail,
            detail: detail.into(),
            fix: fix.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Diagnosis {
    pub checks: Vec<DoctorCheck>,
}

impl Diagnosis {
    pub fn count(&self, level: DoctorLevel) -> usize {
        self.checks
            .iter()
            .filter(|check| check.level == level)
            .count()
    }

    /// 没有任何 Fail 就算健康（Warn 不影响退出码）。
    pub fn is_healthy(&self) -> bool {
        self.count(DoctorLevel::Fail) == 0
    }
}

/// 一个项目某项密钥是否已设置（Actions 用）。
///
/// 抽成 trait object 是为了让 [`config_checks`] 不依赖 `App` 和磁盘，可以单测。
pub type SecretLookup<'a> = &'a dyn Fn(&str, &str, bool) -> bool;

impl App {
    /// 跑一遍全部体检项。
    pub async fn doctor(&self) -> AppResult<Diagnosis> {
        let profiles = self.list_workspaces()?;
        let settings = self.settings()?;
        let secret_present = |workspace_id: &str, key: &str, shared: bool| -> bool {
            let found = if shared {
                self.with_data(|store| Ok(store.get_shared_secret(key)))
            } else {
                self.with_data(|store| store.get_workspace_secret(workspace_id, key))
            };
            found
                .ok()
                .flatten()
                .is_some_and(|value| !value.trim().is_empty())
        };
        let service_secret_present = |key: &str| -> bool {
            self.with_data(|store| Ok(store.get_app_secret(HUB_SCOPE, key)))
                .ok()
                .flatten()
                .is_some_and(|value| !value.trim().is_empty())
        };

        let mut checks = environment_checks(&settings, &profiles);
        checks.extend(service_checks(
            &profiles,
            &settings,
            &service_secret_present,
        ));
        checks.push(tunnel_check(
            &settings.hub,
            hub::runtime::tunnel_snapshot().await.as_ref(),
        ));
        checks.extend(config_checks(&profiles, &settings, &secret_present));
        checks.extend(self.port_checks(&profiles, &settings).await?);
        Ok(Diagnosis { checks })
    }

    async fn port_checks(
        &self,
        profiles: &[WorkspaceProfile],
        settings: &AppSettings,
    ) -> AppResult<Vec<DoctorCheck>> {
        let running = !matches!(hub::runtime::state().await, HubState::Stopped);
        let mut checks = vec![service_port_check(
            settings.hub.local_port,
            running,
            occupant(settings.hub.local_port),
        )];
        for profile in profiles {
            for kind in ServiceKind::ALL {
                let running =
                    self.with_runtime(|runtime| Ok(runtime.is_running(&profile.id, kind)))?;
                // 项目自己的 MCP 端口是单项目服务留下的（RFC-0004 之后没有命令会
                // 去监听它）：没在跑就不报，免得为一个用不上的端口让人去改配置。
                // Actions 没在跑、也没配过公网入口，就是没在用，同理。
                let in_use = running
                    || (kind == ServiceKind::Actions
                        && (profile.actions.tunnel_type != "none"
                            || !profile.actions.public_url.trim().is_empty()));
                if !in_use {
                    continue;
                }
                let port = match kind {
                    ServiceKind::Mcp => profile.runtime.local_port,
                    ServiceKind::Actions => profile.actions.local_port,
                };
                checks.push(port_check(
                    &profile.name,
                    kind,
                    port,
                    running,
                    occupant(port),
                ));
            }
        }
        Ok(checks)
    }
}

fn occupant(port: u16) -> Option<PortOccupant> {
    platform()
        .find_pid_listening_on_port(port)
        .ok()
        .flatten()
        .map(|pid| PortOccupant {
            is_self: is_own_process(pid),
            image: platform()
                .process_image_path(pid)
                .ok()
                .flatten()
                .unwrap_or_else(|| format!("pid {pid}")),
        })
}

/// 占用端口的进程。
#[derive(Debug, Clone)]
pub struct PortOccupant {
    /// 占用者就是本进程（也就是我们自己的监听器）。
    pub is_self: bool,
    /// 可执行文件路径，取不到时退化成 `pid <n>`。
    pub image: String,
}

/// 项目自己那几条服务（Actions，以及还在跑的旧单项目 MCP）的端口判定。
/// 抽成纯函数是为了让每个分支（连同它的修复命令）都能被测到。
pub fn port_check(
    workspace: &str,
    kind: ServiceKind,
    port: u16,
    running: bool,
    occupant: Option<PortOccupant>,
) -> DoctorCheck {
    let label = format!("{} 端口 {port}", kind.label().trim());
    match (running, occupant) {
        (true, None) => DoctorCheck::fail(
            workspace,
            &label,
            "标记为运行中，但没有进程在监听这个端口",
            match kind {
                ServiceKind::Mcp => "gld stop（这是旧的单项目服务，现在只用一个服务）".to_string(),
                ServiceKind::Actions => format!("gld restart -w {workspace} -s actions"),
            },
        ),
        (true, Some(_)) => DoctorCheck::ok(workspace, &label, "运行中"),
        (false, Some(occupant)) if !occupant.is_self => DoctorCheck::warn(
            workspace,
            &label,
            format!("未启动，但端口已被占用：{}", occupant.image),
            format!("换端口：gld set {workspace} actions.port=<其他端口>"),
        ),
        (false, _) => DoctorCheck::ok(workspace, &label, "空闲"),
    }
}

/// 服务端口的判定，修复命令指向服务级的命令。
pub fn service_port_check(port: u16, running: bool, occupant: Option<PortOccupant>) -> DoctorCheck {
    let label = format!("端口 {port}");
    match (running, occupant) {
        (true, None) => DoctorCheck::fail(
            SERVICE_SCOPE,
            &label,
            "标记为运行中，但没有进程在监听这个端口",
            "gld restart",
        ),
        (true, Some(_)) => DoctorCheck::ok(SERVICE_SCOPE, &label, "运行中"),
        (false, Some(occupant)) if !occupant.is_self => DoctorCheck::warn(
            SERVICE_SCOPE,
            &label,
            format!("未启动，但端口已被占用：{}", occupant.image),
            "换端口：gld upgrade --port <其他端口>",
        ),
        (false, _) => DoctorCheck::ok(SERVICE_SCOPE, &label, "空闲"),
    }
}

/// 隧道二进制是否就位。`installed_at` 为 `None` 表示没找到。
///
/// gld 不代管这两个程序（0.3.0 起去掉了 `gld software`），所以修复建议给的是
/// 系统包管理器的命令——装完在 PATH 里就能被认出来，不需要再告诉 gld。
pub fn software_check(kind: &str, installed_at: Option<&str>) -> DoctorCheck {
    match installed_at {
        Some(path) => DoctorCheck::ok("环境", kind, path.to_string()),
        None => DoctorCheck::fail(
            "环境",
            kind,
            "配置了这种隧道，但 PATH 里找不到可执行文件",
            match kind {
                "cloudflared" => {
                    "brew install cloudflared（Windows: winget install Cloudflare.cloudflared）"
                }
                _ => "brew install frpc（其他平台见 https://github.com/fatedier/frp/releases）",
            },
        ),
    }
}

/// 数据目录、外部二进制这类与操作系统有关的检查。
fn environment_checks(settings: &AppSettings, profiles: &[WorkspaceProfile]) -> Vec<DoctorCheck> {
    const SCOPE: &str = "环境";
    let mut checks = Vec::new();

    match crate::home::data_home() {
        Ok(home) => {
            checks.push(match std::fs::create_dir_all(&home) {
                Ok(()) => DoctorCheck::ok(SCOPE, "数据目录", home.display().to_string()),
                Err(error) => DoctorCheck::fail(
                    SCOPE,
                    "数据目录",
                    format!("{} 无法创建或写入：{error}", home.display()),
                    "检查目录权限，或用 GLD_HOME 指定另一个位置",
                ),
            });
            if let Some(check) =
                data_file_permission_check(&home.join("data").join("profiles.json"))
            {
                checks.push(check);
            }
        }
        Err(error) => checks.push(DoctorCheck::fail(
            SCOPE,
            "数据目录",
            error.to_string(),
            "设置 HOME 或 GLD_HOME 环境变量",
        )),
    }

    // 只有真的会用到隧道时才检查二进制，否则纯本机用户会看到一堆无关的红叉。
    let uses = |tunnel_type: &str| {
        settings.hub.tunnel_type == tunnel_type
            || profiles
                .iter()
                .any(|profile| actions_uses(profile, tunnel_type))
            || (settings.global_gateway.enabled
                && settings.global_gateway.tunnel_type == tunnel_type)
    };
    for (kind, tunnel_type) in [("frpc", "frp"), ("cloudflared", "cloudflare")] {
        if !uses(tunnel_type) {
            continue;
        }
        let found = crate::tunnel::tunnel_binary_path(kind);
        checks.push(software_check(kind, found.as_deref()));
    }

    checks
}

fn actions_uses(profile: &WorkspaceProfile, tunnel_type: &str) -> bool {
    !profile.actions.use_global_gateway && profile.actions.tunnel_type == tunnel_type
}

#[cfg(unix)]
fn data_file_permission_check(path: &Path) -> Option<DoctorCheck> {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(path).ok()?.permissions().mode() & 0o777;
    Some(if mode & 0o077 == 0 {
        DoctorCheck::ok("环境", "配置文件权限", format!("{mode:03o}"))
    } else {
        DoctorCheck::warn(
            "环境",
            "配置文件权限",
            format!(
                "{} 权限是 {mode:03o}，同机其他用户可读，里面有密钥明文",
                path.display()
            ),
            format!("chmod 600 {}", path.display()),
        )
    })
}

#[cfg(not(unix))]
fn data_file_permission_check(_path: &Path) -> Option<DoctorCheck> {
    None
}

/// 服务（RFC-0004 之后唯一的那个 MCP 入口）自己的检查：认证、公网入口、项目。
///
/// 纯函数：`token_present` 回答"服务凭据里这一项设过没有"。
pub fn service_checks(
    profiles: &[WorkspaceProfile],
    settings: &AppSettings,
    token_present: &dyn Fn(&str) -> bool,
) -> Vec<DoctorCheck> {
    let hub = &settings.hub;
    let mut checks = Vec::new();

    // noauth 本身没问题，前提是**外面真的进不来**。隧道就是从 127.0.0.1 把端口
    // 转到公网的，绑回环地址对它一点约束都没有。
    let exposure = if hub.tunnel_type != "none" {
        Some(format!("配了 {} 隧道", hub.tunnel_type))
    } else if hub.use_global_gateway {
        Some("接进了全局入口".to_string())
    } else if !hub.public_url.trim().is_empty() {
        Some("填了公网地址".to_string())
    } else {
        None
    };
    checks.push(match (hub.auth_type.as_str(), exposure) {
        ("noauth", Some(reason)) => DoctorCheck::fail(
            SERVICE_SCOPE,
            "认证",
            format!("noauth 但{reason}：任何人都能读写全部项目并执行命令"),
            "gld upgrade --auth oauth；或 gld share --off 收回公网入口",
        ),
        ("noauth", None) if settings.allow_lan_access => DoctorCheck::fail(
            SERVICE_SCOPE,
            "认证",
            "noauth 且已开启局域网访问：同网段任何人都能读写全部项目并执行命令",
            "gld upgrade --auth bearer；或关闭局域网访问 gld cfg runtime --lan-access false",
        ),
        ("noauth", None) => DoctorCheck::warn(
            SERVICE_SCOPE,
            "认证",
            "noauth（仅监听 127.0.0.1，且没有公网入口）",
            "本机自用可以；开局域网访问或开公网入口之前必须换成 bearer 或 oauth",
        ),
        (auth, _) => DoctorCheck::ok(SERVICE_SCOPE, "认证", auth.to_string()),
    });

    const ENTRY: &str = "公网入口";
    checks.push(match hub.tunnel_type.as_str() {
        "frp" => match settings.find_frp_profile(&hub.frp_profile_id) {
            None if hub.frp_profile_id.trim().is_empty() => DoctorCheck::fail(
                SERVICE_SCOPE,
                ENTRY,
                "隧道类型是 frp，但没有选择 FRP 配置",
                "gld frp add --name <名称> --server <frps地址> --token <token> 后再 gld share --tunnel frp:<名称>",
            ),
            None => DoctorCheck::fail(
                SERVICE_SCOPE,
                ENTRY,
                format!(
                    "引用的 FRP 配置 {} 不存在（多半是被 gld frp remove --force 删掉了）",
                    hub.frp_profile_id
                ),
                "gld frp list 看现有的，再 gld share --tunnel frp:<名称>；不要公网就 gld share --off",
            ),
            Some(_) if hub.frp_subdomain.trim().is_empty() => DoctorCheck::fail(
                SERVICE_SCOPE,
                ENTRY,
                "选了 FRP 配置但没有子域名，公网地址无法生成",
                "gld share --tunnel frp:<名称> --subdomain <子域名>",
            ),
            Some(frp) => DoctorCheck::ok(
                SERVICE_SCOPE,
                ENTRY,
                format!("frp https://{}.{}", hub.frp_subdomain.trim(), frp.server),
            ),
        },
        "cloudflare" if hub.cloudflare_mode == "named" => {
            if hub.public_url.trim().is_empty() {
                DoctorCheck::fail(
                    SERVICE_SCOPE,
                    ENTRY,
                    "Cloudflare 固定域名模式没有域名",
                    "gld share --tunnel cf:mcp.example.com",
                )
            } else if !token_present("cloudflare_token") {
                DoctorCheck::fail(
                    SERVICE_SCOPE,
                    ENTRY,
                    "Cloudflare 固定域名模式缺少 Tunnel Token",
                    "gld secret set cloudflare_token <token>",
                )
            } else {
                DoctorCheck::ok(
                    SERVICE_SCOPE,
                    ENTRY,
                    format!("cloudflare 固定域名 {}", hub.public_url),
                )
            }
        }
        // 这不算错，但得说出来：用户来跑 doctor 常常就是因为"地址昨天还好好的"。
        "cloudflare" => DoctorCheck::ok(
            SERVICE_SCOPE,
            ENTRY,
            "cloudflare 临时地址：每次重启都会变，客户端要跟着改",
        ),
        _ if hub.use_global_gateway => {
            if settings.global_gateway.enabled {
                DoctorCheck::ok(SERVICE_SCOPE, ENTRY, "经全局入口（/hub）")
            } else {
                DoctorCheck::fail(
                    SERVICE_SCOPE,
                    ENTRY,
                    "配置为经全局入口，但全局入口没启用",
                    "改用服务自己的入口：gld share --tunnel cf",
                )
            }
        }
        _ if !hub.public_url.trim().is_empty() => {
            DoctorCheck::ok(SERVICE_SCOPE, ENTRY, hub.public_url.clone())
        }
        _ => DoctorCheck::warn(
            SERVICE_SCOPE,
            ENTRY,
            "没有公网入口，只能本机访问",
            "本机客户端（Claude Code / Cursor）够用；ChatGPT 需要公网地址：gld share",
        ),
    });

    // 登记了却不在服务里：RFC-0004 之前的老数据才会这样。AI 看不见它们，
    // 而用户以为加过了。
    let outside: Vec<&str> = profiles
        .iter()
        .filter(|profile| !hub.members.contains(&profile.id))
        .map(|profile| profile.name.as_str())
        .collect();
    if !outside.is_empty() {
        checks.push(DoctorCheck::warn(
            SERVICE_SCOPE,
            "项目",
            format!("登记了但不在服务里，AI 看不见：{}", outside.join("、")),
            "gld start（会把它们加进来并逐个列出）",
        ));
    }

    checks
}

/// 隧道此刻在不在：配置对不对由 [`service_checks`] 的"公网入口"那条管，这里只看
/// 运行时——隧道进程还在不在、这次实际用的是哪个地址、起隧道时报了什么错。
///
/// 为什么要单独一条：配置全对、服务也在跑，而 cloudflared 自己退了（token 失效、
/// 被 kill、放弃重连）时，本机客户端一切正常，只有公网那一头没了。这种情况以前
/// 只在 `gld ls` 的一行地址里看不出来，体检也一个字不说。
///
/// 纯函数：`snapshot` 是 `None` 表示服务没在跑。
pub fn tunnel_check(hub: &HubConfig, snapshot: Option<&TunnelSnapshot>) -> DoctorCheck {
    const TUNNEL: &str = "隧道";
    let configured = hub.public_url.trim().trim_end_matches('/');
    let Some(snapshot) = snapshot else {
        return DoctorCheck::ok(SERVICE_SCOPE, TUNNEL, "服务没在跑，隧道也没起");
    };
    if !snapshot.error.is_empty() {
        return DoctorCheck::fail(
            SERVICE_SCOPE,
            TUNNEL,
            format!(
                "隧道没起来：{}。服务本身在跑，本机客户端不受影响",
                snapshot.error.trim()
            ),
            "修好上面那个原因后 gld restart；只用本机就 gld share --off",
        );
    }
    let effective = snapshot.public_base.trim().trim_end_matches('/');
    if snapshot.managed {
        let pid = snapshot
            .pid
            .map(|pid| format!("pid {pid}"))
            .unwrap_or_else(|| "拿不到 pid".to_string());
        if snapshot.alive == Some(false) {
            return DoctorCheck::fail(
                SERVICE_SCOPE,
                TUNNEL,
                format!(
                    "隧道进程（{pid}）已经退出，公网地址现在不通；服务还在本地跑，所以本机客户端看不出异常"
                ),
                "gld restart（服务和隧道一起重起）；反复退出看它自己的日志：gld logs",
            );
        }
        let temporary = hub.tunnel_type == "cloudflare" && hub.cloudflare_mode != "named";
        let detail = if effective.is_empty() {
            format!("{} 隧道在跑（{pid}），但还没拿到公网地址", hub.tunnel_type)
        } else if temporary {
            format!(
                "{} 临时地址 {effective}（{pid}）；每次重启都会变",
                hub.tunnel_type
            )
        } else {
            format!("{} 隧道在跑（{pid}）：{effective}", hub.tunnel_type)
        };
        return DoctorCheck::ok(SERVICE_SCOPE, TUNNEL, detail);
    }
    if effective.is_empty() {
        return DoctorCheck::ok(SERVICE_SCOPE, TUNNEL, "没有公网入口，服务只在本机地址上");
    }
    // 自建反代、已有公网地址：进程不是 gld 起的，gld 只知道这个地址被登记了。
    let mut detail = format!("{effective}：这条链路不是 gld 起的（自建入口），它通不通 gld 看不到");
    if !configured.is_empty() && configured != effective {
        detail = format!("{detail}；配置里写的是 {configured}，服务这次用的是 {effective}");
    }
    DoctorCheck::ok(SERVICE_SCOPE, TUNNEL, detail)
}

/// 纯配置检查：不碰磁盘、不碰端口，只看配置之间是否自洽。
pub fn config_checks(
    profiles: &[WorkspaceProfile],
    settings: &AppSettings,
    secret_present: SecretLookup<'_>,
) -> Vec<DoctorCheck> {
    let mut checks = Vec::new();
    if profiles.is_empty() {
        checks.push(DoctorCheck::warn(
            "环境",
            "项目",
            "还没有登记任何项目",
            "gld add <项目目录>",
        ));
        return checks;
    }

    checks.extend(duplicate_port_checks(profiles, settings));
    checks.extend(duplicate_subdomain_checks(profiles, settings));
    checks.extend(gateway_checks(settings));

    for profile in profiles {
        let scope = profile.name.as_str();

        // 目录还在吗——路径被移走 / 删除后，所有工具调用都会失败。
        let root = Path::new(&profile.path);
        checks.push(if root.is_dir() {
            DoctorCheck::ok(scope, "项目目录", profile.path.clone())
        } else {
            DoctorCheck::fail(
                scope,
                "项目目录",
                format!("{} 不存在或不是目录", profile.path),
                format!("目录搬走了就改过去：gld upgrade {scope} --path <新路径>"),
            )
        });
        if root.is_dir() && !root.join(".git").exists() {
            checks.push(DoctorCheck::warn(
                scope,
                "版本控制",
                "项目不是 Git 仓库",
                "建议 git init：Patch 只在单次操作内回滚，长期恢复依赖 Git 历史",
            ));
        }

        checks.extend(actions_auth_checks(profile, secret_present));
        checks.extend(actions_tunnel_checks(profile, settings, secret_present));
    }

    checks
}

/// 全局入口自身的配置。
///
/// 它不属于任何项目，所以逐项目的那轮检查看不到它。最典型的漏网之鱼是
/// `gld frp remove --force`：入口引用的 FRP 配置被删掉之后，项目那边全绿，
/// 只有 `gld gateway start` 会失败，而那时的报错只说隧道起不来。
fn gateway_checks(settings: &AppSettings) -> Vec<DoctorCheck> {
    const SCOPE: &str = "全局入口";
    let gateway = &settings.global_gateway;
    let mut checks = Vec::new();
    if !gateway.enabled {
        return checks;
    }

    match gateway.tunnel_type.as_str() {
        "frp" => {
            let profile_id = gateway.frp_profile_id.trim();
            if profile_id.is_empty() {
                checks.push(DoctorCheck::fail(
                    SCOPE,
                    "FRP 配置",
                    "隧道类型是 frp，但没有选择 FRP 配置",
                    "gld frp list 看有哪些，再 gld gateway set --frp-profile <名称>",
                ));
            } else if settings.find_frp_profile(profile_id).is_none() {
                checks.push(DoctorCheck::fail(
                    SCOPE,
                    "FRP 配置",
                    format!(
                        "引用的 FRP 配置 {profile_id} 不存在（多半是被 gld frp remove --force 删掉了）"
                    ),
                    "gld frp list 看现有的，再 gld gateway set --frp-profile <名称>",
                ));
            } else if gateway.frp_subdomain.trim().is_empty() {
                checks.push(DoctorCheck::fail(
                    SCOPE,
                    "子域名",
                    "选了 FRP 配置但没有填子域名，公网地址无法生成",
                    "gld gateway set --frp-subdomain <小写字母 / 数字 / 连字符>",
                ));
            } else {
                checks.push(DoctorCheck::ok(
                    SCOPE,
                    "隧道",
                    format!("frp · 子域名 {}", gateway.frp_subdomain),
                ));
            }
        }
        // quick 是它唯一支持的 Cloudflare 模式，地址每次重启都变。
        "cloudflare" => checks.push(DoctorCheck::ok(
            SCOPE,
            "隧道",
            "cloudflare quick：地址每次重启都会变，客户端要跟着改",
        )),
        _ if gateway.public_url.trim().is_empty() => checks.push(DoctorCheck::fail(
            SCOPE,
            "公网地址",
            "入口已启用，但既没有隧道也没有手动地址，接入它的服务拿不到公网地址",
            "gld gateway set --tunnel frp --frp-profile <名称> --frp-subdomain <子域名>，\
             或用现成地址：--tunnel off --public-url <地址>",
        )),
        _ => checks.push(DoctorCheck::ok(
            SCOPE,
            "公网地址",
            gateway.public_url.clone(),
        )),
    }

    checks
}

/// 端口撞车：服务端口、全局入口端口、各项目的 Actions 端口。
///
/// 项目自己那个 MCP 端口不算：单项目服务没了，那个号码不会被监听。
fn duplicate_port_checks(
    profiles: &[WorkspaceProfile],
    settings: &AppSettings,
) -> Vec<DoctorCheck> {
    let mut owners: HashMap<u16, Vec<String>> = HashMap::new();
    owners
        .entry(settings.hub.local_port)
        .or_default()
        .push(SERVICE_SCOPE.to_string());
    if settings.global_gateway.enabled {
        owners
            .entry(settings.global_gateway.local_port)
            .or_default()
            .push("全局入口".to_string());
    }
    for profile in profiles {
        owners
            .entry(profile.actions.local_port)
            .or_default()
            .push(format!("{} Actions", profile.name));
    }
    let mut duplicates: Vec<_> = owners
        .into_iter()
        .filter(|(_, users)| users.len() > 1)
        .collect();
    duplicates.sort_by_key(|(port, _)| *port);
    duplicates
        .into_iter()
        .map(|(port, users)| {
            // 修复提示要指向能改的那一个：服务在里面就改服务，否则改某个项目的 Actions。
            let fix = if users.iter().any(|user| user == SERVICE_SCOPE) {
                "给服务换端口：gld upgrade --port <其他端口>".to_string()
            } else {
                "给其中一个换端口：gld set <项目> actions.port=<其他端口>".to_string()
            };
            DoctorCheck::fail(
                "环境",
                &format!("端口 {port} 冲突"),
                format!("被多个服务同时占用：{}", users.join("、")),
                fix,
            )
        })
        .collect()
}

fn duplicate_subdomain_checks(
    profiles: &[WorkspaceProfile],
    settings: &AppSettings,
) -> Vec<DoctorCheck> {
    let mut owners: HashMap<String, Vec<String>> = HashMap::new();
    if settings.hub.tunnel_type == "frp" && !settings.hub.frp_subdomain.trim().is_empty() {
        owners
            .entry(settings.hub.frp_subdomain.trim().to_ascii_lowercase())
            .or_default()
            .push(SERVICE_SCOPE.to_string());
    }
    for profile in profiles {
        let subdomain = profile.actions.frp_subdomain.trim();
        if !actions_uses(profile, "frp") || subdomain.is_empty() {
            continue;
        }
        owners
            .entry(subdomain.to_ascii_lowercase())
            .or_default()
            .push(format!("{} Actions", profile.name));
    }
    let mut duplicates: Vec<_> = owners
        .into_iter()
        .filter(|(_, users)| users.len() > 1)
        .collect();
    duplicates.sort_by(|left, right| left.0.cmp(&right.0));
    duplicates
        .into_iter()
        .map(|(subdomain, users)| {
            DoctorCheck::fail(
                "环境",
                &format!("FRP 子域名 {subdomain} 冲突"),
                format!("被多个服务同时使用：{}", users.join("、")),
                "每个服务用不同子域名：gld set <项目> actions.frp-subdomain=<其他名字>",
            )
        })
        .collect()
}

/// Actions 是唯一还按项目起的服务（自定义 GPT 导入的是一个项目的 OpenAPI 文档）。
fn actions_auth_checks(
    profile: &WorkspaceProfile,
    secret_present: SecretLookup<'_>,
) -> Vec<DoctorCheck> {
    let scope = profile.name.as_str();
    let shared = profile.actions.use_shared_secrets;
    let missing: Vec<&str> = match profile.actions.auth_type.as_str() {
        "api_key" => ["actions_api_key"]
            .into_iter()
            .filter(|key| !secret_present(&profile.id, key, shared))
            .collect(),
        "oauth" => ["actions_oauth_password", "actions_oauth_token_secret"]
            .into_iter()
            .filter(|key| !secret_present(&profile.id, key, shared))
            .collect(),
        _ => Vec::new(),
    };
    if missing.is_empty() {
        return Vec::new();
    }
    vec![DoctorCheck::fail(
        scope,
        "Actions 认证",
        format!(
            "{} 缺少凭据：{}",
            profile.actions.auth_type,
            missing.join("、")
        ),
        format!(
            "gld secret {}regen {} -w {scope}（Actions 未启用时可忽略）",
            if shared { "shared " } else { "" },
            missing[0]
        ),
    )]
}

fn actions_tunnel_checks(
    profile: &WorkspaceProfile,
    settings: &AppSettings,
    secret_present: SecretLookup<'_>,
) -> Vec<DoctorCheck> {
    const LABEL: &str = "Actions 公网入口";
    let scope = profile.name.as_str();
    let actions = &profile.actions;
    if actions.use_global_gateway {
        return vec![if settings.global_gateway.enabled {
            DoctorCheck::ok(scope, LABEL, "走全局共享入口")
        } else {
            DoctorCheck::fail(
                scope,
                LABEL,
                "配置为走全局入口，但全局入口未启用",
                "gld gateway set --enabled true",
            )
        }];
    }
    match actions.tunnel_type.as_str() {
        "frp" => {
            let known = settings.find_frp_profile(&actions.frp_profile_id);
            vec![
                if !actions.frp_profile_id.trim().is_empty() && known.is_none() {
                    DoctorCheck::fail(
                        scope,
                        LABEL,
                        format!(
                            "引用的 FRP 配置 {} 不存在（多半是被 gld frp remove --force 删掉了）",
                            actions.frp_profile_id
                        ),
                        format!(
                        "gld frp list 看现有的，再 gld set {scope} actions.frp-profile=<名称>；\
                         不要公网就 gld share --off -w {scope} -s actions"
                    ),
                    )
                } else if known.is_none() {
                    DoctorCheck::fail(
                    scope,
                    LABEL,
                    "隧道类型是 frp，但没有选择 FRP 配置",
                    "gld frp add --name <名称> --server <frps地址> --token <token> 后再 gld set",
                )
                } else if actions.frp_subdomain.trim().is_empty() {
                    DoctorCheck::fail(
                        scope,
                        LABEL,
                        "选了 FRP 配置但没有填子域名，公网地址无法生成",
                        format!("gld set {scope} actions.frp-subdomain=<子域名>"),
                    )
                } else {
                    DoctorCheck::ok(
                        scope,
                        LABEL,
                        format!(
                            "frp https://{}.{}",
                            actions.frp_subdomain.trim(),
                            known.map(|item| item.server.as_str()).unwrap_or("")
                        ),
                    )
                },
            ]
        }
        "cloudflare" => vec![if actions.cloudflare_mode == "named"
            && !secret_present(&profile.id, "actions_cloudflare_token", false)
        {
            DoctorCheck::fail(
                scope,
                LABEL,
                "Cloudflare named 模式缺少 tunnel token",
                format!("gld secret set actions_cloudflare_token <token> -w {scope}"),
            )
        } else {
            DoctorCheck::ok(
                scope,
                LABEL,
                format!("cloudflare（{}）", actions.cloudflare_mode),
            )
        }],
        // 手填了公网地址就报一句；出厂状态（无隧道、无地址）就是"没在用"，
        // 不该给只用 MCP 的人报一条与他无关的提醒。
        _ if !actions.public_url.trim().is_empty() => {
            vec![DoctorCheck::ok(scope, LABEL, actions.public_url.clone())]
        }
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(name: &str, path: &str) -> WorkspaceProfile {
        let mut profile = WorkspaceProfile::new(path.into(), Some(name.into()));
        profile.id = format!("id-{name}");
        profile.tunnel.tunnel_type = "none".into();
        profile.actions.tunnel_type = "none".into();
        profile.auth.auth_type = "bearer".into();
        profile.actions.auth_type = "none".into();
        profile
    }

    /// 服务里已经有这几个项目的设置。
    fn settings_with(members: &[&WorkspaceProfile]) -> AppSettings {
        let mut settings = AppSettings::default();
        settings.hub.members = members.iter().map(|item| item.id.clone()).collect();
        settings
    }

    fn all_present(_: &str, _: &str, _: bool) -> bool {
        true
    }

    fn find<'a>(checks: &'a [DoctorCheck], label: &str) -> Option<&'a DoctorCheck> {
        checks.iter().find(|check| check.label.contains(label))
    }

    fn frp_profile(settings: &mut AppSettings) {
        settings.frp_profiles.push(crate::settings::FrpProfile {
            id: "p1".into(),
            name: "p".into(),
            server: "frp.example.com".into(),
            server_port: 7000,
        });
    }

    #[test]
    fn a_healthy_setup_reports_no_failures() {
        let temp = tempfile::tempdir().expect("workspace");
        std::fs::create_dir_all(temp.path().join(".git")).unwrap();
        let item = profile("solo", temp.path().to_str().unwrap());
        let settings = settings_with(&[&item]);
        let mut checks = config_checks(std::slice::from_ref(&item), &settings, &all_present);
        checks.extend(service_checks(
            std::slice::from_ref(&item),
            &settings,
            &|_| true,
        ));
        assert!(
            !checks.iter().any(|check| check.level == DoctorLevel::Fail),
            "{checks:#?}"
        );
    }

    #[test]
    fn noauth_on_the_service_plus_lan_access_is_a_failure() {
        let mut settings = AppSettings {
            allow_lan_access: true,
            ..AppSettings::default()
        };
        settings.hub.auth_type = "noauth".into();
        let checks = service_checks(&[], &settings, &|_| true);
        assert_eq!(find(&checks, "认证").unwrap().level, DoctorLevel::Fail);

        // 只监听本机、又没有公网入口时降为提醒。
        settings.allow_lan_access = false;
        let checks = service_checks(&[], &settings, &|_| true);
        assert_eq!(find(&checks, "认证").unwrap().level, DoctorLevel::Warn);
    }

    /// noauth + 任何一种公网入口都是 Fail，哪怕只监听 127.0.0.1。
    ///
    /// 隧道就是从 127.0.0.1 把端口转到公网的，绑回环地址对它没有任何约束。
    #[test]
    fn noauth_plus_a_public_entrance_is_a_failure_even_on_loopback() {
        for kind in ["tunnel", "public_url", "gateway"] {
            let mut settings = AppSettings::default();
            settings.hub.auth_type = "noauth".into();
            match kind {
                "tunnel" => settings.hub.tunnel_type = "cloudflare".into(),
                "public_url" => settings.hub.public_url = "https://mcp.example.com".into(),
                _ => settings.hub.use_global_gateway = true,
            }
            let checks = service_checks(&[], &settings, &|_| true);
            let auth = find(&checks, "认证").expect("auth check");
            assert_eq!(auth.level, DoctorLevel::Fail, "{kind}: {auth:#?}");
            assert!(auth.fix.contains("--auth"), "{}", auth.fix);
        }
    }

    /// 项目自己那个 MCP 端口不再有人监听，两个项目分到同一个号码不算冲突；
    /// 服务端口撞上某个项目的 Actions 端口才算，而且修复命令要指向服务。
    #[test]
    fn port_clashes_count_the_service_and_actions_only() {
        let temp = tempfile::tempdir().expect("workspace");
        let path = temp.path().to_str().unwrap();
        let first = profile("a", path);
        let mut second = profile("b", path);
        second.runtime.local_port = first.runtime.local_port;
        second.actions.local_port = first.actions.local_port + 1;
        let settings = settings_with(&[&first, &second]);
        let checks = config_checks(&[first.clone(), second.clone()], &settings, &all_present);
        assert!(
            !checks.iter().any(|check| check.label.contains("冲突")),
            "{checks:#?}"
        );

        let mut settings = settings;
        settings.hub.local_port = first.actions.local_port;
        let checks = config_checks(&[first, second], &settings, &all_present);
        let clash = find(&checks, "冲突").expect("service clash");
        assert_eq!(clash.level, DoctorLevel::Fail);
        assert!(clash.fix.contains("gld upgrade --port"), "{}", clash.fix);
    }

    #[test]
    fn an_actions_only_port_clash_points_at_the_actions_field() {
        let temp = tempfile::tempdir().expect("workspace");
        let path = temp.path().to_str().unwrap();
        let mut first = profile("a", path);
        let mut second = profile("b", path);
        first.actions.local_port = 9000;
        second.actions.local_port = 9000;
        let settings = settings_with(&[&first, &second]);

        let checks = config_checks(&[first, second], &settings, &all_present);
        let clash = checks
            .iter()
            .find(|check| check.label.contains("端口 9000"))
            .expect("actions clash");
        assert!(clash.fix.contains("actions.port"), "{}", clash.fix);
    }

    /// 子域名大小写不同也算冲突：frps 侧不区分大小写。服务和项目的 Actions 抢同一个也算。
    #[test]
    fn a_subdomain_shared_by_the_service_and_an_actions_line_is_reported_once() {
        let temp = tempfile::tempdir().expect("workspace");
        let mut item = profile("a", temp.path().to_str().unwrap());
        item.actions.tunnel_type = "frp".into();
        item.actions.frp_profile_id = "p1".into();
        item.actions.frp_subdomain = "Same".into();
        let mut settings = settings_with(&[&item]);
        frp_profile(&mut settings);
        settings.hub.tunnel_type = "frp".into();
        settings.hub.frp_profile_id = "p1".into();
        settings.hub.frp_subdomain = "same".into();

        let checks = config_checks(&[item], &settings, &all_present);
        let subdomains: Vec<_> = checks
            .iter()
            .filter(|check| check.label.contains("子域名") && check.label.contains("冲突"))
            .collect();
        assert_eq!(subdomains.len(), 1, "{checks:#?}");
    }

    #[test]
    fn an_unused_actions_service_says_nothing() {
        let temp = tempfile::tempdir().expect("workspace");
        let item = profile("local-only", temp.path().to_str().unwrap());
        let settings = settings_with(&[&item]);
        let checks = config_checks(std::slice::from_ref(&item), &settings, &all_present);
        assert!(
            !checks.iter().any(|check| check.label.contains("Actions")),
            "{checks:#?}"
        );

        // 但 Actions 一旦真的配了隧道，就照常检查。
        let mut configured = profile("with-actions", temp.path().to_str().unwrap());
        configured.actions.tunnel_type = "frp".into();
        let checks = config_checks(&[configured], &settings, &all_present);
        assert!(checks
            .iter()
            .any(|check| check.label == "Actions 公网入口" && check.level == DoctorLevel::Fail));
    }

    #[test]
    fn the_service_tunnel_says_what_is_missing_and_how_to_give_it() {
        let mut settings = AppSettings::default();
        settings.hub.tunnel_type = "frp".into();
        let checks = service_checks(&[], &settings, &|_| true);
        let entry = find(&checks, "公网入口").expect("tunnel check");
        assert_eq!(entry.level, DoctorLevel::Fail);
        assert!(entry.fix.contains("gld frp add"), "{}", entry.fix);

        // 有配置但没子域名：换一条提示。
        frp_profile(&mut settings);
        settings.hub.frp_profile_id = "p1".into();
        let checks = service_checks(&[], &settings, &|_| true);
        let entry = find(&checks, "公网入口").expect("tunnel check");
        assert_eq!(entry.level, DoctorLevel::Fail);
        assert!(entry.fix.contains("--subdomain"), "{}", entry.fix);

        // 固定域名缺 token。
        let mut settings = AppSettings::default();
        settings.hub.tunnel_type = "cloudflare".into();
        settings.hub.cloudflare_mode = "named".into();
        settings.hub.public_url = "https://mcp.example.com".into();
        let checks = service_checks(&[], &settings, &|_| false);
        let entry = find(&checks, "公网入口").expect("tunnel check");
        assert_eq!(entry.level, DoctorLevel::Fail);
        assert!(
            entry.fix.contains("gld secret set cloudflare_token"),
            "{}",
            entry.fix
        );
        let checks = service_checks(&[], &settings, &|_| true);
        assert_eq!(find(&checks, "公网入口").unwrap().level, DoctorLevel::Ok);
    }

    /// 老配置经全局入口挂公网：入口没开就起不来。
    #[test]
    fn the_old_gateway_route_requires_the_gateway_to_be_enabled() {
        let mut settings = AppSettings::default();
        settings.hub.use_global_gateway = true;
        let checks = service_checks(&[], &settings, &|_| true);
        assert_eq!(find(&checks, "公网入口").unwrap().level, DoctorLevel::Fail);

        settings.global_gateway.enabled = true;
        let checks = service_checks(&[], &settings, &|_| true);
        assert_eq!(find(&checks, "公网入口").unwrap().level, DoctorLevel::Ok);
    }

    /// RFC-0004 之前可以"登记了但不在 hub 里"：AI 看不见它，得报出来。
    #[test]
    fn a_project_outside_the_service_is_named() {
        let temp = tempfile::tempdir().expect("workspace");
        let inside = profile("inside", temp.path().to_str().unwrap());
        let outside = profile("outside", temp.path().to_str().unwrap());
        let settings = settings_with(&[&inside]);
        let checks = service_checks(&[inside, outside], &settings, &|_| true);
        let entry = find(&checks, "项目").expect("members check");
        assert_eq!(entry.level, DoctorLevel::Warn);
        assert!(entry.detail.contains("outside") && !entry.detail.contains("inside"));
        assert!(entry.fix.contains("gld start"), "{}", entry.fix);
    }

    /// 全局入口不属于任何项目，逐项目那轮检查覆盖不到它。
    /// 少了这条，`gld frp remove --force` 留下的悬空引用在 doctor 里全绿，
    /// 直到 `gld gateway start` 起不来。
    #[test]
    fn a_dangling_frp_reference_in_the_gateway_is_reported() {
        let temp = tempfile::tempdir().expect("workspace");
        let item = profile("hub", temp.path().to_str().unwrap());
        let mut settings = settings_with(&[&item]);
        settings.global_gateway.enabled = true;
        settings.global_gateway.tunnel_type = "frp".into();
        settings.global_gateway.frp_profile_id = "已经没了".into();
        settings.global_gateway.frp_subdomain = "hub".into();

        let checks = config_checks(std::slice::from_ref(&item), &settings, &all_present);
        let entry = find(&checks, "FRP 配置").expect("gateway frp check");
        assert_eq!(entry.level, DoctorLevel::Fail);
        assert_eq!(entry.scope, "全局入口");
        assert!(
            entry.fix.contains("gld gateway set --frp-profile"),
            "修复命令要指向网关自己的字段，不是 ws set：{}",
            entry.fix
        );

        // 配置补回来就该恢复正常。
        settings.frp_profiles.push(crate::settings::FrpProfile {
            id: "已经没了".into(),
            name: "又有了".into(),
            server: "frp.example.com".into(),
            server_port: 7000,
        });
        let checks = config_checks(std::slice::from_ref(&item), &settings, &all_present);
        assert_eq!(
            find(&checks, "隧道").map(|entry| entry.level),
            Some(DoctorLevel::Ok)
        );
    }

    /// 入口开着却没有任何拿地址的办法——接进来的服务会一直没有公网地址。
    #[test]
    fn an_enabled_gateway_without_any_address_is_reported() {
        let temp = tempfile::tempdir().expect("workspace");
        let item = profile("hub", temp.path().to_str().unwrap());
        let mut settings = settings_with(&[&item]);
        settings.global_gateway.enabled = true;
        settings.global_gateway.tunnel_type = "none".into();

        let checks = config_checks(std::slice::from_ref(&item), &settings, &all_present);
        assert_eq!(
            find(&checks, "公网地址").map(|entry| entry.level),
            Some(DoctorLevel::Fail)
        );

        settings.global_gateway.public_url = "https://hub.example.com".into();
        let checks = config_checks(std::slice::from_ref(&item), &settings, &all_present);
        assert_eq!(
            find(&checks, "公网地址").map(|entry| entry.level),
            Some(DoctorLevel::Ok)
        );
    }

    /// 入口没启用就一句都不该报——大多数人从来不用它，
    /// 每次 doctor 多几行噪音会把真正的问题埋掉。
    #[test]
    fn a_disabled_gateway_says_nothing() {
        let temp = tempfile::tempdir().expect("workspace");
        let item = profile("hub", temp.path().to_str().unwrap());
        let mut settings = settings_with(&[&item]);
        settings.global_gateway.tunnel_type = "frp".into();
        settings.global_gateway.frp_profile_id = "已经没了".into();

        let checks = config_checks(&[item], &settings, &all_present);
        assert!(
            checks.iter().all(|entry| entry.scope != "全局入口"),
            "入口没启用时不该有任何全局入口检查：{checks:?}"
        );
    }

    #[test]
    fn a_missing_project_directory_fails() {
        let item = profile("gone", "/definitely/not/here/gld");
        let settings = settings_with(&[&item]);
        let checks = config_checks(&[item], &settings, &all_present);
        let entry = find(&checks, "项目目录").expect("directory check");
        assert_eq!(entry.level, DoctorLevel::Fail);
        assert!(entry.fix.contains("--path"), "{}", entry.fix);
    }

    fn snapshot(
        public_base: &str,
        error: &str,
        managed: bool,
        alive: Option<bool>,
    ) -> TunnelSnapshot {
        TunnelSnapshot {
            public_base: public_base.into(),
            error: error.into(),
            managed,
            pid: managed.then_some(4242),
            alive,
        }
    }

    /// 这条检查存在的理由：配置全对、服务在跑，而隧道进程自己退了。
    /// 本机客户端一切正常，公网那一头已经没了。
    #[test]
    fn a_tunnel_process_that_died_is_a_failure() {
        let hub = HubConfig {
            tunnel_type: "cloudflare".into(),
            cloudflare_mode: "named".into(),
            public_url: "https://mcp.example.com".into(),
            ..Default::default()
        };

        let check = tunnel_check(
            &hub,
            Some(&snapshot("https://mcp.example.com", "", true, Some(false))),
        );
        assert_eq!(check.level, DoctorLevel::Fail);
        assert!(check.detail.contains("pid 4242"), "{}", check.detail);
        assert!(check.fix.contains("gld restart"), "{}", check.fix);

        let alive = tunnel_check(
            &hub,
            Some(&snapshot("https://mcp.example.com", "", true, Some(true))),
        );
        assert_eq!(alive.level, DoctorLevel::Ok);
        assert!(alive.detail.contains("mcp.example.com"), "{}", alive.detail);
    }

    /// 隧道起不来时服务照样在本地跑，所以得说清"哪半边坏了"。
    #[test]
    fn a_tunnel_that_never_started_says_why() {
        let hub = HubConfig {
            tunnel_type: "frp".into(),
            ..Default::default()
        };
        let check = tunnel_check(&hub, Some(&snapshot("", "frpc 退出码 1", false, None)));
        assert_eq!(check.level, DoctorLevel::Fail);
        assert!(check.detail.contains("frpc 退出码 1"), "{}", check.detail);
        assert!(check.detail.contains("本机客户端"), "{}", check.detail);
    }

    /// 临时地址要点名"会变"：这是唯一一种逼人删了重建连接器的配置。
    #[test]
    fn a_quick_address_is_named_as_temporary() {
        let hub = HubConfig {
            tunnel_type: "cloudflare".into(),
            cloudflare_mode: "quick".into(),
            ..Default::default()
        };
        let check = tunnel_check(
            &hub,
            Some(&snapshot(
                "https://ab-cd.trycloudflare.com",
                "",
                true,
                Some(true),
            )),
        );
        assert_eq!(check.level, DoctorLevel::Ok);
        assert!(check.detail.contains("每次重启都会变"), "{}", check.detail);
        assert!(check.detail.contains("trycloudflare"), "{}", check.detail);
    }

    /// 自建入口：进程不是 gld 起的，体检不能假装知道它通不通。
    #[test]
    fn a_self_hosted_entry_says_gld_cannot_see_it() {
        let hub = HubConfig {
            tunnel_type: "none".into(),
            public_url: "https://ai.example.top".into(),
            ..Default::default()
        };
        let check = tunnel_check(
            &hub,
            Some(&snapshot("https://ai.example.top", "", false, None)),
        );
        assert_eq!(check.level, DoctorLevel::Ok);
        assert!(check.detail.contains("不是 gld 起的"), "{}", check.detail);
    }

    #[test]
    fn a_stopped_service_has_no_tunnel_to_report() {
        let check = tunnel_check(&HubConfig::default(), None);
        assert_eq!(check.level, DoctorLevel::Ok);
        assert!(check.detail.contains("没在跑"), "{}", check.detail);
    }
}
