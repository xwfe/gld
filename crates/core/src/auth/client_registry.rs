//! 动态注册（RFC 7591）拿到的客户端，落盘保存。
//!
//! ChatGPT 这类客户端会保存注册时拿到的 `dcr-…` client_id，以后继续使用。
//! 只有注册表写入成功才确认注册；并发请求不能覆盖已经确认的客户端，容量满时也
//! 不能淘汰旧 id。已有注册表读不动或损坏时保留原文件，暂时拒绝新注册。
//!
//! 服务重启后读取同一份注册表；就算文件丢了，已有签名刷新令牌仍走原来的兼容
//! 路径（见 `oauth_flow::refresh_token_exchange`），但不能据此找回未知的注册记录。

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::error::AppResult;
use crate::logs::append_profile_log;

/// 一个作用域最多接纳多少个已注册客户端；满了拒绝新注册，保留所有已发出的 id。
///
/// `/register` 不需要认证，必须限制增长；容量限制不能变成淘汰已有连接器的入口。
const MAX_CLIENTS: usize = 50;

/// 注册失败时可安全返回给 HTTP 层的原因，不携带路径、凭据或底层 I/O 错误。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RegistrationError {
    #[error("Client registration capacity has been reached; existing clients are preserved.")]
    CapacityReached,
    #[error("Client registration is temporarily unavailable.")]
    Unavailable,
}

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
    clients: HashMap<String, RegisteredClient>,
}

/// 一个作用域（一个工作区的 MCP 服务、或它的 Actions 服务）的注册表。
pub struct ClientRegistry {
    /// `None` 只用于显式内存实例，或无法定位文件时的只读降级实例。
    path: Option<PathBuf>,
    /// 定位、读取或解析失败的实例不可注册，避免覆盖未知的旧记录。
    writable: bool,
    /// 写日志用的工作区 id；只记失败，成功路径不出声。
    log_scope: String,
    clients: Mutex<HashMap<String, RegisteredClient>>,
}

impl ClientRegistry {
    /// 读取注册表。只有文件明确不存在才从可写空表开始；其他读取失败保留原文件，
    /// 让服务继续验证已有签名令牌，但拒绝新注册，等待恢复注册表后重新加载。
    pub fn load(scope: &str, log_scope: &str) -> Self {
        Self::load_path(registry_path(scope), log_scope)
    }

    fn load_path(path: AppResult<PathBuf>, log_scope: &str) -> Self {
        let path = match path {
            Ok(path) => path,
            Err(error) => {
                append_profile_log(
                    log_scope,
                    "stderr.log",
                    &format!("[oauth] 定位不到客户端注册表，新注册已停用：{error}"),
                );
                return Self {
                    path: None,
                    writable: false,
                    log_scope: log_scope.to_string(),
                    clients: Mutex::new(HashMap::new()),
                };
            }
        };
        let (clients, writable) = match fs::read_to_string(&path) {
            Ok(raw) => match serde_json::from_str::<RegistryFile>(&raw) {
                Ok(file) => (file.clients, true),
                Err(error) => {
                    append_profile_log(
                        log_scope,
                        "stderr.log",
                        &format!(
                            "[oauth] 客户端注册表 {} 解析失败（{error}），保留原文件，\
                             新注册已停用；恢复注册表后重新加载。",
                            path.display()
                        ),
                    );
                    (HashMap::new(), false)
                }
            },
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound
                    && fs::symlink_metadata(&path)
                        .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
            {
                (HashMap::new(), true)
            }
            Err(error) => {
                append_profile_log(
                    log_scope,
                    "stderr.log",
                    &format!(
                        "[oauth] 客户端注册表 {} 读不动（{error}），保留原文件，\
                         新注册已停用；恢复注册表后重新加载。",
                        path.display()
                    ),
                );
                (HashMap::new(), false)
            }
        };
        Self {
            path: Some(path),
            writable,
            log_scope: log_scope.to_string(),
            clients: Mutex::new(clients),
        }
    }

    /// 显式不落盘的注册表，给单元测试用。生产加载失败不走这条可写内存路径。
    pub fn in_memory() -> Self {
        Self {
            path: None,
            writable: true,
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

    /// 先持久化，再发布新客户端。整个过程持锁，保证较旧的并发快照不会覆盖新注册。
    /// 写入失败时内存和已有注册表不变，调用方不得返回注册成功。
    pub fn insert(
        &self,
        client_id: String,
        client: RegisteredClient,
    ) -> Result<(), RegistrationError> {
        let mut clients = self.clients.lock().expect("oauth clients lock");
        if !self.writable {
            return Err(RegistrationError::Unavailable);
        }
        if clients.len() >= MAX_CLIENTS && !clients.contains_key(&client_id) {
            return Err(RegistrationError::CapacityReached);
        }
        let mut candidate = clients.clone();
        candidate.insert(client_id, client);
        self.persist(&candidate)?;
        *clients = candidate;
        Ok(())
    }

    fn persist(
        &self,
        clients: &HashMap<String, RegisteredClient>,
    ) -> Result<(), RegistrationError> {
        let Some(path) = self.path.as_ref() else {
            return Ok(());
        };
        write_registry(path, clients).map_err(|error| {
            append_profile_log(
                &self.log_scope,
                "stderr.log",
                &format!(
                    "[oauth] 客户端注册表写不进 {}（{error}），本次注册未生效，\
                     保留已有客户端。",
                    path.display()
                ),
            );
            RegistrationError::Unavailable
        })
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
    fn capacity_rejects_new_clients_without_evicting_existing_ids() {
        let path = crate::home::test_scratch_dir("oauth-capacity").join("clients.json");
        let registry = ClientRegistry::load_path(Ok(path.clone()), "capacity-test");
        for i in 0..MAX_CLIENTS {
            registry
                .insert(format!("dcr-{i}"), client(i as u64))
                .expect("register existing client");
        }
        let before = fs::read(&path).expect("original registry");
        assert_eq!(
            registry.insert("dcr-over-capacity".into(), client(100)),
            Err(RegistrationError::CapacityReached)
        );
        assert_eq!(fs::read(&path).expect("preserved registry"), before);
        assert!(!registry.contains("dcr-over-capacity"));
        let reloaded = ClientRegistry::load_path(Ok(path), "capacity-test");
        for i in 0..MAX_CLIENTS {
            let id = format!("dcr-{i}");
            assert!(registry.contains(&id), "missing in memory: {id}");
            assert!(reloaded.contains(&id), "missing after reload: {id}");
        }
    }

    #[test]
    fn concurrent_registrations_all_survive_a_reload() {
        use std::sync::{Arc, Barrier};

        const COUNT: usize = 16;
        let path = crate::home::test_scratch_dir("oauth-concurrent").join("clients.json");
        let registry = Arc::new(ClientRegistry::load_path(
            Ok(path.clone()),
            "concurrent-test",
        ));
        registry
            .insert("dcr-existing".into(), client(0))
            .expect("existing client");
        let ready = Arc::new(Barrier::new(COUNT));
        let threads: Vec<_> = (0..COUNT)
            .map(|i| {
                let registry = registry.clone();
                let ready = ready.clone();
                std::thread::spawn(move || {
                    ready.wait();
                    registry
                        .insert(format!("dcr-concurrent-{i}"), client(i as u64 + 1))
                        .expect("successful registration must persist");
                })
            })
            .collect();
        for thread in threads {
            thread.join().expect("registration thread");
        }

        let reloaded = ClientRegistry::load_path(Ok(path), "concurrent-test");
        assert_eq!(reloaded.len(), COUNT + 1);
        assert!(reloaded.contains("dcr-existing"));
        for i in 0..COUNT {
            assert!(reloaded.contains(&format!("dcr-concurrent-{i}")));
        }
    }

    #[test]
    fn failed_persistence_preserves_memory_and_the_original_file() {
        crate::home::isolate_for_tests();
        let path = crate::home::test_scratch_dir("oauth-write-failure").join("clients.json");
        let registry = ClientRegistry::load_path(Ok(path.clone()), "write-failure-test");
        registry
            .insert("dcr-existing".into(), client(7))
            .expect("existing client");
        let before = fs::read(&path).expect("original registry");
        // 目录不可作为临时文件写入；即使用 root 跑测试也能可靠地制造 I/O 失败。
        let blocked_tmp = path.with_extension(format!("json.tmp.{}", std::process::id()));
        fs::create_dir(&blocked_tmp).expect("block temporary file");
        assert_eq!(
            registry.insert("dcr-new".into(), client(8)),
            Err(RegistrationError::Unavailable)
        );
        assert_eq!(
            registry.insert("dcr-existing".into(), client(99)),
            Err(RegistrationError::Unavailable)
        );
        assert_eq!(registry.len(), 1);
        assert!(!registry.contains("dcr-new"));
        assert_eq!(registry.get("dcr-existing").unwrap().created_at, 7);
        assert_eq!(fs::read(&path).expect("preserved registry"), before);

        fs::remove_dir(&blocked_tmp).expect("restore storage");
        registry
            .insert("dcr-retry".into(), client(9))
            .expect("registration can retry after storage recovers");
        let reloaded = ClientRegistry::load_path(Ok(path), "write-failure-test");
        assert!(reloaded.contains("dcr-existing"));
        assert!(reloaded.contains("dcr-retry"));
        assert!(!reloaded.contains("dcr-new"));
    }

    #[test]
    fn malformed_existing_registries_are_not_overwritten() {
        crate::home::isolate_for_tests();
        for (i, raw) in ["{broken", "{}", r#"{"clients":[]}"#].iter().enumerate() {
            let path = crate::home::test_scratch_dir("oauth-malformed").join("clients.json");
            fs::write(&path, raw).expect("malformed original registry");
            let registry = ClientRegistry::load_path(Ok(path.clone()), "malformed-test");
            assert_eq!(
                registry.insert(format!("dcr-new-{i}"), client(1)),
                Err(RegistrationError::Unavailable)
            );
            assert!(registry.is_empty(), "unknown records were not recovered");
            assert_eq!(fs::read_to_string(path).expect("preserved original"), *raw);
        }
    }

    #[test]
    fn unreadable_existing_registry_disables_registration() {
        crate::home::isolate_for_tests();
        let path = crate::home::test_scratch_dir("oauth-unreadable").join("clients.json");
        fs::create_dir(&path).expect("a directory cannot be read as registry JSON");
        let marker = path.join("keep");
        fs::write(&marker, "preserve").expect("original contents");
        let registry = ClientRegistry::load_path(Ok(path.clone()), "unreadable-test");
        assert_eq!(
            registry.insert("dcr-new".into(), client(1)),
            Err(RegistrationError::Unavailable)
        );
        assert!(registry.is_empty());
        assert!(path.is_dir());
        assert_eq!(
            fs::read_to_string(marker).expect("preserved contents"),
            "preserve"
        );
    }

    #[test]
    fn unresolved_registry_path_is_not_a_writable_memory_fallback() {
        crate::home::isolate_for_tests();
        let registry = ClientRegistry::load_path(
            Err(crate::error::AppError::Message(
                "unavailable registry path".into(),
            )),
            "path-failure-test",
        );
        assert_eq!(
            registry.insert("dcr-new".into(), client(1)),
            Err(RegistrationError::Unavailable)
        );
        assert!(registry.is_empty());
        assert!(registry.path.is_none());
    }

    /// 文件里可能有 client_secret，别人读得到就等于能冒充那个客户端。
    #[cfg(unix)]
    #[test]
    fn registry_file_is_not_readable_by_others() {
        use std::os::unix::fs::PermissionsExt;

        crate::home::isolate_for_tests();
        let scope = "permission-test";
        ClientRegistry::load(scope, scope)
            .insert("dcr-1".into(), client(1))
            .expect("register client");

        let path = registry_path(scope).expect("路径");
        let mode = fs::metadata(&path).expect("元数据").permissions().mode();
        assert_eq!(mode & 0o077, 0, "注册表不该让同组或其他人读到：{mode:o}");

        remove_scope(scope).expect("清理");
    }

    #[test]
    fn in_memory_registry_never_touches_disk() {
        let registry = ClientRegistry::in_memory();
        registry
            .insert("dcr-1".into(), client(1))
            .expect("register in memory");
        assert!(registry.contains("dcr-1"));
        assert!(registry.path.is_none());
    }

    /// 重新加载后保留客户端注册元数据，原 client_id 仍可重新授权；
    /// 签名 refresh token 的兼容路径另由 oauth_flow 测试覆盖。
    #[test]
    fn registry_survives_a_reload() {
        crate::home::isolate_for_tests();
        let scope = "reload-test";
        let registry = ClientRegistry::load(scope, scope);
        registry
            .insert("dcr-persisted".into(), client(7))
            .expect("persist client");

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
