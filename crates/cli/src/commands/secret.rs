use gld_core::app::{reads_from_shared_pool, SHARED_SECRET_KEYS, WORKSPACE_SECRET_KEYS};
use gld_core::workspace::WorkspaceProfile;
use gld_daemon::Request;
use serde_json::json;

use super::Ctx;
use crate::cli::{SecretCmd, SharedSecretCmd};
use crate::error::CliResult;
use crate::output::mask;

const KEY_DOCS: &[(&str, &str)] = &[
    ("bearer_token", "MCP 认证方式为 bearer 时客户端携带的 Token"),
    (
        "oauth_client_id",
        "MCP OAuth Client ID（仅共享池；工作区级用 gld ws set mcp.oauth-client-id）",
    ),
    (
        "oauth_client_secret",
        "MCP OAuth 静态 Client Secret（可选；ChatGPT 走 PKCE 不需要）",
    ),
    ("oauth_password", "MCP OAuth 授权页输入的口令"),
    (
        "oauth_token_secret",
        "签发 MCP Access / Refresh Token 用的密钥",
    ),
    ("cloudflare_token", "MCP Named Cloudflare Tunnel 的 token"),
    (
        "frp_token",
        "覆盖 MCP 隧道使用的 frps token（通常配在 FRP 配置里）",
    ),
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
    let target = ctx.target.clone();
    match command {
        SecretCmd::Show { key, reveal } => {
            // 勾了 shared-secrets 的工作区，服务读的是共享池，工作区自己那份
            // 完全不参与。这里必须给"真正生效的那个"，否则用户照着配客户端
            // 会一直 401——而 gld list 显示的又是对的，两边对不上更难查。
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
                "这个工作区勾了 shared-secrets，值来自共享池；改它用 gld secret shared set。",
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
                    ctx.out.line(format!("已写入工作区的 {key}。"));
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
                    ctx.out.note("旧值立即失效，记得更新客户端里的配置。");
                }
            }
            Ok(())
        }
        SecretCmd::Shared(command) => shared(ctx, command).await,
        SecretCmd::Keys => {
            if ctx.out.json_or(
                &json!({ "workspace": WORKSPACE_SECRET_KEYS, "shared": SHARED_SECRET_KEYS }),
            ) {
                return Ok(());
            }
            let rows: Vec<Vec<String>> = KEY_DOCS
                .iter()
                .map(|(key, doc)| {
                    let scope = match (
                        WORKSPACE_SECRET_KEYS.contains(key),
                        SHARED_SECRET_KEYS.contains(key),
                    ) {
                        (true, true) => "工作区 / 共享",
                        (true, false) => "工作区",
                        (false, true) => "共享",
                        (false, false) => "-",
                    };
                    vec![key.to_string(), scope.to_string(), doc.to_string()]
                })
                .collect();
            ctx.out.table(&["密钥名", "作用域", "用途"], &rows);
            ctx.out.line("");
            ctx.out.line("工作区级：gld secret show|set|regen <KEY>     共享池：gld secret shared show|set|regen <KEY>");
            Ok(())
        }
    }
}

async fn shared(ctx: &mut Ctx, command: SharedSecretCmd) -> CliResult {
    match command {
        SharedSecretCmd::Show { key, reveal } => {
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
            }
            Ok(())
        }
    }
}

/// 当前工作区的这个 key 是不是从共享池读。
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
            "注意：这个工作区勾了 shared-secrets，服务读的是共享池，这次改动不会生效。\
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
