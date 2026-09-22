//! `gld secret`：不带 `-w` 是服务的凭据（RFC-0004 之后只有一个服务、一套凭据）；
//! 带了 `-w` 是那个项目自己的——现在只有它的 GPT Actions 还用得上。

use gld_core::app::{reads_from_shared_pool, SHARED_SECRET_KEYS, WORKSPACE_SECRET_KEYS};
use gld_core::workspace::WorkspaceProfile;
use gld_daemon::Request;
use serde_json::json;

use super::Ctx;
use crate::cli::{SecretCmd, SharedSecretCmd};
use crate::error::CliResult;
use crate::output::mask;

/// 服务的凭据名和用途。顺序就是 `gld secret ls` 列出来的顺序。
const SERVICE_KEYS: &[(&str, &str)] = &[
    ("oauth_password", "OAuth 授权页输入的口令"),
    (
        "oauth_client_id",
        "OAuth 静态 Client ID（ChatGPT 这类自动注册的客户端用不到）",
    ),
    (
        "oauth_token_secret",
        "签发访问令牌的密钥；换了所有已授权的客户端都要重新授权",
    ),
    ("bearer_token", "认证方式为 bearer 时客户端携带的 Token"),
    (
        "cloudflare_token",
        "Cloudflare 固定域名的 Tunnel Token（只能 set，不能 regen）",
    ),
];

const KEY_DOCS: &[(&str, &str)] = &[
    ("actions_api_key", "Actions 认证方式为 api_key 时的 Key"),
    ("actions_oauth_client_secret", "Actions OAuth Client Secret"),
    ("actions_oauth_password", "Actions OAuth 授权口令"),
    ("actions_oauth_token_secret", "签发 Actions Token 的密钥"),
    (
        "actions_cloudflare_token",
        "Actions Named Cloudflare Tunnel 的 token",
    ),
    ("actions_frp_token", "覆盖 Actions 隧道使用的 frps token"),
];

pub async fn run(ctx: &mut Ctx, command: SecretCmd) -> CliResult {
    if ctx.explicit_workspace && !matches!(command, SecretCmd::Keys | SecretCmd::Shared(_)) {
        return run_project(ctx, command).await;
    }
    match command {
        SecretCmd::List { key, reveal } => list_service(ctx, key, reveal).await,
        SecretCmd::Set { key, value } => {
            ctx.backend
                .call(Request::SetHubSecret {
                    key: key.clone(),
                    value,
                })
                .await?;
            if !ctx.out.json_or(&json!({ "key": key, "updated": true })) {
                ctx.out
                    .line(format!("已设置 {key}；服务在跑的话已自动重启。"));
                ctx.out.note(client_impact(&key));
            }
            Ok(())
        }
        SecretCmd::Regenerate { key } => regenerate_service(ctx, key).await,
        SecretCmd::Shared(command) => shared(ctx, command).await,
        SecretCmd::Keys => {
            if ctx.out.json_or(&json!({
                "service": SERVICE_KEYS.iter().map(|(key, _)| key).collect::<Vec<_>>(),
                "workspace": WORKSPACE_SECRET_KEYS,
                "shared": SHARED_SECRET_KEYS,
            })) {
                return Ok(());
            }
            let mut rows: Vec<Vec<String>> = SERVICE_KEYS
                .iter()
                .map(|(key, doc)| vec![key.to_string(), "服务".into(), doc.to_string()])
                .collect();
            rows.extend(
                KEY_DOCS
                    .iter()
                    .map(|(key, doc)| vec![key.to_string(), "项目（-w）".into(), doc.to_string()]),
            );
            ctx.out.table(&["凭据名", "属于", "用途"], &rows);
            ctx.out.line("");
            ctx.out.line(
                "服务的：gld secret ls|set|regen <KEY>     项目的 GPT Actions：gld secret ls|set|regen <KEY> -w <项目>",
            );
            Ok(())
        }
    }
}

async fn list_service(ctx: &mut Ctx, key: Option<String>, reveal: bool) -> CliResult {
    let keys: Vec<&str> = match &key {
        Some(key) => vec![key.as_str()],
        None => SERVICE_KEYS.iter().map(|(key, _)| *key).collect(),
    };
    let mut rows = Vec::new();
    for key in keys {
        let value: String = ctx
            .backend
            .call_typed(Request::HubSecret { key: key.into() })
            .await?;
        rows.push((key.to_string(), value));
    }
    let shown: Vec<(String, String)> = rows
        .into_iter()
        .map(|(key, value)| {
            let text = if value.is_empty() {
                String::new()
            } else if reveal {
                value
            } else {
                mask(&value)
            };
            (key, text)
        })
        .collect();
    if ctx.out.json_or(&json!(shown
        .iter()
        .map(|(key, value)| json!({ "key": key, "value": value, "scope": "service" }))
        .collect::<Vec<_>>()))
    {
        return Ok(());
    }
    ctx.out.kv(&shown
        .iter()
        .map(|(key, value)| {
            (
                key.as_str(),
                if value.is_empty() {
                    ctx.out.dim("未设置")
                } else {
                    value.clone()
                },
            )
        })
        .collect::<Vec<_>>());
    if !reveal {
        ctx.out.note("已脱敏，--reveal 显示明文。");
    }
    Ok(())
}

pub async fn regenerate_service(ctx: &mut Ctx, key: String) -> CliResult {
    let value: String = ctx
        .backend
        .call_typed(Request::RegenerateHubSecret { key: key.clone() })
        .await?;
    if ctx.out.json_or(&json!({ "key": key, "value": value })) {
        return Ok(());
    }
    ctx.out.kv(&[(key.as_str(), value)]);
    ctx.out.note(client_impact(&key));
    Ok(())
}

/// 旧的项目级凭据（`-w <项目>`）：现在只有项目的 GPT Actions 用。
async fn run_project(ctx: &mut Ctx, command: SecretCmd) -> CliResult {
    let target = ctx.target.clone();
    match command {
        SecretCmd::List { key, reveal } => {
            let Some(key) = key else {
                return Err(crate::error::CliError::new(
                    "项目的凭据要点名看哪一项：gld secret ls <KEY> -w <项目>（gld secret keys 看有哪些）",
                ));
            };
            // 勾了 shared-secrets 的项目，服务读的是共享池，项目自己那份
            // 完全不参与。这里必须给"真正生效的那个"。
            let profile: WorkspaceProfile = ctx
                .backend
                .call_typed(Request::ResolveWorkspace {
                    target: target.clone(),
                })
                .await?;
            let shared = reads_from_shared_pool(&profile, &key);
            let value: Option<String> = if shared {
                ctx.backend
                    .call_typed(Request::SharedSecret { key: key.clone() })
                    .await?
            } else {
                ctx.backend
                    .call_typed(Request::WorkspaceSecret {
                        target,
                        key: key.clone(),
                    })
                    .await?
            };
            let hint = shared.then_some(
                "这个项目勾了 shared-secrets，值来自共享池；改它用 gld secret shared set。",
            );
            let scope = if shared { "shared" } else { "workspace" };
            show_scoped(ctx, &key, value, reveal, scope, hint)
        }
        SecretCmd::Set { key, value } => {
            let shared = uses_shared_pool(ctx, &key).await?;
            ctx.backend
                .call(Request::SetWorkspaceSecret {
                    target,
                    key: key.clone(),
                    value,
                })
                .await?;
            if !ctx.out.json_or(&json!({ "key": key, "updated": true })) {
                if shared {
                    // 这种情况下服务不会重启，也不该重启——它读的是另一份。
                    ctx.out.line(format!("已写入项目的 {key}。"));
                } else {
                    ctx.out
                        .line(format!("已设置 {key}；正在运行且用到它的服务已自动重启。"));
                }
                warn_if_shadowed_by_pool(ctx, &key, shared);
            }
            Ok(())
        }
        SecretCmd::Regenerate { key } => {
            let shared = uses_shared_pool(ctx, &key).await?;
            let value: String = ctx
                .backend
                .call_typed(Request::RegenerateWorkspaceSecret {
                    target,
                    key: key.clone(),
                })
                .await?;
            if !ctx.out.json_or(&json!({ "key": key, "value": value })) {
                ctx.out.line(format!("{key} 已重新生成：{value}"));
                if shared {
                    warn_if_shadowed_by_pool(ctx, &key, shared);
                } else {
                    ctx.out.note(client_impact(&key));
                }
            }
            Ok(())
        }
        SecretCmd::Shared(_) | SecretCmd::Keys => unreachable!("handled by run"),
    }
}

async fn shared(ctx: &mut Ctx, command: SharedSecretCmd) -> CliResult {
    match command {
        SharedSecretCmd::List { key, reveal } => {
            let value: Option<String> = ctx
                .backend
                .call_typed(Request::SharedSecret { key: key.clone() })
                .await?;
            show_scoped(ctx, &key, value, reveal, "shared", None)
        }
        SharedSecretCmd::Set { key, value } => {
            ctx.backend
                .call(Request::SetSharedSecret {
                    key: key.clone(),
                    value,
                })
                .await?;
            if !ctx.out.json_or(&json!({ "key": key, "updated": true })) {
                ctx.out.line(format!(
                    "已设置共享密钥 {key}；使用共享池且正在运行的服务已自动重启。"
                ));
            }
            Ok(())
        }
        SharedSecretCmd::Regenerate { key } => {
            let value: String = ctx
                .backend
                .call_typed(Request::RegenerateSharedSecret { key: key.clone() })
                .await?;
            if !ctx.out.json_or(&json!({ "key": key, "value": value })) {
                ctx.out.line(format!("共享密钥 {key} 已重新生成：{value}"));
                ctx.out.note(client_impact(&key));
            }
            Ok(())
        }
    }
}

/// 换掉这个密钥之后，已经连好的客户端会怎么样。
///
/// 分清楚很要紧：`oauth_token_secret` 把已经发出去的令牌一起作废，
/// 每个连接器都得重新授权；而 `oauth_password` 只影响"下次授权时填什么"，
/// 已经授权过的客户端照常能用。以前这两种情况共用一句"记得更新客户端里的配置"，
/// 而 OAuth 客户端根本没有"配置里的密钥"可更新，照着做只会更迷糊。
fn client_impact(key: &str) -> &'static str {
    match key {
        "oauth_token_secret" | "actions_oauth_token_secret" => {
            "已经发出去的访问令牌全部作废，每个连上的客户端都要重新授权一次。\
             ChatGPT 那边不用删连接器：它会自己弹出重新授权，输一次口令就好。"
        }
        "oauth_password" | "actions_oauth_password" => {
            "已经授权过的客户端不受影响，继续能用。只有下次重新授权时，\
             授权页要填这个新口令。"
        }
        "oauth_client_id" => {
            "只影响手填了静态 Client ID 的客户端；ChatGPT 这类自动注册的不受影响。"
        }
        "cloudflare_token" => "隧道下次起来时用它；服务在跑的话已经按它重启了。",
        _ => "旧值立即失效，记得更新客户端里的配置。",
    }
}

/// 当前项目的这个 key 是不是从共享池读。
async fn uses_shared_pool(ctx: &mut Ctx, key: &str) -> CliResult<bool> {
    if !SHARED_SECRET_KEYS.contains(&key) {
        return Ok(false);
    }
    let profile: WorkspaceProfile = ctx
        .backend
        .call_typed(Request::ResolveWorkspace {
            target: ctx.target.clone(),
        })
        .await?;
    Ok(reads_from_shared_pool(&profile, key))
}

/// 写了工作区那份、但服务其实读的是共享池——写完等于没写，得说一声。
fn warn_if_shadowed_by_pool(ctx: &Ctx, key: &str, shared: bool) {
    if shared {
        ctx.out.note(format!(
            "注意：这个项目勾了 shared-secrets，服务读的是共享池，这次改动不会生效。\
             要改生效的那份：gld secret shared set {key} <值>"
        ));
    }
}

/// 显示一个密钥。
///
/// `scope` 说明这个值存在哪儿（"workspace" / "shared"），`hint` 是额外要交代的
/// 一句话——只有"工作区在读共享池"这一种情况需要，`gld secret shared show`
/// 本来就是在看池子，不用解释。
fn show_scoped(
    ctx: &Ctx,
    key: &str,
    value: Option<String>,
    reveal: bool,
    scope: &str,
    hint: Option<&str>,
) -> CliResult {
    let shown = value
        .as_deref()
        .map(|v| if reveal { v.to_string() } else { mask(v) });
    if ctx
        .out
        .json_or(&json!({ "key": key, "value": shown, "scope": scope }))
    {
        return Ok(());
    }
    match shown {
        Some(text) => {
            ctx.out.line(format!("{key} = {text}"));
            if let Some(hint) = hint {
                ctx.out.note(hint);
            }
            if !reveal {
                ctx.out.note("已脱敏，--reveal 显示明文。");
            }
        }
        None => ctx.out.line(format!("{key} 未设置。")),
    }
    Ok(())
}
