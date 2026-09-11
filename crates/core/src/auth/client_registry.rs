//! 动态注册（RFC 7591）拿到的客户端，落盘保存。
//!
//! 为什么要落盘：ChatGPT 这类客户端连上来时先注册、拿到一个 `dcr-…` 的
//! client_id 存在它自己那边，之后一直拿这个 id 刷新令牌。注册表只放内存的话，
//! 服务一重启表就空了，客户端拿着还没过期的 refresh_token 来续会被判成
//! `invalid_client`——用户看到的是"用得好好的连接器突然要重连，配置还没动过"。
//!
//! 落盘之后重启不再掉授权；就算这个文件丢了，刷新令牌也还能续（见
//! `oauth_flow::refresh_token_exchange`），只是重新授权时要重新注册一次。

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::error::AppResult;
use crate::logs::append_profile_log;

/// 一个作用域最多留多少个已注册客户端，超了按注册时间淘汰最旧的。
///
/// 一个连接器只注册一次，50 个足够覆盖反复删了重建的情况；有上限是因为
/// `/register` 不需要认证，没有上限就等于谁都能往这个文件里无限塞条目。
const MAX_CLIENTS: usize = 50;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RegisteredClient {
    pub redirect_uris: Vec<String>,
    pub token_endpoint_auth_method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
    #[serde(default)]
    pub client_name: String,
    #[serde(default)]
    pub created_at: u64,
}

#[derive(Default, Serialize, Deserialize)]
struct RegistryFile {
    #[serde(default)]
    clients: HashMap<String, RegisteredClient>,
}

/// 一个作用域（一个工作区的 MCP 服务、或它的 Actions 服务）的注册表。
pub struct ClientRegistry {
    /// `None` 表示只在内存里放着，给单元测试用。
    path: Option<PathBuf>,
    /// 写日志用的工作区 id；只记失败，成功路径不出声。
    log_scope: String,
    clients: Mutex<HashMap<String, RegisteredClient>>,
}

impl ClientRegistry {
    /// 读某个作用域的注册表。文件不在、读不动、内容坏了，都降级成空表继续跑——
    /// 注册表丢了只是让客户端多授权一次，不该连服务都起不来。
    pub fn load(scope: &str, log_scope: &str) -> Self {
        let path = match registry_path(scope) {
            Ok(path) => Some(path),
            Err(error) => {
                append_profile_log(
                    log_scope,
                    "stderr.log",
                    &format!("[oauth] 定位不到客户端注册表，本次只用内存：{error}"),
                );
                None
            }
        };
        let clients = path
            .as_ref()
            .filter(|path| path.exists())
            .map(|path| match fs::read_to_string(path) {
                Ok(raw) => match serde_json::from_str::<RegistryFile>(&raw) {
                    Ok(file) => file.clients,
                    Err(error) => {
                        append_profile_log(
                            log_scope,
                            "stderr.log",
                            &format!(
                                "[oauth] 客户端注册表 {} 解析失败（{error}），\
                                 当成空表继续；已连的客户端需要重新授权一次。",
                                path.display()
                            ),
                        );
                        HashMap::new()
                    }
                },
                Err(error) => {
                    append_profile_log(
                        log_scope,
                        "stderr.log",
                        &format!(
                            "[oauth] 客户端注册表 {} 读不动（{error}），当成空表继续。",
                            path.display()
                        ),
                    );
                    HashMap::new()
                }
            })
            .unwrap_or_default();
        Self {
            path,
            log_scope: log_scope.to_string(),
            clients: Mutex::new(clients),
        }
    }

    /// 不落盘的注册表，给单元测试和"拿不到数据目录"的降级路径用。
    pub fn in_memory() -> Self {
        Self {
            path: None,
            log_scope: String::new(),
            clients: Mutex::new(HashMap::new()),
        }
    }

    pub fn get(&self, client_id: &str) -> Option<RegisteredClient> {
        self.clients
            .lock()
            .expect("oauth clients lock")
            .get(client_id)
            .cloned()
    }

    pub fn contains(&self, client_id: &str) -> bool {
        self.clients
            .lock()
            .expect("oauth clients lock")
            .contains_key(client_id)
    }

    pub fn len(&self) -> usize {
        self.clients.lock().expect("oauth clients lock").len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 记一个新注册的客户端并立刻落盘。
    ///
    /// 写盘失败不影响本次注册——客户端已经拿到 client_id 了，这次授权照常能走完，
    /// 代价只是下次重启后它要重新注册。所以这里记日志而不是返回错误。
    pub fn insert(&self, client_id: String, client: RegisteredClient) {
        let snapshot = {
            let mut clients = self.clients.lock().expect("oauth clients lock");
            clients.insert(client_id, client);
            evict_oldest(&mut clients, MAX_CLIENTS);
            clients.clone()
        };
        self.persist(&snapshot);
    }

    fn persist(&self, clients: &HashMap<String, RegisteredClient>) {
        let Some(path) = self.path.as_ref() else {
            return;
        };
        if let Err(error) = write_registry(path, clients) {
            append_profile_log(
                &self.log_scope,
                "stderr.log",
                &format!(
                    "[oauth] 客户端注册表写不进 {}（{error}），本次注册只留在内存里，\
                     重启后这个客户端要重新授权。",
                    path.display()
                ),
            );
        }
    }
}

/// 超出上限时，按注册时间从旧到新删到刚好等于上限。
fn evict_oldest(clients: &mut HashMap<String, RegisteredClient>, max: usize) {
    if clients.len() <= max {
        return;
    }
    let mut by_age: Vec<(String, u64)> = clients
        .iter()
        .map(|(id, client)| (id.clone(), client.created_at))
        .collect();
    by_age.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
    for (id, _) in by_age.into_iter().take(clients.len() - max) {
        clients.remove(&id);
    }
}

fn write_registry(path: &PathBuf, clients: &HashMap<String, RegisteredClient>) -> AppResult<()> {
    if let Some(parent) = path.parent() {
        crate::home::create_data_dir(parent)?;
    }
    let text = serde_json::to_string_pretty(&RegistryFile {
        clients: clients.clone(),
    })?;
    // 和 profiles.json 一个写法：先写带 pid 的临时文件再 rename，
    // 写到一半被杀不会留下半个 JSON，两个进程也不会抢同一个 .tmp。
    let tmp = path.with_extension(format!("json.tmp.{}", std::process::id()));
    fs::write(&tmp, format!("{text}\n"))?;
    // 文件里可能有 client_secret，只允许当前用户读写。
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

/// `<数据目录>/data/oauth-clients/<作用域>.json`
fn registry_path(scope: &str) -> AppResult<PathBuf> {
    Ok(crate::home::data_home()?
        .join("data")
        .join("oauth-clients")
        .join(format!("{}.json", sanitize_scope(scope))))
}

/// 作用域直接进文件名，所以只放行字母数字和 `-_`，其余一律换成 `_`。
/// 挡的是 `../` 这种把文件写到别处去的写法。
fn sanitize_scope(scope: &str) -> String {
    let cleaned: String = scope
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "default".to_string()
    } else {
        cleaned
    }
}

/// 删工作区时一并清掉它的注册表文件。文件本来就不在也算成功。
pub fn remove_scope(scope: &str) -> AppResult<()> {
    let path = registry_path(scope)?;
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client(created_at: u64) -> RegisteredClient {
        RegisteredClient {
            redirect_uris: vec!["https://chatgpt.com/connector/oauth/cb".into()],
            token_endpoint_auth_method: "none".into(),
            client_secret: None,
            client_name: "ChatGPT".into(),
            created_at,
        }
    }

    #[test]
    fn sanitize_scope_keeps_path_inside_the_registry_dir() {
        assert_eq!(sanitize_scope("ws-abc123"), "ws-abc123");
        assert_eq!(sanitize_scope("ws-abc-actions"), "ws-abc-actions");
        assert_eq!(sanitize_scope(""), "default");
        let escaped = sanitize_scope("../../etc/passwd");
        assert!(
            !escaped.contains('/') && !escaped.contains('.'),
            "{escaped}"
        );
    }

    #[test]
    fn evict_oldest_keeps_the_newest_entries() {
        let mut clients = HashMap::new();
        for i in 0..5u64 {
            clients.insert(format!("dcr-{i}"), client(i));
        }
        evict_oldest(&mut clients, 3);
        assert_eq!(clients.len(), 3);
        assert!(clients.contains_key("dcr-4"));
        assert!(clients.contains_key("dcr-3"));
        assert!(clients.contains_key("dcr-2"));
        assert!(!clients.contains_key("dcr-0"));
    }

    /// 文件里可能有 client_secret，别人读得到就等于能冒充那个客户端。
    #[cfg(unix)]
    #[test]
    fn registry_file_is_not_readable_by_others() {
        use std::os::unix::fs::PermissionsExt;

        crate::home::isolate_for_tests();
        let scope = "permission-test";
        ClientRegistry::load(scope, scope).insert("dcr-1".into(), client(1));

        let path = registry_path(scope).expect("路径");
        let mode = fs::metadata(&path).expect("元数据").permissions().mode();
        assert_eq!(mode & 0o077, 0, "注册表不该让同组或其他人读到：{mode:o}");

        remove_scope(scope).expect("清理");
    }

    #[test]
    fn in_memory_registry_never_touches_disk() {
        let registry = ClientRegistry::in_memory();
        registry.insert("dcr-1".into(), client(1));
        assert!(registry.contains("dcr-1"));
        assert!(registry.path.is_none());
    }

    /// 这条是整件事的核心：重启（等价于重新 `load` 一次）之后客户端还在，
    /// 刷新令牌才不会被判成 invalid_client。
    #[test]
    fn registry_survives_a_reload() {
        crate::home::isolate_for_tests();
        let scope = "reload-test";
        let registry = ClientRegistry::load(scope, scope);
        registry.insert("dcr-persisted".into(), client(7));

        let reloaded = ClientRegistry::load(scope, scope);
        let found = reloaded.get("dcr-persisted").expect("客户端应当还在");
        assert_eq!(
            found.redirect_uris,
            vec!["https://chatgpt.com/connector/oauth/cb"]
        );
        assert_eq!(found.created_at, 7);
        assert!(registry_path(scope).expect("路径").exists());

        remove_scope(scope).expect("清理");
        assert!(ClientRegistry::load(scope, scope).is_empty());
    }
}
