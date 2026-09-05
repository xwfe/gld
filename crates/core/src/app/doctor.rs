//! 配置体检。
//!
//! 目标是把“连不上 / 起不来”这类模糊问题，变成一条条能照着做的修复命令。
//! 每一项检查都要能回答两个问题：**哪里不对**（detail）、**怎么办**（fix）。
//! 只报现象不给做法的检查不要加。
//!
//! 检查分两类：
//!
//! - **配置一致性**（[`config_checks`]）：只看 profiles + settings + 密钥是否存在，
//!   纯函数，可单测；
//! - **环境**：数据目录权限、端口占用、外部二进制是否就位，依赖操作系统状态。

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::App;
use crate::error::AppResult;
use crate::platform::platform;
use crate::runtime::{is_own_process, ServiceKind};
use crate::settings::AppSettings;
use crate::workspace::WorkspaceProfile;

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
    /// 归属：`环境` 或工作区名称。
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

/// 一个工作区某项密钥是否已设置。
///
/// 抽成 trait object 是为了让 [`config_checks`] 不依赖 `App` 和磁盘，可以单测。
pub type SecretLookup<'a> = &'a dyn Fn(&str, &str, bool) -> bool;

impl App {
    /// 跑一遍全部体检项。
    pub fn doctor(&self) -> AppResult<Diagnosis> {
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

        let mut checks = environment_checks(&settings, &profiles);
        checks.extend(config_checks(&profiles, &settings, &secret_present));
        checks.extend(self.port_checks(&profiles)?);
        Ok(Diagnosis { checks })
    }

    fn port_checks(&self, profiles: &[WorkspaceProfile]) -> AppResult<Vec<DoctorCheck>> {
        let mut checks = Vec::new();
        for profile in profiles {
            for kind in ServiceKind::ALL {
                let port = match kind {
                    ServiceKind::Mcp => profile.runtime.local_port,
                    ServiceKind::Actions => profile.actions.local_port,
                };
                let running =
                    self.with_runtime(|runtime| Ok(runtime.is_running(&profile.id, kind)))?;
                let occupant = platform()
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
                    });
                checks.push(port_check(&profile.name, kind, port, running, occupant));
            }
        }
        Ok(checks)
    }
}

/// 占用端口的进程。
#[derive(Debug, Clone)]
pub struct PortOccupant {
    /// 占用者就是本进程（也就是我们自己的监听器）。
    pub is_self: bool,
    /// 可执行文件路径，取不到时退化成 `pid <n>`。
    pub image: String,
}

/// 端口状态的判定。抽成纯函数是为了让每个分支（连同它的修复命令）都能被测到。
pub fn port_check(
    workspace: &str,
    kind: ServiceKind,
    port: u16,
    running: bool,
    occupant: Option<PortOccupant>,
) -> DoctorCheck {
    let label = format!("{} 端口 {port}", kind.label().trim());
    let field = match kind {
        ServiceKind::Mcp => "mcp",
        ServiceKind::Actions => "actions",
    };
    match (running, occupant) {
        (true, None) => DoctorCheck::fail(
            workspace,
            &label,
            "标记为运行中，但没有进程在监听这个端口",
            format!("gld restart -w {workspace} -s {}", kind.as_str()),
        ),
        (true, Some(_)) => DoctorCheck::ok(workspace, &label, "运行中"),
        (false, Some(occupant)) if !occupant.is_self => DoctorCheck::warn(
            workspace,
            &label,
            format!("未启动，但端口已被占用：{}", occupant.image),
            format!("换端口：gld ws set -w {workspace} {field}.port=<其他端口>"),
        ),
        (false, _) => DoctorCheck::ok(workspace, &label, "空闲"),
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
            "有工作区配置了这种隧道，但 PATH 里找不到可执行文件",
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
    let needs_frpc = profiles.iter().any(|profile| uses(profile, "frp"))
        || (settings.global_gateway.enabled && settings.global_gateway.tunnel_type == "frp");
    let needs_cloudflared = profiles.iter().any(|profile| uses(profile, "cloudflare"))
        || (settings.global_gateway.enabled && settings.global_gateway.tunnel_type == "cloudflare");
    for (kind, needed) in [("frpc", needs_frpc), ("cloudflared", needs_cloudflared)] {
        if !needed {
            continue;
        }
        let found = crate::tunnel::tunnel_binary_path(kind);
        checks.push(software_check(kind, found.as_deref()));
    }

    checks
}

fn uses(profile: &WorkspaceProfile, tunnel_type: &str) -> bool {
    (!profile.tunnel.use_global_gateway && profile.tunnel.tunnel_type == tunnel_type)
        || (!profile.actions.use_global_gateway && profile.actions.tunnel_type == tunnel_type)
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
            "工作区",
            "还没有登记任何工作区",
            "gld workspace add <项目目录>",
        ));
        return checks;
    }

    checks.extend(duplicate_port_checks(profiles));
    checks.extend(duplicate_subdomain_checks(profiles));

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
                format!("目录移动了就重新登记：gld ws rm -w {scope} 后 gld ws add <新路径>"),
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

        checks.extend(auth_checks(profile, settings, secret_present));
        checks.extend(tunnel_checks(profile, settings, secret_present));
    }

    checks
}

fn duplicate_port_checks(profiles: &[WorkspaceProfile]) -> Vec<DoctorCheck> {
    let mut owners: HashMap<u16, Vec<String>> = HashMap::new();
    for profile in profiles {
        owners
            .entry(profile.runtime.local_port)
            .or_default()
            .push(format!("{} MCP", profile.name));
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
            // 冲突的可能是 MCP 也可能是 Actions，修复提示要指向对的字段。
            let field = if users.iter().all(|user| user.ends_with("Actions")) {
                "actions.port"
            } else {
                "mcp.port"
            };
            DoctorCheck::fail(
                "环境",
                &format!("端口 {port} 冲突"),
                format!("被多个服务同时占用：{}", users.join("、")),
                format!("给其中一个换端口：gld ws set -w <工作区> {field}=<其他端口>"),
            )
        })
        .collect()
}

fn duplicate_subdomain_checks(profiles: &[WorkspaceProfile]) -> Vec<DoctorCheck> {
    let mut owners: HashMap<String, Vec<String>> = HashMap::new();
    for profile in profiles {
        for (subdomain, service, gateway, tunnel_type) in [
            (
                &profile.tunnel.frp_subdomain,
                "MCP",
                profile.tunnel.use_global_gateway,
                &profile.tunnel.tunnel_type,
            ),
            (
                &profile.actions.frp_subdomain,
                "Actions",
                profile.actions.use_global_gateway,
                &profile.actions.tunnel_type,
            ),
        ] {
            if gateway || tunnel_type != "frp" || subdomain.trim().is_empty() {
                continue;
            }
            owners
                .entry(subdomain.trim().to_ascii_lowercase())
                .or_default()
                .push(format!("{} {service}", profile.name));
        }
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
                "每个服务用不同子域名：gld ws set -w <工作区> mcp.frp-subdomain=<其他名字>",
            )
        })
        .collect()
}

/// 这个工作区的 MCP 是不是有公网入口；有的话返回一句人话，用于拼报错。
///
/// 三种都算：配了隧道、手填了公网地址、接进了全局共享入口。
fn public_exposure(profile: &WorkspaceProfile) -> Option<String> {
    let tunnel = profile.tunnel.tunnel_type.trim();
    if !tunnel.is_empty() && tunnel != "none" {
        return Some(format!("配了 {tunnel} 隧道"));
    }
    if !profile.tunnel.public_url.trim().is_empty() {
        return Some("填了公网地址".into());
    }
    if profile.tunnel.use_global_gateway {
        return Some("接进了全局共享入口".into());
    }
    None
}

fn auth_checks(
    profile: &WorkspaceProfile,
    settings: &AppSettings,
    secret_present: SecretLookup<'_>,
) -> Vec<DoctorCheck> {
    let scope = profile.name.as_str();
    let mut checks = Vec::new();
    let shared = profile.auth.use_shared_secrets;

    match profile.auth.auth_type.as_str() {
        "oauth" => {
            let missing: Vec<&str> = ["oauth_password", "oauth_token_secret"]
                .into_iter()
                .filter(|key| !secret_present(&profile.id, key, shared))
                .collect();
            checks.push(if missing.is_empty() {
                DoctorCheck::ok(scope, "MCP 认证", "oauth（凭据齐全）")
            } else {
                DoctorCheck::fail(
                    scope,
                    "MCP 认证",
                    format!("oauth 缺少凭据：{}", missing.join("、")),
                    format!(
                        "gld secret {}regen {} -w {scope}",
                        if shared { "shared " } else { "" },
                        missing[0]
                    ),
                )
            });
        }
        "bearer" => {
            checks.push(if secret_present(&profile.id, "bearer_token", shared) {
                DoctorCheck::ok(scope, "MCP 认证", "bearer（Token 已设置）")
            } else {
                DoctorCheck::fail(
                    scope,
                    "MCP 认证",
                    "bearer 但没有 bearer_token",
                    format!(
                        "gld secret {}regen bearer_token -w {scope}",
                        if shared { "shared " } else { "" }
                    ),
                )
            });
        }
        _ => {
            // noauth 本身没问题，前提是**外面真的进不来**。
            //
            // 光看 allow_lan_access 是不够的：隧道（cloudflared / frpc）就是从
            // 127.0.0.1 把端口转到公网的，绑回环地址对它一点约束都没有。
            // 以前这种组合只报一句"noauth（仅监听 127.0.0.1）· 本机自用可以"，
            // 而实际状态是"全互联网都能无认证读写这个项目并执行命令"。
            checks.push(if let Some(reason) = public_exposure(profile) {
                DoctorCheck::fail(
                    scope,
                    "MCP 认证",
                    format!("noauth 但{reason}：任何人都能读写这个项目并执行命令"),
                    format!("gld ws set -w {scope} auth=bearer；或 gld share --off -w {scope} 收回公网入口"),
                )
            } else if settings.allow_lan_access {
                DoctorCheck::fail(
                    scope,
                    "MCP 认证",
                    "noauth 且已开启局域网访问：同网段任何人都能读写这个项目并执行命令",
                    format!("改认证：gld ws set -w {scope} mcp.auth=bearer；或关闭 gld settings runtime --lan-access false"),
                )
            } else {
                DoctorCheck::warn(
                    scope,
                    "MCP 认证",
                    "noauth（仅监听 127.0.0.1，且没有公网入口）",
                    "本机自用可以；开局域网访问或开隧道之前必须换成 bearer 或 oauth",
                )
            });
        }
    }

    let actions_shared = profile.actions.use_shared_secrets;
    let actions_missing: Vec<&str> = match profile.actions.auth_type.as_str() {
        "api_key" => ["actions_api_key"]
            .into_iter()
            .filter(|key| !secret_present(&profile.id, key, actions_shared))
            .collect(),
        "oauth" => ["actions_oauth_password", "actions_oauth_token_secret"]
            .into_iter()
            .filter(|key| !secret_present(&profile.id, key, actions_shared))
            .collect(),
        _ => Vec::new(),
    };
    if !actions_missing.is_empty() {
        checks.push(DoctorCheck::fail(
            scope,
            "Actions 认证",
            format!(
                "{} 缺少凭据：{}",
                profile.actions.auth_type,
                actions_missing.join("、")
            ),
            format!(
                "gld secret {}regen {} -w {scope}（Actions 未启用时可忽略）",
                if actions_shared { "shared " } else { "" },
                actions_missing[0]
            ),
        ));
    }

    checks
}

fn tunnel_checks(
    profile: &WorkspaceProfile,
    settings: &AppSettings,
    secret_present: SecretLookup<'_>,
) -> Vec<DoctorCheck> {
    let scope = profile.name.as_str();
    let mut checks = Vec::new();

    // 一个工作区只跑一个 frpc 进程，两条线路必须连同一台 frps。
    let both_frp = !profile.tunnel.use_global_gateway
        && !profile.actions.use_global_gateway
        && profile.tunnel.tunnel_type == "frp"
        && profile.actions.tunnel_type == "frp";
    if both_frp && profile.tunnel.frp_profile_id != profile.actions.frp_profile_id {
        checks.push(DoctorCheck::fail(
            scope,
            "FRP 服务器一致性",
            "MCP 与 Actions 指向不同的 FRP 配置，但一个工作区只会启动一个 frpc",
            format!(
                "统一：gld ws set -w {scope} actions.frp-profile={}",
                profile.tunnel.frp_profile_id
            ),
        ));
    }

    for (service, tunnel_type, profile_id, subdomain, gateway, cloudflare_mode, token_key) in [
        (
            "MCP",
            &profile.tunnel.tunnel_type,
            &profile.tunnel.frp_profile_id,
            &profile.tunnel.frp_subdomain,
            profile.tunnel.use_global_gateway,
            &profile.tunnel.cloudflare_mode,
            "cloudflare_token",
        ),
        (
            "Actions",
            &profile.actions.tunnel_type,
            &profile.actions.frp_profile_id,
            &profile.actions.frp_subdomain,
            profile.actions.use_global_gateway,
            &profile.actions.cloudflare_mode,
            "actions_cloudflare_token",
        ),
    ] {
        let label = format!("{service} 公网入口");
        // 修复命令要能直接敲：MCP 侧字段名省前缀，Actions 侧写全并带上 -s actions。
        let field_prefix = if service == "Actions" { "actions." } else { "" };
        let expose_service = if service == "Actions" {
            " -s actions"
        } else {
            ""
        };
        if gateway {
            checks.push(if settings.global_gateway.enabled {
                DoctorCheck::ok(scope, &label, "走全局共享入口")
            } else {
                DoctorCheck::fail(
                    scope,
                    &label,
                    "配置为走全局入口，但全局入口未启用",
                    "gld gateway set --enabled true",
                )
            });
            continue;
        }

        match tunnel_type.as_str() {
            "frp" => {
                let known_profile = settings.find_frp_profile(profile_id);
                if !profile_id.trim().is_empty() && known_profile.is_none() {
                    checks.push(DoctorCheck::fail(
                        scope,
                        &label,
                        format!("引用的 FRP 配置 {profile_id} 不存在（多半是被 gld frp remove --force 删掉了）"),
                        format!(
                            "gld frp list 看现有的，再 gld ws set -w {scope} {field_prefix}frp-profile=<名称>；\
                             不要公网就 gld share --off -w {scope}{expose_service}"
                        ),
                    ));
                } else if known_profile.is_none() {
                    checks.push(DoctorCheck::fail(
                        scope,
                        &label,
                        "隧道类型是 frp，但没有选择 FRP 配置",
                        "gld frp add --name <名称> --server <frps地址> --token <token> 后再 gld ws set",
                    ));
                } else if subdomain.trim().is_empty() {
                    checks.push(DoctorCheck::fail(
                        scope,
                        &label,
                        "选了 FRP 配置但没有填子域名，公网地址无法生成",
                        format!(
                            "gld ws set -w {scope} {}.frp-subdomain=<子域名>",
                            service.to_ascii_lowercase()
                        ),
                    ));
                } else {
                    checks.push(DoctorCheck::ok(
                        scope,
                        &label,
                        format!(
                            "frp https://{}.{}",
                            subdomain.trim(),
                            known_profile.map(|item| item.server.as_str()).unwrap_or("")
                        ),
                    ));
                }
            }
            "cloudflare" => {
                if cloudflare_mode == "named" && !secret_present(&profile.id, token_key, false) {
                    checks.push(DoctorCheck::fail(
                        scope,
                        &label,
                        "Cloudflare named 模式缺少 tunnel token",
                        format!("gld secret set {token_key} <token> -w {scope}"),
                    ));
                } else {
                    checks.push(DoctorCheck::ok(
                        scope,
                        &label,
                        format!("cloudflare（{cloudflare_mode}）"),
                    ));
                }
            }
            _ => {
                let public = if service == "MCP" {
                    &profile.tunnel.public_url
                } else {
                    &profile.actions.public_url
                };
                if !public.trim().is_empty() {
                    checks.push(DoctorCheck::ok(scope, &label, public.clone()));
                    continue;
                }
                // Actions 保持出厂状态（无隧道、无公网地址）就是「没在用」，
                // 不该给只用 MCP 的人重复报一条一模一样的提醒。
                if service == "Actions" {
                    continue;
                }
                checks.push(DoctorCheck::warn(
                    scope,
                    &label,
                    "没有公网入口，只能本机访问",
                    "本机客户端（Claude Code / Cursor）够用；ChatGPT 需要公网地址，见 docs/connect-clients.md",
                ));
            }
        }
    }

    checks
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

    fn all_present(_: &str, _: &str, _: bool) -> bool {
        true
    }

    fn find<'a>(checks: &'a [DoctorCheck], label: &str) -> Option<&'a DoctorCheck> {
        checks.iter().find(|check| check.label.contains(label))
    }

    #[test]
    fn healthy_workspace_reports_no_failures() {
        let temp = tempfile::tempdir().expect("workspace");
        std::fs::create_dir_all(temp.path().join(".git")).unwrap();
        let checks = config_checks(
            &[profile("solo", temp.path().to_str().unwrap())],
            &AppSettings::default(),
            &all_present,
        );
        assert!(
            !checks.iter().any(|check| check.level == DoctorLevel::Fail),
            "{checks:#?}"
        );
    }

    #[test]
    fn missing_bearer_token_is_a_failure_with_a_command_to_run() {
        let temp = tempfile::tempdir().expect("workspace");
        let checks = config_checks(
            &[profile("api", temp.path().to_str().unwrap())],
            &AppSettings::default(),
            &|_, _, _| false,
        );
        let auth = find(&checks, "MCP 认证").expect("auth check");
        assert_eq!(auth.level, DoctorLevel::Fail);
        assert!(auth.fix.contains("gld secret"), "{}", auth.fix);
    }

    #[test]
    fn noauth_plus_lan_access_is_a_failure() {
        let temp = tempfile::tempdir().expect("workspace");
        let mut item = profile("open", temp.path().to_str().unwrap());
        item.auth.auth_type = "noauth".into();
        let settings = AppSettings {
            allow_lan_access: true,
            ..AppSettings::default()
        };

        let checks = config_checks(&[item.clone()], &settings, &all_present);
        assert_eq!(find(&checks, "MCP 认证").unwrap().level, DoctorLevel::Fail);

        // 只监听本机、又没有公网入口时降为提醒。
        let checks = config_checks(&[item], &AppSettings::default(), &all_present);
        assert_eq!(find(&checks, "MCP 认证").unwrap().level, DoctorLevel::Warn);
    }

    /// noauth + 任何一种公网入口都是 Fail，哪怕只监听 127.0.0.1。
    ///
    /// 隧道就是从 127.0.0.1 把端口转到公网的，绑回环地址对它没有任何约束。
    /// 以前这种组合只报"noauth（仅监听 127.0.0.1）· 本机自用可以"，
    /// 而实际状态是全互联网都能无认证执行命令。
    #[test]
    fn noauth_plus_a_public_entrance_is_a_failure_even_on_loopback() {
        let temp = tempfile::tempdir().expect("workspace");
        let base = |kind: &str| {
            let mut item = profile("exposed", temp.path().to_str().unwrap());
            item.auth.auth_type = "noauth".into();
            match kind {
                "tunnel" => item.tunnel.tunnel_type = "cloudflare".into(),
                "public_url" => item.tunnel.public_url = "https://mcp.example.com".into(),
                _ => item.tunnel.use_global_gateway = true,
            }
            item
        };

        for kind in ["tunnel", "public_url", "gateway"] {
            // 注意 settings 里 lan-access 是关的：要证的就是"绑回环也不安全"。
            let checks = config_checks(&[base(kind)], &AppSettings::default(), &all_present);
            let auth = find(&checks, "MCP 认证").expect("auth check");
            assert_eq!(auth.level, DoctorLevel::Fail, "{kind}: {auth:#?}");
            assert!(auth.fix.contains("auth=bearer"), "{}", auth.fix);
        }
    }

    #[test]
    fn duplicate_ports_and_subdomains_are_reported_once_each() {
        let temp = tempfile::tempdir().expect("workspace");
        let path = temp.path().to_str().unwrap();
        let mut first = profile("a", path);
        let mut second = profile("b", path);
        // 只让 MCP 端口撞车，Actions 各用各的，便于断言“一个端口一条”。
        second.runtime.local_port = first.runtime.local_port;
        second.actions.local_port = first.actions.local_port + 1;
        for item in [&mut first, &mut second] {
            item.tunnel.tunnel_type = "frp".into();
            item.tunnel.frp_subdomain = "Same".into();
            item.tunnel.frp_profile_id = "p1".into();
        }
        let mut settings = AppSettings::default();
        settings.frp_profiles.push(crate::settings::FrpProfile {
            id: "p1".into(),
            name: "p".into(),
            server: "frp.example.com".into(),
            server_port: 7000,
        });

        let checks = config_checks(&[first, second], &settings, &all_present);
        let ports: Vec<_> = checks
            .iter()
            .filter(|check| check.label.contains("端口") && check.label.contains("冲突"))
            .collect();
        assert_eq!(ports.len(), 1, "{checks:#?}");
        assert_eq!(ports[0].level, DoctorLevel::Fail);
        assert!(ports[0].fix.contains("mcp.port"), "{}", ports[0].fix);
        // 子域名大小写不同也算冲突：frps 侧不区分大小写。
        let subdomains: Vec<_> = checks
            .iter()
            .filter(|check| check.label.contains("子域名"))
            .collect();
        assert_eq!(subdomains.len(), 1, "{checks:#?}");
    }

    #[test]
    fn an_unused_actions_service_does_not_repeat_the_no_public_entry_warning() {
        let temp = tempfile::tempdir().expect("workspace");
        let checks = config_checks(
            &[profile("local-only", temp.path().to_str().unwrap())],
            &AppSettings::default(),
            &all_present,
        );
        let entries: Vec<_> = checks
            .iter()
            .filter(|check| check.label.contains("公网入口"))
            .collect();
        assert_eq!(entries.len(), 1, "{entries:#?}");
        assert!(entries[0].label.starts_with("MCP"));

        // 但 Actions 一旦真的配了隧道，就照常检查。
        let mut configured = profile("with-actions", temp.path().to_str().unwrap());
        configured.actions.tunnel_type = "frp".into();
        let checks = config_checks(&[configured], &AppSettings::default(), &all_present);
        assert!(checks
            .iter()
            .any(|check| check.label == "Actions 公网入口" && check.level == DoctorLevel::Fail));
    }

    #[test]
    fn an_actions_only_port_clash_points_at_the_actions_field() {
        let temp = tempfile::tempdir().expect("workspace");
        let path = temp.path().to_str().unwrap();
        let mut first = profile("a", path);
        let mut second = profile("b", path);
        second.runtime.local_port = first.runtime.local_port + 1;
        first.actions.local_port = 9000;
        second.actions.local_port = 9000;

        let checks = config_checks(&[first, second], &AppSettings::default(), &all_present);
        let clash = checks
            .iter()
            .find(|check| check.label.contains("端口 9000"))
            .expect("actions clash");
        assert!(clash.fix.contains("actions.port"), "{}", clash.fix);
    }

    #[test]
    fn frp_without_a_profile_or_subdomain_fails_with_the_next_command() {
        let temp = tempfile::tempdir().expect("workspace");
        let mut item = profile("web", temp.path().to_str().unwrap());
        item.tunnel.tunnel_type = "frp".into();

        let checks = config_checks(&[item.clone()], &AppSettings::default(), &all_present);
        let entry = find(&checks, "MCP 公网入口").expect("tunnel check");
        assert_eq!(entry.level, DoctorLevel::Fail);
        assert!(entry.fix.contains("gld frp add"), "{}", entry.fix);

        // 有配置但没子域名：换一条提示。
        let mut settings = AppSettings::default();
        settings.frp_profiles.push(crate::settings::FrpProfile {
            id: "p1".into(),
            name: "p".into(),
            server: "frp.example.com".into(),
            server_port: 7000,
        });
        item.tunnel.frp_profile_id = "p1".into();
        let checks = config_checks(&[item], &settings, &all_present);
        let entry = find(&checks, "MCP 公网入口").expect("tunnel check");
        assert_eq!(entry.level, DoctorLevel::Fail);
        assert!(entry.fix.contains("frp-subdomain"), "{}", entry.fix);
    }

    #[test]
    fn global_gateway_reference_requires_the_gateway_to_be_enabled() {
        let temp = tempfile::tempdir().expect("workspace");
        let mut item = profile("hub", temp.path().to_str().unwrap());
        item.tunnel.use_global_gateway = true;

        let checks = config_checks(&[item.clone()], &AppSettings::default(), &all_present);
        assert_eq!(
            find(&checks, "MCP 公网入口").unwrap().level,
            DoctorLevel::Fail
        );

        let mut settings = AppSettings::default();
        settings.global_gateway.enabled = true;
        let checks = config_checks(&[item], &settings, &all_present);
        assert_eq!(
            find(&checks, "MCP 公网入口").unwrap().level,
            DoctorLevel::Ok
        );
    }

    #[test]
    fn split_frp_profiles_in_one_workspace_fail() {
        let temp = tempfile::tempdir().expect("workspace");
        let mut item = profile("split", temp.path().to_str().unwrap());
        item.tunnel.tunnel_type = "frp".into();
        item.actions.tunnel_type = "frp".into();
        item.tunnel.frp_profile_id = "p1".into();
        item.actions.frp_profile_id = "p2".into();

        let checks = config_checks(&[item], &AppSettings::default(), &all_present);
        let entry = find(&checks, "FRP 服务器一致性").expect("consistency check");
        assert_eq!(entry.level, DoctorLevel::Fail);
    }

    #[test]
    fn a_missing_project_directory_fails() {
        let checks = config_checks(
            &[profile("gone", "/definitely/not/here/gld")],
            &AppSettings::default(),
            &all_present,
        );
        let entry = find(&checks, "项目目录").expect("directory check");
        assert_eq!(entry.level, DoctorLevel::Fail);
    }
}
