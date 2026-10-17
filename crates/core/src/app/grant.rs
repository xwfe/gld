//! `gld grant`：发一把只开部分项目的凭据、列出来、作废（RFC-0007）。
//!
//! 范围怎么在请求里生效在 [`crate::hub`]，钥匙怎么验在 [`crate::auth`]。这里只管登记。

use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::App;
use crate::auth::{
    BrowserCredential, Grant, GrantRecord, DEFAULT_BROWSER_GRANT_TTL_SECONDS,
    DEFAULT_BROWSER_ORIGIN, MAX_BROWSER_GRANT_TTL_SECONDS,
};
use crate::error::{AppError, AppResult};

/// `gld grant add` 收到的参数。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrantSpec {
    pub name: String,
    /// 服务里的项目：id、名称或 id 前缀（≥4 位），本地和远端都行。
    pub workspaces: Vec<String>,
    /// 能写能跑命令。默认 false（只读）：能跑命令就能以你的身份读到数据目录里的服务
    /// 口令，拿到全权，所以要显式要（`gld grant add --write`）。
    #[serde(default)]
    pub writable: bool,
}

/// 浏览器连接单独发短期钥匙；旧 AddGrant IPC 不接受这项操作。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserGrantSpec {
    pub name: String,
    pub workspaces: Vec<String>,
    #[serde(default)]
    pub writable: bool,
    #[serde(default = "default_browser_ttl")]
    pub ttl_seconds: u64,
    #[serde(default)]
    pub allowed_origins: Vec<String>,
}

fn default_browser_ttl() -> u64 {
    DEFAULT_BROWSER_GRANT_TTL_SECONDS
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserGrantInfoDto {
    pub expires_at_unix_secs: u64,
    pub allowed_origins: Vec<String>,
}

/// 原始 URL 仅由创建操作返回，列表和删除回包没有它。
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserGrantCreatedDto {
    pub grant: GrantDto,
    pub local_url: String,
    pub public_url: Option<String>,
    pub expires_at_unix_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrantMemberDto {
    pub id: String,
    /// 项目已经不在服务里时是空串：grant 里还记着它的 id，但它不会被看到。
    pub name: String,
}

/// 一把 grant。钥匙是明文，由命令行决定脱不脱敏（和 `gld secret ls` 一样）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrantDto {
    pub id: String,
    pub name: String,
    pub workspaces: Vec<GrantMemberDto>,
    pub read_only: bool,
    pub created_at: String,
    /// OAuth 授权页上填的口令。
    pub oauth_password: String,
    /// bearer 认证时客户端带的令牌；浏览器专用 grant 为空。
    pub bearer_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser: Option<BrowserGrantInfoDto>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrantRemoved {
    pub grant: GrantDto,
    /// 它起的、被一起停掉的命令会话数。
    pub stopped_commands: usize,
}

impl App {
    pub fn list_grants(&self) -> AppResult<Vec<GrantDto>> {
        let records = self.with_data(|store| Ok(store.grants().to_vec()))?;
        let members = self.hub_member_names()?;
        Ok(records
            .iter()
            .map(|record| grant_dto(record, &members))
            .collect())
    }

    /// 发一把新的。口令和令牌当场随机生成，两样都给：服务换认证方式时不用重发。
    pub fn add_grant(&self, spec: GrantSpec) -> AppResult<GrantDto> {
        let (grant, members) = self.prepare_grant(spec)?;
        let record = GrantRecord {
            grant,
            oauth_password: crate::data::random_secret(),
            bearer_token: crate::data::random_secret(),
            created_at: created_at(),
            browser: None,
        };
        self.store_grant(&record)?;
        Ok(grant_dto(&record, &members))
    }

    pub fn add_browser_grant(&self, spec: BrowserGrantSpec) -> AppResult<BrowserGrantCreatedDto> {
        if spec.ttl_seconds == 0 || spec.ttl_seconds > MAX_BROWSER_GRANT_TTL_SECONDS {
            return Err(AppError::Message(
                "浏览器 grant 的有效期必须在 1 秒到 24 小时之间".into(),
            ));
        }
        let offered_origins = if spec.allowed_origins.is_empty() {
            vec![DEFAULT_BROWSER_ORIGIN.to_string()]
        } else {
            spec.allowed_origins
        };
        let mut allowed_origins = Vec::new();
        for origin in offered_origins {
            if !valid_browser_origin(&origin) {
                return Err(AppError::Message(format!(
                    "无效的浏览器 Origin「{origin}」：填写无路径、凭据或查询参数的 HTTPS、扩展 Origin 或本机 HTTP Origin"
                )));
            }
            if !allowed_origins.contains(&origin) {
                allowed_origins.push(origin);
            }
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| AppError::Message("系统时间早于 Unix 时间起点，不能创建短期凭据".into()))?
            .as_secs();
        let expires_at_unix_secs = now
            .checked_add(spec.ttl_seconds)
            .ok_or_else(|| AppError::Message("浏览器 grant 的到期时间溢出".into()))?;
        let (grant, members) = self.prepare_grant(GrantSpec {
            name: spec.name,
            workspaces: spec.workspaces,
            writable: spec.writable,
        })?;
        let settings = self.settings()?;
        if settings.hub.local_port == 0 {
            return Err(AppError::Message("服务端口必须在 1-65535".into()));
        }
        let token = crate::data::random_secret();
        let local_url = format!(
            "http://127.0.0.1:{}/mcp/browser/{token}",
            settings.hub.local_port
        );
        // 只读受信的服务配置，不取请求 Host，也不启动或改变任何隧道。
        let public_base = crate::hub::public_base_url(&settings);
        let public_url = if public_base.is_empty() {
            None
        } else {
            Some(browser_public_url(&public_base, &token)?)
        };
        let record = GrantRecord {
            grant,
            oauth_password: String::new(),
            bearer_token: String::new(),
            created_at: created_at(),
            browser: Some(BrowserCredential::new(
                &token,
                expires_at_unix_secs,
                allowed_origins,
            )),
        };
        self.store_grant(&record)?;
        Ok(BrowserGrantCreatedDto {
            grant: grant_dto(&record, &members),
            local_url,
            public_url,
            expires_at_unix_secs,
        })
    }

    fn prepare_grant(&self, spec: GrantSpec) -> AppResult<(Grant, Vec<(String, String)>)> {
        let name = spec.name.trim().to_string();
        if name.is_empty() || name.chars().count() > 64 {
            return Err(AppError::Message(
                "grant 要有个名字（64 个字以内），用来在 gld grant ls / rm 里认它".into(),
            ));
        }
        // noauth 下谁连上都是全权，没有"用哪把钥匙进来"这回事：建了也挡不住任何人。
        if self.settings()?.hub.auth_type == "noauth" {
            return Err(AppError::Message(
                "服务的认证是 noauth：谁连上都是全权，按凭据分范围没有意义。先 gld upgrade --auth oauth（或 bearer）".into(),
            ));
        }
        if spec.workspaces.is_empty() {
            return Err(AppError::Message(
                "至少开一个项目：gld grant add <名字> api web".into(),
            ));
        }
        let members = self.hub_member_names()?;
        let profiles = self.list_workspaces()?;
        let mut workspaces: Vec<String> = Vec::new();
        for selector in &spec.workspaces {
            let id = resolve_member(&members, selector)?;
            // 关了 confine-reads 的项目，读工具能读整台机器；服务也会拒 grant 用它，
            // 这里先拦下，免得发出去一把用不了的。
            if let Some(open) = profiles
                .iter()
                .find(|profile| profile.id == id && !profile.runtime.confine_reads)
            {
                return Err(AppError::Message(format!(
                    "项目「{}」关了 confine-reads：经它能读到项目目录外面，不能开给 grant。要开就先 gld set {} confine-reads=true",
                    open.name, open.name
                )));
            }
            if !workspaces.contains(&id) {
                workspaces.push(id);
            }
        }
        Ok((
            Grant {
                id: uuid::Uuid::new_v4().simple().to_string(),
                name,
                workspaces,
                read_only: !spec.writable,
            },
            members,
        ))
    }

    fn store_grant(&self, record: &GrantRecord) -> AppResult<()> {
        // 查重和写入在同一次 with_data 里，浏览器和普通 grant 共享名字空间。
        self.with_data(|store| {
            if let Some(clash) = store
                .grants()
                .iter()
                .find(|item| item.grant.name.eq_ignore_ascii_case(&record.grant.name))
            {
                return Err(AppError::Message(format!(
                    "已经有一个叫「{}」的 grant 了。换个名字，或者先 gld grant rm {} 再建",
                    clash.grant.name, clash.grant.name
                )));
            }
            store.add_grant(record.clone())
        })
    }

    /// 作废一把：下一次请求起它的令牌 401，刷新也换不来新的；它起的命令一起停掉。
    pub fn remove_grant(&self, selector: &str) -> AppResult<GrantRemoved> {
        let selector = selector.trim();
        let removed = self.with_data(|store| {
            let found = store
                .grants()
                .iter()
                .find(|item| item.grant.id == selector)
                .or_else(|| {
                    store
                        .grants()
                        .iter()
                        .find(|item| item.grant.name.eq_ignore_ascii_case(selector))
                })
                .map(|item| item.grant.id.clone());
            match found {
                Some(id) => store.remove_grant(&id),
                None => Ok(None),
            }
        })?;
        let Some(record) = removed else {
            return Err(AppError::Message(format!(
                "没有叫「{selector}」的 grant。看看有哪些：gld grant ls"
            )));
        };
        // 先删记录再停命令：反过来的话，停完到删掉之间它还能再起一条。
        let grant_id = record.grant.id.clone();
        let stopped_commands = crate::tools::workspace_runtime::terminate_grant_sessions(&grant_id);
        // 删之前已经过了鉴权的那一次调用还在跑：它要起命令的话，可能正排在写锁上（最多等
        // 30 秒），起来时已经在上面那次清理之后了，没人读得到、停得掉，一直跑到它自己的
        // timeout（上限 10 分钟）。35 秒后再清一次。
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(35));
            crate::tools::workspace_runtime::terminate_grant_sessions(&grant_id);
        });
        let members = self.hub_member_names()?;
        Ok(GrantRemoved {
            grant: grant_dto(&record, &members),
            stopped_commands,
        })
    }

    /// 服务里的成员（本地和远端），按加入顺序：(id, 名称)。
    fn hub_member_names(&self) -> AppResult<Vec<(String, String)>> {
        let ids = self.settings()?.hub.members;
        let profiles = self.list_workspaces()?;
        let remotes = self.ccnm_members()?;
        Ok(ids
            .iter()
            .filter_map(|id| {
                profiles
                    .iter()
                    .find(|profile| &profile.id == id)
                    .map(|profile| (profile.id.clone(), profile.name.clone()))
                    .or_else(|| {
                        remotes
                            .iter()
                            .find(|remote| &remote.id == id)
                            .map(|remote| (remote.id.clone(), remote.name.clone()))
                    })
            })
            .collect())
    }
}

fn created_at() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().to_string())
        .unwrap_or_else(|_| "0".into())
}

fn loopback_url(url: &reqwest::Url) -> bool {
    url.host_str().is_some_and(|host| {
        host == "localhost"
            || host
                .trim_start_matches('[')
                .trim_end_matches(']')
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    })
}

fn valid_browser_origin(origin: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(origin) else {
        return false;
    };
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return false;
    }
    let canonical = match url.scheme() {
        "https" => url.origin().ascii_serialization(),
        "http" if loopback_url(&url) => url.origin().ascii_serialization(),
        "chrome-extension" | "moz-extension" if url.port().is_none() => {
            let Some(host) = url.host_str() else {
                return false;
            };
            if host.is_empty()
                || !host
                    .chars()
                    .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
            {
                return false;
            }
            format!("{}://{host}", url.scheme())
        }
        _ => return false,
    };
    // 逐字匹配序列化 origin：不接受通配符、null、路径、尾部 / 或默认端口写法。
    origin == canonical
}

fn browser_public_url(base: &str, token: &str) -> AppResult<String> {
    let url = reqwest::Url::parse(base).map_err(|_| {
        AppError::Message("服务的公网地址不是有效 URL，不能生成浏览器接入地址".into())
    })?;
    if !(url.scheme() == "https" || (url.scheme() == "http" && loopback_url(&url)))
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(AppError::Message(
            "浏览器接入只支持 HTTPS 或本机 HTTP 地址，公网地址不能带凭据、查询参数或片段".into(),
        ));
    }
    Ok(format!(
        "{}/mcp/browser/{token}",
        base.trim_end_matches('/')
    ))
}

/// 在服务成员里找：id、名称（不分大小写）、id 前缀（≥4 位）。只认服务里的——grant
/// 开的是"经服务访问"的范围，登记了但不在服务里的项目经服务本来就进不去。
fn resolve_member(members: &[(String, String)], selector: &str) -> AppResult<String> {
    let selector = selector.trim();
    if let Some((id, _)) = members.iter().find(|(id, _)| id == selector) {
        return Ok(id.clone());
    }
    let by_name: Vec<&(String, String)> = members
        .iter()
        .filter(|(_, name)| name.eq_ignore_ascii_case(selector))
        .collect();
    let matched = if by_name.is_empty() && selector.len() >= 4 {
        members
            .iter()
            .filter(|(id, _)| id.starts_with(selector))
            .collect()
    } else {
        by_name
    };
    match matched.as_slice() {
        [(id, _)] => Ok(id.clone()),
        [] => Err(AppError::Message(format!(
            "「{selector}」不在服务里。gld ls 看有哪些项目；没加进来的先 gld add"
        ))),
        several => Err(AppError::Message(format!(
            "「{selector}」对得上好几个项目：{}。改用完整 id",
            several
                .iter()
                .map(|(id, name)| format!("{name}（{}）", crate::short_id(id)))
                .collect::<Vec<_>>()
                .join("、")
        ))),
    }
}

fn grant_dto(record: &GrantRecord, members: &[(String, String)]) -> GrantDto {
    GrantDto {
        id: record.grant.id.clone(),
        name: record.grant.name.clone(),
        workspaces: record
            .grant
            .workspaces
            .iter()
            .map(|id| GrantMemberDto {
                id: id.clone(),
                name: members
                    .iter()
                    .find(|(member, _)| member == id)
                    .map(|(_, name)| name.clone())
                    .unwrap_or_default(),
            })
            .collect(),
        read_only: record.grant.read_only,
        created_at: record.created_at.clone(),
        oauth_password: record.oauth_password.clone(),
        bearer_token: record.bearer_token.clone(),
        browser: record.browser.as_ref().map(|key| BrowserGrantInfoDto {
            expires_at_unix_secs: key.expires_at_unix_secs,
            allowed_origins: key.allowed_origins.clone(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_origins_are_exact_serialized_origins() {
        for origin in [
            DEFAULT_BROWSER_ORIGIN,
            "moz-extension://12345678-1234-1234-1234-123456789abc",
            "https://tools.example.test",
            "https://tools.example.test:8443",
            "http://localhost:8080",
            "http://127.0.0.1:8080",
            "http://[::1]:8080",
        ] {
            assert!(valid_browser_origin(origin), "{origin}");
        }
        for origin in [
            "*",
            "null",
            "",
            "https://tools.example.test/",
            "https://tools.example.test/path",
            "https://tools.example.test?x=1",
            "https://tools.example.test#x",
            "https://user:password@tools.example.test",
            "https://tools.example.test:443",
            "http://tools.example.test",
            "http://localhost.evil.test",
            "chrome-extension://id/path",
            "chrome-extension://id:8080",
            "chrome-extension://*",
            "file:///tmp",
            " https://tools.example.test",
        ] {
            assert!(!valid_browser_origin(origin), "{origin}");
        }
    }

    #[test]
    fn browser_public_urls_preserve_configured_prefix_without_accepting_unsafe_bases() {
        assert_eq!(
            browser_public_url("https://hub.example/hub/", "test-key").unwrap(),
            "https://hub.example/hub/mcp/browser/test-key"
        );
        assert!(browser_public_url("http://127.0.0.1:8080", "test-key").is_ok());
        for base in [
            "http://remote.example",
            "https://user:pass@remote.example",
            "https://remote.example?x=1",
            "https://remote.example#frag",
        ] {
            assert!(browser_public_url(base, "test-key").is_err(), "{base}");
        }
    }

    fn members() -> Vec<(String, String)> {
        vec![
            ("a1b2c3d4e5".into(), "api".into()),
            ("f6a7b8c9d0".into(), "web".into()),
            ("remote-prod".into(), "prod".into()),
        ]
    }

    #[test]
    fn members_resolve_by_id_name_or_prefix_and_nothing_else() {
        let members = members();
        assert_eq!(resolve_member(&members, "API").unwrap(), "a1b2c3d4e5");
        assert_eq!(resolve_member(&members, "f6a7").unwrap(), "f6a7b8c9d0");
        assert_eq!(
            resolve_member(&members, "remote-prod").unwrap(),
            "remote-prod"
        );
        assert!(resolve_member(&members, "f6a").is_err(), "前缀太短不猜");
        let missing = resolve_member(&members, "outsider")
            .unwrap_err()
            .to_string();
        assert!(missing.contains("不在服务里"), "{missing}");
    }
}
