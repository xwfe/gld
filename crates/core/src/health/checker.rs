use std::time::Duration;

use serde::Serialize;

use crate::workspace::WorkspaceProfile;

const TIMEOUT: Duration = Duration::from_secs(4);

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthItem {
    pub label: String,
    pub ok: bool,
    pub detail: String,
    pub hint: String,
}

/// 两个客户端：本机地址必须绕过环境变量里的 HTTP(S)_PROXY，否则用户配了
/// 全局代理时，对 127.0.0.1 的探测会被代理吃掉并返回 502；公网地址则照常走代理。
struct Clients {
    local: reqwest::Client,
    public: reqwest::Client,
}

impl Clients {
    fn new() -> Self {
        Self {
            local: reqwest::Client::builder()
                .timeout(TIMEOUT)
                .no_proxy()
                .build()
                .expect("failed to build local HTTP client"),
            public: reqwest::Client::builder()
                .timeout(TIMEOUT)
                .build()
                .expect("failed to build HTTP client"),
        }
    }

    fn for_url(&self, url: &str) -> &reqwest::Client {
        if is_loopback_url(url) {
            &self.local
        } else {
            &self.public
        }
    }
}

pub(crate) fn is_loopback_url(url: &str) -> bool {
    let host = url
        .trim_start_matches("http://")
        .trim_start_matches("https://")
        .split(['/', ':'])
        .next()
        .unwrap_or("");
    matches!(
        host,
        "127.0.0.1" | "localhost" | "[::1]" | "::1" | "0.0.0.0"
    )
}

fn format_single_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        other => other.to_string(),
    }
}

fn format_field_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Array(items) => items
            .iter()
            .map(format_single_value)
            .collect::<Vec<_>>()
            .join(" / "),
        other => format_single_value(other),
    }
}

/// 一次探测算不算通过。
///
/// 200 = 服务在；401 = 服务在，只是要凭据（认证方式不是 noauth 时的正常反应）。
/// 其余都算没通过，**404 尤其不算**：`/mcp`、`/health`、`/openapi.json` 都是
/// gld 自己的固定路径，我们的服务不会在这些路径上回 404。回了就说明这个端口上
/// 应答的是别的程序——Actions 默认端口 8787 很容易被别的开发服务占掉。
///
/// 这里原来把 404 也当成通过，于是 `gld health` 会给一台完全无关的服务打勾，
/// 而 `gld doctor` 同时报「端口已被占用」，两个排障命令互相打架，
/// 照着 health 的勾去排查会绕远路。
fn endpoint_ok(code: u16) -> bool {
    matches!(code, 200 | 401)
}

async fn check_url(client: &reqwest::Client, url: &str) -> (bool, String) {
    if url.is_empty() {
        return (false, "URL not configured".to_string());
    }
    match client.get(url).send().await {
        Ok(response) => {
            let code = response.status().as_u16();
            let ok = endpoint_ok(code);
            if !ok && code == 404 && is_loopback_url(url) {
                return (
                    false,
                    "HTTP 404（这个端口上应答的不是 gld 的服务）".to_string(),
                );
            }
            (ok, format!("HTTP {code}"))
        }
        Err(err) => (false, err.to_string()),
    }
}

async fn check_mcp_public_url(client: &reqwest::Client, url: &str) -> (bool, String) {
    if url.is_empty() {
        return (false, "URL not configured".to_string());
    }
    match client.get(url).send().await {
        Ok(response) => {
            let code = response.status().as_u16();
            let body = response.text().await.unwrap_or_default();
            let lower = body.to_ascii_lowercase();
            if (lower.contains("powered by") && lower.contains("frp"))
                || (lower.contains("the page you requested was not found") && lower.contains("frp"))
            {
                return (
                    false,
                    format!("HTTP {code}; FRP 未挂载代理（返回 frp 404 页）"),
                );
            }
            let ok = matches!(code, 200 | 401 | 405);
            (ok, format!("HTTP {code}"))
        }
        Err(err) => (false, err.to_string()),
    }
}

async fn check_json_field(client: &reqwest::Client, url: &str, field: &str) -> (bool, String) {
    if url.is_empty() {
        return (false, "URL not configured".to_string());
    }
    match client.get(url).send().await {
        Ok(response) => {
            let status = response.status();
            if !status.is_success() {
                return (false, format!("HTTP {}", status.as_u16()));
            }
            match response.json::<serde_json::Value>().await {
                Ok(payload) => {
                    let value = payload
                        .get(field)
                        .map(format_field_value)
                        .unwrap_or_default();
                    (true, format!("HTTP {}; {field}={value}", status.as_u16()))
                }
                Err(err) => (false, err.to_string()),
            }
        }
        Err(err) => (false, err.to_string()),
    }
}

fn well_known_url(base: &str, path: &str) -> String {
    if base.is_empty() {
        return String::new();
    }
    format!("{}/{}", base.trim_end_matches('/'), path)
}

pub async fn run_health_checks(profile: &WorkspaceProfile) -> Vec<HealthItem> {
    let clients = Clients::new();
    let mcp_public = profile.effective_public_url();
    let actions_local = profile.actions_local_base_url();
    let actions_public = profile.actions_effective_public_url();
    let actions_oauth_base = if actions_public.is_empty() {
        actions_local.clone()
    } else {
        actions_public.clone()
    };

    let mcp_local_url = profile.local_endpoint();
    let (mcp_local_ok, mcp_local_detail) =
        check_url(clients.for_url(&mcp_local_url), &mcp_local_url).await;
    let mcp_public_url = profile.public_endpoint();
    let (mcp_public_ok, mcp_public_detail) =
        check_mcp_public_url(clients.for_url(&mcp_public_url), &mcp_public_url).await;
    let mcp_oauth_url = well_known_url(&mcp_public, ".well-known/oauth-authorization-server");
    let (mcp_oauth_ok, mcp_oauth_detail) = check_json_field(
        clients.for_url(&mcp_oauth_url),
        &mcp_oauth_url,
        "token_endpoint_auth_methods_supported",
    )
    .await;
    let mcp_protected_url = well_known_url(&mcp_public, ".well-known/oauth-protected-resource");
    let (mcp_protected_ok, mcp_protected_detail) = check_json_field(
        clients.for_url(&mcp_protected_url),
        &mcp_protected_url,
        "authorization_servers",
    )
    .await;

    let actions_health_url = format!("{actions_local}/health");
    let actions_openapi_local = format!("{actions_local}/openapi.json");
    let actions_openapi_public = profile.actions_openapi_url();

    let (actions_local_ok, actions_local_detail) =
        check_url(clients.for_url(&actions_health_url), &actions_health_url).await;
    let (actions_openapi_local_ok, actions_openapi_local_detail) = check_url(
        clients.for_url(&actions_openapi_local),
        &actions_openapi_local,
    )
    .await;
    let (actions_openapi_public_ok, actions_openapi_public_detail) = check_url(
        clients.for_url(&actions_openapi_public),
        &actions_openapi_public,
    )
    .await;
    let actions_oauth_url = well_known_url(
        &actions_oauth_base,
        ".well-known/oauth-authorization-server",
    );
    let (actions_oauth_ok, actions_oauth_detail) = check_json_field(
        clients.for_url(&actions_oauth_url),
        &actions_oauth_url,
        "token_endpoint_auth_methods_supported",
    )
    .await;

    vec![
        health_item(
            "本地 /mcp",
            mcp_local_ok,
            mcp_local_detail,
            "确认 MCP 服务已启动，端口与工作区配置一致。",
        ),
        health_item(
            "公网 /mcp",
            mcp_public_ok,
            mcp_public_detail,
            "检查隧道是否已连接，或公网 URL 是否填写正确。",
        ),
        health_item(
            "MCP OAuth 授权元数据",
            mcp_oauth_ok,
            mcp_oauth_detail,
            "MCP 认证需设为 OAuth，且公网地址可访问。",
        ),
        health_item(
            "MCP OAuth 受保护资源",
            mcp_protected_ok,
            mcp_protected_detail,
            "确认公网 MCP 根地址与 OAuth 配置一致。",
        ),
        health_item(
            "本地 Actions /health",
            actions_local_ok,
            actions_local_detail,
            "确认 Actions 服务已启动；端口被别的程序占着就换一个：gld ws set actions.port=<其他端口>",
        ),
        health_item(
            "本地 Actions /openapi.json",
            actions_openapi_local_ok,
            actions_openapi_local_detail,
            "Actions 监听器异常时请查看 actions-stderr.log。",
        ),
        health_item(
            "公网 Actions /openapi.json",
            actions_openapi_public_ok,
            actions_openapi_public_detail,
            "检查 Actions 隧道与子域名配置。",
        ),
        health_item(
            "Actions OAuth 授权元数据",
            actions_oauth_ok,
            actions_oauth_detail,
            "Actions 认证需设为 OAuth，公网地址需可达。",
        ),
    ]
}

fn health_item(label: &str, ok: bool, detail: String, hint: &str) -> HealthItem {
    HealthItem {
        label: label.into(),
        ok,
        detail,
        hint: if ok { String::new() } else { hint.into() },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    /// 起一个只回固定状态码的假服务器，返回它的地址。
    /// 用它来冒充"占了端口的别的程序"。
    fn server_that_answers(status: &'static str) -> String {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind");
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in listener.incoming().take(1) {
                let mut stream = match stream {
                    Ok(stream) => stream,
                    Err(_) => continue,
                };
                let mut buffer = [0u8; 1024];
                let _ = stream.read(&mut buffer);
                let _ = stream.write_all(
                    format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                        .as_bytes(),
                );
            }
        });
        format!("http://127.0.0.1:{port}/health")
    }

    #[test]
    fn only_200_and_401_count_as_reachable() {
        // 401 = 服务在，只是要凭据。
        assert!(endpoint_ok(200));
        assert!(endpoint_ok(401));
        // 404 不算：我们自己的服务不会在这些固定路径上回 404。
        assert!(!endpoint_ok(404));
        assert!(!endpoint_ok(502));
    }

    /// 端口被别的程序占着时（Actions 默认端口 8787 很容易撞上），
    /// health 必须报失败——不然它给别人的服务打勾，doctor 却报端口冲突，
    /// 两个排障命令说的话对不上。
    #[tokio::test]
    async fn a_stranger_on_the_port_is_not_healthy() {
        let url = server_that_answers("404 Not Found");
        let (ok, detail) = check_url(&Clients::new().local, &url).await;

        assert!(!ok, "404 不该算通过：{detail}");
        assert!(
            detail.contains("不是 gld 的服务"),
            "本机 404 要说清是端口被占，实际：{detail}"
        );
    }

    #[tokio::test]
    async fn an_authenticated_service_is_healthy() {
        let url = server_that_answers("401 Unauthorized");
        let (ok, _) = check_url(&Clients::new().local, &url).await;

        assert!(ok, "401 说明服务在，只是要凭据");
    }
}
