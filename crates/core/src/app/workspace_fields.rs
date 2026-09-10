//! `gld workspace set <ws> key=value` 的字段表。
//!
//! 把“哪些字段允许改、怎么解析”集中在一张表里：命令行帮助、文档和
//! 实际写入都从这里取，不会出现三处不一致。

use serde::Serialize;

use crate::error::{AppError, AppResult};
use crate::settings::FrpProfile;
use crate::workspace::WorkspaceProfile;

/// 单个可设置字段的说明。
#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceFieldDoc {
    pub key: &'static str,
    pub value: &'static str,
    pub description: &'static str,
}

/// 校验取值时需要看的全局数据。
///
/// 目前只有 FRP 配置表：`frp-profile` 填的是别处定义的 id，光看工作区自己
/// 判断不了它存不存在。
#[derive(Debug, Clone, Default)]
pub struct FieldContext {
    pub frp_profiles: Vec<FrpProfile>,
}

/// 不写前缀时补哪个。
///
/// 一个工作区里 MCP 是主服务（`gld start` 默认起它、`gld list` 默认展示它），
/// Actions 是可选的第二条线路。所以 `port=30000` 补成 `mcp.port`，改 Actions
/// 才需要写全 `actions.port`——常见的那一半不用打前缀，少一半噪音。
const IMPLIED_PREFIX: &str = "mcp.";

struct Field {
    key: &'static str,
    value: &'static str,
    description: &'static str,
    apply: fn(&mut WorkspaceProfile, &str, &FieldContext) -> AppResult<()>,
}

macro_rules! field {
    // 大多数字段只看取值本身，写两个参数就行；要查全局数据的补第三个。
    ($key:literal, $value:literal, $desc:literal, |$profile:ident, $raw:ident| $body:expr) => {
        field!($key, $value, $desc, |$profile, $raw, _ctx| $body)
    };
    ($key:literal, $value:literal, $desc:literal, |$profile:ident, $raw:ident, $ctx:ident| $body:expr) => {
        Field {
            key: $key,
            value: $value,
            description: $desc,
            apply: |$profile: &mut WorkspaceProfile,
                    $raw: &str,
                    $ctx: &FieldContext|
             -> AppResult<()> { $body },
        }
    };
}

const FIELDS: &[Field] = &[
    field!("name", "文本", "显示名称", |p, v| {
        require_non_empty("name", v)?;
        p.name = v.trim().into();
        Ok(())
    }),
    field!(
        "path",
        "已存在的目录",
        "项目根目录；换目录后服务会重启到新目录（旧目录里的历史档案留在原地）",
        |p, v| {
            p.path = parse_workspace_path(v)?;
            Ok(())
        }
    ),
    field!("mcp.port", "1-65535", "MCP 本地监听端口", |p, v| {
        p.runtime.local_port = parse_port(v)?;
        Ok(())
    }),
    field!(
        "mcp.auth",
        "oauth | bearer | noauth",
        "MCP 认证方式",
        |p, v| {
            p.auth.auth_type = parse_choice(v, &["oauth", "bearer", "noauth"])?;
            Ok(())
        }
    ),
    field!(
        "mcp.oauth-client-id",
        "文本",
        "MCP OAuth 静态 Client ID",
        |p, v| {
            require_non_empty("mcp.oauth-client-id", v)?;
            p.auth.oauth_client_id = v.trim().into();
            Ok(())
        }
    ),
    field!(
        "mcp.shared-secrets",
        "true | false",
        "MCP 使用共享密钥池而非工作区密钥",
        |p, v| {
            p.auth.use_shared_secrets = parse_bool(v)?;
            Ok(())
        }
    ),
    field!(
        "mcp.tool-profile",
        "compact | core | advanced | read-only | compat-readonly-all",
        "暴露给客户端的工具集（compact 为稳定聚合 API；core / advanced 保留兼容旧工具名）",
        |p, v| {
            p.runtime.tool_profile = parse_choice(
                v,
                &[
                    "compact",
                    "core",
                    "advanced",
                    "read-only",
                    "compat-readonly-all",
                ],
            )?;
            Ok(())
        }
    ),
    field!(
        "mcp.permission-mode",
        "trusted | dangerous",
        "工具权限模式；两者的写入边界完全一样（都只能写工作区内），见 docs/concepts.md",
        |p, v| {
            p.runtime.permission_mode = parse_choice(v, &["trusted", "dangerous"])?;
            Ok(())
        }
    ),
    field!(
        "mcp.history-recording",
        "true | false",
        "是否允许把会话检查点写入 docs/history-session",
        |p, v| {
            p.runtime.history_recording = parse_bool(v)?;
            Ok(())
        }
    ),
    field!(
        "mcp.history-context",
        "逗号分隔的编号，或空",
        "新会话注入哪些历史档案（有界快照）",
        |p, v| {
            p.runtime.history_context_sessions = parse_u64_list(v)?;
            Ok(())
        }
    ),
    field!(
        "mcp.allowed-commands",
        "逗号分隔",
        "在默认白名单之外追加的命令；写成 only:cargo,git 则表示只允许这些",
        |p, v| {
            p.runtime.allowed_commands = v.trim().into();
            Ok(())
        }
    ),
    field!(
        "mcp.confine-reads",
        "true | false",
        "读工具只许读 Workspace 内（默认 true；关掉才能读隔壁仓库等外部路径）",
        |p, v| {
            p.runtime.confine_reads = parse_bool(v)?;
            Ok(())
        }
    ),
    field!(
        "mcp.executable-paths",
        "路径列表（换行或分号分隔）",
        "额外的可执行文件搜索路径",
        |p, v| {
            p.runtime.executable_paths = v.trim().into();
            Ok(())
        }
    ),
    field!(
        "mcp.ai-instructions",
        "文本",
        "注入 Agent 的工作区级说明",
        |p, v| {
            p.runtime.ai_instructions = v.into();
            Ok(())
        }
    ),
    field!(
        "mcp.tunnel",
        "frp | cf | none",
        "MCP 公网隧道类型（cf 即 cloudflare，两种写法都收）",
        |p, v| {
            p.tunnel.tunnel_type = parse_tunnel_type(v)?;
            Ok(())
        }
    ),
    field!(
        "mcp.frp-profile",
        "FRP 配置的名称或 id，或空",
        "使用哪个 FRP 服务器配置（见 gld frp list）",
        |p, v, ctx| {
            p.tunnel.frp_profile_id = resolve_frp_profile(v, &ctx.frp_profiles)?;
            Ok(())
        }
    ),
    field!(
        "mcp.frp-subdomain",
        "子域名（小写字母 / 数字 / 连字符）",
        "FRP 子域名，公网地址为 https://<子域名>.<服务器>",
        |p, v| {
            p.tunnel.frp_subdomain = parse_subdomain("mcp.frp-subdomain", v)?;
            Ok(())
        }
    ),
    field!(
        "mcp.cloudflare-mode",
        "quick | named",
        "Cloudflare 隧道模式",
        |p, v| {
            p.tunnel.cloudflare_mode = parse_choice(v, &["quick", "named"])?;
            Ok(())
        }
    ),
    field!(
        "mcp.public-url",
        "https:// 开头的 URL，或空",
        "手动指定公网地址（隧道类型 none 时使用）",
        |p, v| {
            p.tunnel.public_url = parse_public_url("mcp.public-url", v)?;
            Ok(())
        }
    ),
    field!(
        "mcp.use-proxy",
        "true | false",
        "启动隧道时是否套用全局代理",
        |p, v| {
            p.tunnel.use_proxy = parse_bool(v)?;
            Ok(())
        }
    ),
    field!(
        "mcp.global-gateway",
        "true | false",
        "通过全局共享入口 /w/<id> 暴露而不是独立隧道",
        |p, v| {
            p.tunnel.use_global_gateway = parse_bool(v)?;
            Ok(())
        }
    ),
    field!(
        "actions.port",
        "1-65535",
        "Actions 本地监听端口",
        |p, v| {
            p.actions.local_port = parse_port(v)?;
            Ok(())
        }
    ),
    field!(
        "actions.auth",
        "api_key | oauth | none",
        "Actions 认证方式",
        |p, v| {
            p.actions.auth_type = parse_choice(v, &["api_key", "oauth", "none"])?;
            Ok(())
        }
    ),
    field!(
        "actions.oauth-client-id",
        "文本",
        "Actions OAuth Client ID",
        |p, v| {
            require_non_empty("actions.oauth-client-id", v)?;
            p.actions.oauth_client_id = v.trim().into();
            Ok(())
        }
    ),
    field!(
        "actions.shared-secrets",
        "true | false",
        "Actions 使用共享密钥池",
        |p, v| {
            p.actions.use_shared_secrets = parse_bool(v)?;
            Ok(())
        }
    ),
    field!(
        "actions.confine-reads",
        "true | false",
        "Actions 侧同上（默认 true）",
        |p, v| {
            p.actions.confine_reads = parse_bool(v)?;
            Ok(())
        }
    ),
    field!(
        "actions.allowed-commands",
        "逗号分隔",
        "Actions 侧同上（追加；only: 前缀表示只允许这些）",
        |p, v| {
            p.actions.allowed_commands = v.trim().into();
            Ok(())
        }
    ),
    field!(
        "actions.tunnel",
        "frp | cf | none",
        "Actions 公网隧道类型（cf 即 cloudflare）",
        |p, v| {
            p.actions.tunnel_type = parse_tunnel_type(v)?;
            Ok(())
        }
    ),
    field!(
        "actions.frp-profile",
        "FRP 配置的名称或 id，或空",
        "Actions 使用的 FRP 服务器配置",
        |p, v, ctx| {
            p.actions.frp_profile_id = resolve_frp_profile(v, &ctx.frp_profiles)?;
            Ok(())
        }
    ),
    field!(
        "actions.frp-subdomain",
        "子域名（小写字母 / 数字 / 连字符）",
        "Actions FRP 子域名",
        |p, v| {
            p.actions.frp_subdomain = parse_subdomain("actions.frp-subdomain", v)?;
            Ok(())
        }
    ),
    field!(
        "actions.cloudflare-mode",
        "quick | named",
        "Actions Cloudflare 隧道模式",
        |p, v| {
            p.actions.cloudflare_mode = parse_choice(v, &["quick", "named"])?;
            Ok(())
        }
    ),
    field!(
        "actions.public-url",
        "https:// 开头的 URL，或空",
        "Actions 手动公网地址",
        |p, v| {
            p.actions.public_url = parse_public_url("actions.public-url", v)?;
            Ok(())
        }
    ),
    field!(
        "actions.use-proxy",
        "true | false",
        "Actions 隧道是否套用全局代理",
        |p, v| {
            p.actions.use_proxy = parse_bool(v)?;
            Ok(())
        }
    ),
    field!(
        "actions.global-gateway",
        "true | false",
        "Actions 通过全局共享入口暴露",
        |p, v| {
            p.actions.use_global_gateway = parse_bool(v)?;
            Ok(())
        }
    ),
];

/// 所有可设置字段及说明，按定义顺序。
pub fn workspace_field_catalog() -> Vec<WorkspaceFieldDoc> {
    FIELDS
        .iter()
        .map(|field| WorkspaceFieldDoc {
            key: field.key,
            value: field.value,
            description: field.description,
        })
        .collect()
}

/// 只列出 Actions 侧字段（`actions.` 前缀）的键名，不含前缀。
///
/// `gld ws fields` 默认不把这 13 行铺开：它们和 MCP 侧同名同义，
/// 列出来只是把表撑长一倍。
pub fn actions_field_suffixes() -> Vec<&'static str> {
    FIELDS
        .iter()
        .filter_map(|field| field.key.strip_prefix("actions."))
        .collect()
}

/// 把用户敲的 key 规整成字段表里的键名。
///
/// 两处宽容：大小写不敏感；`_` 当成 `-`（`tool_profile` 和 `tool-profile` 都收）。
/// 不带 `.` 又不是 `name` 这种全局字段时，补上 [`IMPLIED_PREFIX`]。
fn canonical_key(raw: &str) -> Option<&'static Field> {
    let key = raw.trim().to_ascii_lowercase().replace('_', "-");
    if let Some(field) = FIELDS.iter().find(|field| field.key == key) {
        return Some(field);
    }
    if key.contains('.') {
        return None;
    }
    let implied = format!("{IMPLIED_PREFIX}{key}");
    FIELDS.iter().find(|field| field.key == implied)
}

/// 对一个工作区配置应用 `key=value`；未知 key 返回带全部可选项的错误。
pub fn apply_workspace_field(
    profile: &mut WorkspaceProfile,
    key: &str,
    value: &str,
    ctx: &FieldContext,
) -> AppResult<()> {
    let Some(field) = canonical_key(key) else {
        return Err(unknown_field(key.trim()));
    };
    (field.apply)(profile, value, ctx)
}

fn unknown_field(key: &str) -> AppError {
    // 只列 MCP 侧和全局字段，并且按"能直接敲进去"的形式列（省掉 mcp.）：
    // Actions 侧同名，铺开会让这条报错长到看不完。
    let known = FIELDS
        .iter()
        .map(|field| field.key)
        .filter(|key| !key.starts_with("actions."))
        .map(|key| key.strip_prefix(IMPLIED_PREFIX).unwrap_or(key))
        .collect::<Vec<_>>()
        .join(", ");
    AppError::Message(format!(
        "未知字段「{key}」。可用字段（{IMPLIED_PREFIX} 前缀可省略）：{known}\n\
         Actions 侧同名字段写成 actions.<字段>；完整列表 gld workspace fields --all。"
    ))
}

fn require_non_empty(key: &str, value: &str) -> AppResult<()> {
    if value.trim().is_empty() {
        return Err(AppError::Message(format!("{key} 不能为空")));
    }
    Ok(())
}

fn parse_port(value: &str) -> AppResult<u16> {
    value
        .trim()
        .parse::<u16>()
        .ok()
        .filter(|port| *port > 0)
        .ok_or_else(|| AppError::Message(format!("端口无效：{value}（需要 1-65535）")))
}

fn parse_bool(value: &str) -> AppResult<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" | "1" => Ok(true),
        "false" | "no" | "off" | "0" => Ok(false),
        other => Err(AppError::Message(format!(
            "布尔值无效：{other}（接受 true/false/yes/no/on/off/1/0）"
        ))),
    }
}

/// 隧道类型：命令行的 `--tunnel` 收 cf / off，这里收同样的词。
///
/// 同一个概念以前有两套写法——命令行写 `--tunnel cf`，字段写
/// `tunnel=cloudflare`，抄错一处就是"取值无效"。存下去的仍是规范值，
/// 配置文件和别处的匹配逻辑不受影响。
fn parse_tunnel_type(value: &str) -> AppResult<String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "cf" | "cloudflare" => Ok("cloudflare".into()),
        "off" | "none" => Ok("none".into()),
        "frp" => Ok("frp".into()),
        other => Err(AppError::Message(format!(
            "隧道类型无效：{other}（可选：frp | cf | none；cloudflare / off 也收）"
        ))),
    }
}

fn parse_choice(value: &str, choices: &[&str]) -> AppResult<String> {
    let normalized = value.trim().to_ascii_lowercase();
    if choices.contains(&normalized.as_str()) {
        Ok(normalized)
    } else {
        Err(AppError::Message(format!(
            "取值无效：{value}（可选：{}）",
            choices.join(" | ")
        )))
    }
}

/// FRP 子域名会被拼进 `https://<子域名>.<服务器>`，所以只能是主机名里合法的字符。
///
/// 不拦的话，`frp-subdomain=my proj` 或 `my.proj` 会被原样写进 frpc 配置，
/// 表现成隧道起来了但公网地址 404 / 证书不匹配，很难联想到是这里填错。
fn parse_subdomain(key: &str, value: &str) -> AppResult<String> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(String::new());
    }
    let shaped = value
        .chars()
        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
        && !value.starts_with('-')
        && !value.ends_with('-');
    if !shaped {
        return Err(AppError::Message(format!(
            "{key} 无效：{value}\n\
             子域名会拼成 https://<子域名>.<frps 域名>，只能用小写字母、数字和中间的连字符\
             （不能有点、空格、大写）。"
        )));
    }
    Ok(value.to_string())
}

/// 项目根目录：展开 `~`、转成绝对路径，并确认它真的存在。
///
/// 不校验的话，路径写错（少一层目录、拼错字母）会被原样存下来，表现是服务照常
/// 起来、Agent 却看到一个空项目——错在配置里，日志里一行异常都没有。
fn parse_workspace_path(value: &str) -> AppResult<String> {
    let raw = value.trim();
    if raw.is_empty() {
        return Err(AppError::Message(
            "path 不能为空（要换目录就给一个已存在的目录）".into(),
        ));
    }
    let expanded = match raw.strip_prefix("~/") {
        // shell 只在少数位置展开 `~`，`gld ws set path=~/code/x` 里它常常原样传进来。
        Some(rest) => dirs::home_dir()
            .ok_or_else(|| AppError::Message("无法确定用户主目录，请写绝对路径".into()))?
            .join(rest),
        None => std::path::PathBuf::from(raw),
    };
    let canonical = expanded.canonicalize().map_err(|error| {
        AppError::Message(format!(
            "目录不存在或无法访问：{}（{error}）",
            expanded.display()
        ))
    })?;
    if !canonical.is_dir() {
        return Err(AppError::Message(format!(
            "不是目录：{}",
            canonical.display()
        )));
    }
    Ok(canonical.to_string_lossy().into_owned())
}

/// 手动公网地址必须是个 URL；写成 `example.com` 客户端连不上，且没有任何提示。
fn parse_public_url(key: &str, value: &str) -> AppResult<String> {
    let value = value.trim().trim_end_matches('/');
    if value.is_empty() {
        return Ok(String::new());
    }
    if !(value.starts_with("https://") || value.starts_with("http://")) {
        return Err(AppError::Message(format!(
            "{key} 无效：{value}（要带协议头，例如 https://mcp.example.com）\n\
             ChatGPT 连接器只接受 https；http 只在本机 / 内网客户端上可用。"
        )));
    }
    Ok(value.to_string())
}

/// 把用户填的 FRP 配置名称 / id / id 前缀换成真实 id，找不到就报错。
///
/// 以前这里是 `v.trim().into()`：填错名字会被静默接受，`gld start` 之后
/// 没有公网地址，得跑 `gld doctor` 才知道是这个字段的问题。而且用户在
/// `gld frp add --name 公司` 里给的是名字，来这里却只能填 id，两边对不上。
///
/// 全局入口的 `gld gateway set --frp-profile` 也走这里——两处都是"用户给一个
/// FRP 配置的称呼"，认的写法必须一样，否则同一个名字在工作区能用、在网关报错。
pub(super) fn resolve_frp_profile(raw: &str, profiles: &[FrpProfile]) -> AppResult<String> {
    let value = raw.trim();
    if value.is_empty() {
        return Ok(String::new());
    }
    if let Some(found) = profiles.iter().find(|item| item.id == value) {
        return Ok(found.id.clone());
    }

    let by_name: Vec<&FrpProfile> = profiles
        .iter()
        .filter(|item| item.name.eq_ignore_ascii_case(value))
        .collect();
    match by_name.as_slice() {
        [only] => return Ok(only.id.clone()),
        [] => {}
        many => return Err(ambiguous_frp_profile(value, many)),
    }

    // 和工作区 selector 同一个口径：id 前缀至少 4 位才允许简写。
    if value.len() >= 4 {
        let by_prefix: Vec<&FrpProfile> = profiles
            .iter()
            .filter(|item| item.id.starts_with(value))
            .collect();
        match by_prefix.as_slice() {
            [only] => return Ok(only.id.clone()),
            [] => {}
            many => return Err(ambiguous_frp_profile(value, many)),
        }
    }

    Err(unknown_frp_profile(value, profiles))
}

fn unknown_frp_profile(value: &str, profiles: &[FrpProfile]) -> AppError {
    if profiles.is_empty() {
        return AppError::Message(format!(
            "没有名为「{value}」的 FRP 配置——一个都还没建。先执行：\n  \
             gld frp add --name {value} --server <frps 域名> --port 7000 --token <frps token>"
        ));
    }
    AppError::Message(format!(
        "没有名为「{value}」的 FRP 配置。已有：\n{}\n\
         填上面任意一行的名称或 id 即可；要新建：\n  \
         gld frp add --name {value} --server <frps 域名> --port 7000 --token <frps token>",
        listing(profiles.iter())
    ))
}

fn ambiguous_frp_profile(value: &str, matches: &[&FrpProfile]) -> AppError {
    AppError::Message(format!(
        "「{value}」匹配到多个 FRP 配置，请改用完整 id：\n{}",
        listing(matches.iter().copied())
    ))
}

fn listing<'a>(profiles: impl Iterator<Item = &'a FrpProfile>) -> String {
    profiles
        .map(|item| {
            format!(
                "  {}  {}  {}:{}",
                item.id, item.name, item.server, item.server_port
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn parse_u64_list(value: &str) -> AppResult<Vec<u64>> {
    let mut items = Vec::new();
    for part in value
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
    {
        let number = part
            .parse::<u64>()
            .map_err(|_| AppError::Message(format!("历史编号无效：{part}")))?;
        if !items.contains(&number) {
            items.push(number);
        }
    }
    items.sort_unstable();
    Ok(items)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 绝大多数字段用不到 FieldContext，包一层省得每处都传一张空表。
    fn set(profile: &mut WorkspaceProfile, key: &str, value: &str) -> AppResult<()> {
        apply_workspace_field(profile, key, value, &FieldContext::default())
    }

    fn frp(id: &str, name: &str) -> FrpProfile {
        FrpProfile {
            id: id.into(),
            name: name.into(),
            server: "frp.example.com".into(),
            server_port: 7000,
        }
    }

    #[test]
    fn applies_known_fields_and_rejects_unknown_ones() {
        let mut profile = WorkspaceProfile::new("/tmp/x".into(), None);
        set(&mut profile, "mcp.port", "30000").unwrap();
        set(&mut profile, "mcp.auth", "Bearer").unwrap();
        set(&mut profile, "mcp.history-context", "3, 1,3").unwrap();
        assert_eq!(profile.runtime.local_port, 30000);
        assert_eq!(profile.auth.auth_type, "bearer");
        assert_eq!(profile.runtime.history_context_sessions, vec![1, 3]);

        let error = set(&mut profile, "nope", "1").unwrap_err();
        assert!(error.to_string().contains("未知字段"));
        assert!(set(&mut profile, "mcp.port", "0").is_err());
        assert!(set(&mut profile, "mcp.auth", "magic").is_err());
    }

    /// 不写前缀就是改 MCP。写全的老写法必须继续能用。
    #[test]
    fn a_bare_key_means_the_mcp_side() {
        let mut profile = WorkspaceProfile::new("/tmp/x".into(), None);
        set(&mut profile, "port", "30001").unwrap();
        set(&mut profile, "auth", "bearer").unwrap();
        assert_eq!(profile.runtime.local_port, 30001);
        assert_eq!(profile.auth.auth_type, "bearer");
        // Actions 侧没被顺手改掉——补前缀只补 MCP。
        assert_ne!(profile.actions.local_port, 30001);

        set(&mut profile, "actions.port", "9001").unwrap();
        assert_eq!(profile.actions.local_port, 9001);
        assert_eq!(profile.runtime.local_port, 30001);
    }

    /// `name` 没有前缀，不能被补成 `mcp.name`（那个字段不存在，会变成"未知字段"）。
    #[test]
    fn a_prefixless_global_field_is_not_rewritten() {
        let mut profile = WorkspaceProfile::new("/tmp/x".into(), None);
        set(&mut profile, "name", "api").unwrap();
        assert_eq!(profile.name, "api");
    }

    /// 大小写和下划线都收：抄文档时手滑不该变成报错。
    #[test]
    fn keys_are_case_and_underscore_tolerant() {
        let mut profile = WorkspaceProfile::new("/tmp/x".into(), None);
        set(&mut profile, "TOOL_PROFILE", "advanced").unwrap();
        assert_eq!(profile.runtime.tool_profile, "advanced");
        set(&mut profile, "MCP.Confine-Reads", "false").unwrap();
        assert!(!profile.runtime.confine_reads);
    }

    /// 写错前缀不能被"补前缀"救回来：`mcp.nope` 只能是错的，不能试成 `mcp.mcp.nope`。
    #[test]
    fn a_key_with_a_dot_is_never_re_prefixed() {
        let mut profile = WorkspaceProfile::new("/tmp/x".into(), None);
        let error = set(&mut profile, "mcp.nope", "1").unwrap_err().to_string();
        assert!(error.contains("未知字段"), "{error}");
        // 报错里不该把 33 个字段全铺开，但要告诉用户 Actions 侧怎么写。
        assert!(!error.contains("actions.port"), "{error}");
        assert!(error.contains("actions.<字段>"), "{error}");
    }

    /// frp-profile 认名字，也认 id 和 id 前缀——用户在 `gld frp add --name 公司`
    /// 里给的是名字，这里只能填 id 的话两边对不上。
    #[test]
    fn an_frp_profile_can_be_named_by_name_id_or_prefix() {
        let ctx = FieldContext {
            frp_profiles: vec![frp("abcd1234", "公司"), frp("efgh5678", "家里")],
        };
        let mut profile = WorkspaceProfile::new("/tmp/x".into(), None);

        apply_workspace_field(&mut profile, "frp-profile", "公司", &ctx).unwrap();
        assert_eq!(profile.tunnel.frp_profile_id, "abcd1234");
        apply_workspace_field(&mut profile, "frp-profile", "efgh5678", &ctx).unwrap();
        assert_eq!(profile.tunnel.frp_profile_id, "efgh5678");
        apply_workspace_field(&mut profile, "frp-profile", "abcd", &ctx).unwrap();
        assert_eq!(profile.tunnel.frp_profile_id, "abcd1234");
        // 清空仍然要允许：这是"不用 FRP 了"的表达方式。
        apply_workspace_field(&mut profile, "frp-profile", "", &ctx).unwrap();
        assert!(profile.tunnel.frp_profile_id.is_empty());
    }

    /// 以前填错名字会被静默接受，start 之后没有公网地址才发现。
    #[test]
    fn a_nonexistent_frp_profile_is_rejected_with_the_list() {
        let ctx = FieldContext {
            frp_profiles: vec![frp("abcd1234", "公司")],
        };
        let mut profile = WorkspaceProfile::new("/tmp/x".into(), None);

        let error = apply_workspace_field(&mut profile, "frp-profile", "不存在", &ctx)
            .unwrap_err()
            .to_string();
        assert!(error.contains("没有名为「不存在」"), "{error}");
        // 报错里要能直接看到有哪些可选，不用再去跑 gld frp list。
        assert!(
            error.contains("abcd1234") && error.contains("公司"),
            "{error}"
        );
        assert!(error.contains("gld frp add"), "{error}");
        // 被拒绝了就不能顺手把值写进去。
        assert!(profile.tunnel.frp_profile_id.is_empty());

        // 一个都没建时不该列一张空表，直接给建的命令。
        let empty = FieldContext::default();
        let error = apply_workspace_field(&mut profile, "frp-profile", "公司", &empty)
            .unwrap_err()
            .to_string();
        assert!(error.contains("一个都还没建"), "{error}");
    }

    /// 子域名会拼进 URL，点和空格进去只会表现成"隧道起来了但 404"。
    #[test]
    fn a_malformed_subdomain_is_rejected() {
        let mut profile = WorkspaceProfile::new("/tmp/x".into(), None);
        set(&mut profile, "frp-subdomain", "my-proj").unwrap();
        assert_eq!(profile.tunnel.frp_subdomain, "my-proj");
        set(&mut profile, "frp-subdomain", "").unwrap();
        assert!(profile.tunnel.frp_subdomain.is_empty());

        for bad in ["my.proj", "my proj", "MyProj", "-proj", "proj-"] {
            let error = set(&mut profile, "frp-subdomain", bad)
                .unwrap_err()
                .to_string();
            assert!(error.contains("子域名"), "{bad} → {error}");
        }
    }

    /// 隧道类型三处写法必须互通：命令行 `--tunnel cf`、字段 `tunnel=cloudflare`、
    /// 全局入口 `gateway set --tunnel cf`，抄哪一处过来都得认。
    #[test]
    fn tunnel_type_accepts_both_the_short_and_the_long_spelling() {
        let mut profile = WorkspaceProfile::new("/tmp/x".into(), None);
        for (written, stored) in [
            ("cf", "cloudflare"),
            ("cloudflare", "cloudflare"),
            ("CF", "cloudflare"),
            ("off", "none"),
            ("none", "none"),
            ("frp", "frp"),
        ] {
            set(&mut profile, "tunnel", written).unwrap();
            // 存进配置文件的永远是规范值，别处的匹配逻辑不用跟着改。
            assert_eq!(profile.tunnel.tunnel_type, stored, "{written}");
        }
        let error = set(&mut profile, "tunnel", "quick")
            .unwrap_err()
            .to_string();
        assert!(error.contains("frp | cf | none"), "{error}");
    }

    /// 换项目目录时必须当场确认目录存在，否则服务会起在一个空目录上。
    #[test]
    fn a_workspace_path_must_point_at_a_real_directory() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut profile = WorkspaceProfile::new("/tmp/x".into(), None);

        set(&mut profile, "path", temp.path().to_str().unwrap()).unwrap();
        // 存下来的是规范化后的绝对路径（macOS 上 /var 是 /private/var 的软链）。
        assert_eq!(
            std::path::Path::new(&profile.path).canonicalize().unwrap(),
            temp.path().canonicalize().unwrap()
        );

        let missing = temp.path().join("nope");
        let error = set(&mut profile, "path", missing.to_str().unwrap())
            .unwrap_err()
            .to_string();
        assert!(error.contains("目录不存在"), "{error}");
        assert!(set(&mut profile, "path", "").is_err());
    }

    /// 手动公网地址不带协议头，客户端连不上而且没有任何提示。
    #[test]
    fn a_public_url_must_carry_a_scheme() {
        let mut profile = WorkspaceProfile::new("/tmp/x".into(), None);
        set(&mut profile, "public-url", "https://mcp.example.com/").unwrap();
        assert_eq!(profile.tunnel.public_url, "https://mcp.example.com");
        set(&mut profile, "public-url", "").unwrap();
        assert!(profile.tunnel.public_url.is_empty());

        let error = set(&mut profile, "public-url", "mcp.example.com")
            .unwrap_err()
            .to_string();
        assert!(error.contains("要带协议头"), "{error}");
    }

    /// Actions 侧字段必须每个都能在 MCP 侧找到同名的，否则 `gld ws fields`
    /// 的"同名字段换前缀"那句话就是假的。
    #[test]
    fn every_actions_field_mirrors_an_mcp_field() {
        for suffix in actions_field_suffixes() {
            let mirrored = format!("{IMPLIED_PREFIX}{suffix}");
            assert!(
                FIELDS.iter().any(|field| field.key == mirrored),
                "actions.{suffix} 没有对应的 {mirrored}"
            );
        }
    }

    #[test]
    fn catalog_keys_are_unique() {
        let catalog = workspace_field_catalog();
        let mut keys: Vec<_> = catalog.iter().map(|field| field.key).collect();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), catalog.len());
    }
}
