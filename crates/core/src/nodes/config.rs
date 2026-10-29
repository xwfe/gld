//! M0 的显式节点登记与配置。Hub 只保存凭据摘要；邀请和凭据轮换留给后续配对流程。

use std::collections::HashSet;
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::Read;
use std::net::IpAddr;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::data::{AppData, DataStore};
use crate::error::AppError;

const MAX_CONFIG_BYTES: u64 = 64 * 1024;
const MAX_PROJECTS: usize = 256;
const AUTH_FAILED: &str = "Invalid node credentials";

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const HUB: &str = "11111111111111111111111111111111";
    const MACHINE: &str = "22222222222222222222222222222222";
    const PROJECT: &str = "33333333333333333333333333333333";
    const TOKEN: &str = "synthetic-node-credential-0000000000000001";
    const OTHER_TOKEN: &str = "synthetic-node-credential-0000000000000002";

    fn config(url: &str) -> NodeConfig {
        NodeConfig {
            hub_url: url.into(),
            hub_id: HUB.into(),
            machine_id: MACHINE.into(),
            credential: TOKEN.into(),
            workspaces: vec![PROJECT.into()],
            ca_certificate: None,
        }
    }

    #[test]
    fn registration_preserves_old_data_and_never_serializes_the_credential() {
        let mut data: AppData = serde_json::from_value(json!({})).unwrap();
        validate_registry(&data).unwrap();
        let registered = register_in(&mut data, "test-machine", Some(MACHINE), TOKEN).unwrap();
        assert!(valid_id(&registered.hub_id));
        assert_eq!(registered.machine_id, MACHINE);
        assert_eq!(
            data.node_machines[0].credential_sha256,
            credential_hash(TOKEN)
        );
        assert!(!serde_json::to_string(&data).unwrap().contains(TOKEN));
        assert!(!serde_json::to_string(&registered)
            .unwrap()
            .contains("credential"));
        assert!(!format!("{:?}", data.node_machines[0]).contains(&credential_hash(TOKEN)));
        assert!(!format!("{:?}", config("https://example.invalid")).contains(TOKEN));
        validate_registry(&data).unwrap();
    }

    #[test]
    fn duplicate_registration_does_not_change_the_existing_identity_or_projects() {
        let mut data = AppData::default();
        register_in(&mut data, "first", Some(MACHINE), TOKEN).unwrap();
        data.node_machines[0].projects.push(NodeProject {
            id: PROJECT.into(),
            name: "api".into(),
            tool_profile: "read-only".into(),
        });
        let before = serde_json::to_value(&data).unwrap();
        for (name, id, credential) in [
            ("second", MACHINE, OTHER_TOKEN),
            ("FIRST", HUB, OTHER_TOKEN),
            ("second", HUB, TOKEN),
        ] {
            assert!(register_in(&mut data, name, Some(id), credential).is_err());
            assert_eq!(serde_json::to_value(&data).unwrap(), before);
        }
    }

    #[test]
    fn node_credentials_are_distinct_from_existing_service_credentials() {
        let mut data = AppData::default();
        data.shared_secrets
            .insert("bearer_token".into(), TOKEN.into());
        assert!(register_in(&mut data, "node", Some(MACHINE), TOKEN).is_err());
        assert!(data.node_hub_id.is_empty());
        assert!(data.node_machines.is_empty());
        assert!(register_in(&mut data, "node", Some(MACHINE), OTHER_TOKEN).is_ok());
    }

    #[test]
    fn node_urls_preserve_prefixes_and_refuse_credential_or_insecure_remote_urls() {
        assert_eq!(
            config("https://example.invalid/hub")
                .websocket_url()
                .unwrap(),
            format!("wss://example.invalid/hub/nodes/v1/connect/{MACHINE}")
        );
        assert_eq!(
            config("wss://example.invalid/prefix/")
                .websocket_url()
                .unwrap(),
            format!("wss://example.invalid/prefix/nodes/v1/connect/{MACHINE}")
        );
        for url in ["http://127.0.0.1:1234/hub", "ws://[::1]:1234"] {
            config(url).validate().unwrap();
        }
        for url in [
            "http://example.invalid",
            "ws://localhost:1234",
            "ftp://example.invalid",
            "https://user:secret@example.invalid",
            "https://example.invalid?token=secret",
            "https://example.invalid/#secret",
        ] {
            assert!(config(url).validate().is_err(), "{url}");
        }
    }

    #[test]
    fn node_configuration_requires_explicit_unique_full_project_ids() {
        let mut value = config("https://example.invalid");
        value.workspaces.clear();
        assert!(value.validate().is_err());
        value.workspaces = vec![PROJECT.into(), PROJECT.into()];
        assert!(value.validate().is_err());
        value.workspaces = vec!["api".into()];
        assert!(value.validate().is_err());
        value.workspaces = vec![PROJECT.into()];
        value.credential = "short".into();
        assert!(value.validate().is_err());
        value.credential = TOKEN.into();
        value.machine_id = "m".repeat(32);
        assert!(value.validate().is_err());
    }

    #[test]
    fn configuration_file_has_bounded_redacted_reads_and_relative_ca_resolution() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("node.json");
        let mut value = config("https://example.invalid");
        value.ca_certificate = Some(PathBuf::from("ca.der"));
        let file = tempfile::NamedTempFile::new_in(dir.path()).unwrap();
        std::fs::write(file.path(), serde_json::to_vec(&value).unwrap()).unwrap();
        file.persist(&path).unwrap();
        let loaded = NodeConfig::load(&path).unwrap();
        assert_eq!(loaded.ca_certificate, Some(dir.path().join("ca.der")));
        std::fs::write(&path, format!("{{\"credential\":\"{TOKEN}\"")).unwrap();
        let error = NodeConfig::load(&path).unwrap_err();
        assert!(!error.contains(TOKEN));
        std::fs::write(&path, vec![b' '; MAX_CONFIG_BYTES as usize + 1]).unwrap();
        assert!(NodeConfig::load(&path).unwrap_err().contains("64 KiB"));
    }

    #[cfg(unix)]
    #[test]
    fn configuration_file_refuses_shared_permissions_and_symlinks() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("node.json");
        std::fs::write(
            &path,
            serde_json::to_vec(&config("https://example.invalid")).unwrap(),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(NodeConfig::load(&path).is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        NodeConfig::load(&path).unwrap();
        let alias = dir.path().join("alias.json");
        symlink(&path, &alias).unwrap();
        assert!(NodeConfig::load(&alias).is_err());
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeProject {
    pub id: String,
    pub name: String,
    pub tool_profile: String,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineRecord {
    pub id: String,
    pub name: String,
    pub credential_sha256: String,
    #[serde(default)]
    pub projects: Vec<NodeProject>,
}

impl fmt::Debug for MachineRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MachineRecord")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("projects", &self.projects)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct RegisteredMachine {
    pub hub_id: String,
    pub machine_id: String,
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct AuthenticatedMachine {
    pub hub_id: String,
    pub machine: MachineRecord,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeConfig {
    pub hub_url: String,
    pub hub_id: String,
    pub machine_id: String,
    pub credential: String,
    pub workspaces: Vec<String>,
    #[serde(default)]
    pub ca_certificate: Option<PathBuf>,
}

impl fmt::Debug for NodeConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NodeConfig")
            .field("hub_id", &self.hub_id)
            .field("machine_id", &self.machine_id)
            .field("workspaces", &self.workspaces)
            .field("credential", &"[redacted]")
            .finish_non_exhaustive()
    }
}

impl NodeConfig {
    pub fn load(path: &Path) -> Result<Self, String> {
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        }
        let file = options
            .open(path)
            .map_err(|_| "Cannot open node configuration")?;
        check_private_file(&file)?;
        let mut bytes = Vec::new();
        file.take(MAX_CONFIG_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "Cannot read node configuration")?;
        if bytes.len() as u64 > MAX_CONFIG_BYTES {
            return Err("Node configuration exceeds 64 KiB".into());
        }
        // 不附 serde 的错误正文：非法字段可能正好包含凭据。
        let mut config: Self =
            serde_json::from_slice(&bytes).map_err(|_| "Invalid node configuration JSON")?;
        if let Some(ca) = &mut config.ca_certificate {
            if ca.is_relative() {
                *ca = path.parent().unwrap_or_else(|| Path::new(".")).join(&*ca);
            }
        }
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), String> {
        if !valid_id(&self.hub_id) || !valid_id(&self.machine_id) {
            return Err(
                "Node hub_id and machine_id must be 32 lowercase hexadecimal characters".into(),
            );
        }
        validate_credential(&self.credential)?;
        validate_workspace_ids(&self.workspaces)?;
        let url = self.parsed_url()?;
        if self
            .ca_certificate
            .as_ref()
            .is_some_and(|p| p.as_os_str().is_empty())
        {
            return Err("Node CA certificate path cannot be empty".into());
        }
        if self.ca_certificate.is_some() && matches!(url.scheme(), "http" | "ws") {
            return Err("A node CA certificate requires HTTPS or WSS".into());
        }
        Ok(())
    }

    /// 基地址中的 /hub 等前缀原样保留；不从 MCP URL 裁路径。
    pub fn websocket_url(&self) -> Result<String, String> {
        let mut url = self.parsed_url()?;
        let scheme = if matches!(url.scheme(), "https" | "wss") {
            "wss"
        } else {
            "ws"
        };
        url.set_scheme(scheme)
            .map_err(|_| "Invalid node URL scheme")?;
        url.path_segments_mut()
            .map_err(|_| "Invalid node URL base path")?
            .pop_if_empty()
            .push("nodes")
            .push("v1")
            .push("connect")
            .push(&self.machine_id);
        Ok(url.into())
    }

    fn parsed_url(&self) -> Result<reqwest::Url, String> {
        let url = reqwest::Url::parse(&self.hub_url).map_err(|_| "Invalid node Hub URL")?;
        if url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(
                "Node Hub URL must have a host and no credentials, query or fragment".into(),
            );
        }
        match url.scheme() {
            "https" | "wss" => {}
            "http" | "ws"
                if url
                    .host_str()
                    .and_then(|host| host.trim_matches(['[', ']']).parse::<IpAddr>().ok())
                    .is_some_and(|ip| ip.is_loopback()) => {}
            _ => return Err(
                "Nodes require HTTPS/WSS; plain HTTP/WS is allowed only on a literal loopback IP"
                    .into(),
            ),
        }
        Ok(url)
    }
}

fn check_private_file(file: &File) -> Result<(), String> {
    let metadata = file
        .metadata()
        .map_err(|_| "Cannot inspect node configuration")?;
    if !metadata.is_file() {
        return Err("Node configuration must be a regular file".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // 只检查已打开的文件，避免在 stat 与 open 之间被替换。
        if metadata.mode() & 0o077 != 0 || metadata.uid() != unsafe { libc::geteuid() } {
            return Err("Node configuration must be owned by this account and readable only by it (chmod 600)".into());
        }
    }
    Ok(())
}

pub fn valid_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub fn validate_workspace_ids(ids: &[String]) -> Result<(), String> {
    let unique: HashSet<&str> = ids.iter().map(String::as_str).collect();
    if ids.is_empty()
        || ids.len() > MAX_PROJECTS
        || unique.len() != ids.len()
        || !ids.iter().all(|id| valid_id(id))
    {
        return Err("Node workspaces must contain 1..=256 distinct full workspace IDs".into());
    }
    Ok(())
}

pub fn validate_credential(credential: &str) -> Result<(), String> {
    if !(32..=256).contains(&credential.len())
        || !credential
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(
            "Node credential must be 32..=256 ASCII letters, digits, underscores or hyphens".into(),
        );
    }
    Ok(())
}

fn credential_hash(credential: &str) -> String {
    format!("{:x}", Sha256::digest(credential.as_bytes()))
}

fn digest_matches(expected: &str, actual: &str) -> bool {
    expected.len() == 64
        && actual.len() == 64
        && expected
            .bytes()
            .zip(actual.bytes())
            .fold(0_u8, |diff, (a, b)| diff | (a ^ b))
            == 0
}

fn valid_name(name: &str) -> bool {
    !name.trim().is_empty() && name.len() <= 128 && !name.chars().any(char::is_control)
}

fn validate_projects(projects: &[NodeProject]) -> Result<(), String> {
    let unique: HashSet<&str> = projects.iter().map(|p| p.id.as_str()).collect();
    if projects.len() > MAX_PROJECTS
        || unique.len() != projects.len()
        || projects.iter().any(|p| {
            !valid_id(&p.id)
                || p.name.trim().is_empty()
                || p.name.len() > 256
                || p.name.chars().any(char::is_control)
                || !matches!(
                    p.tool_profile.as_str(),
                    "core" | "compact" | "advanced" | "read-only"
                )
        })
    {
        return Err("Invalid node project catalog".into());
    }
    Ok(())
}

fn validate_registry(data: &AppData) -> Result<(), String> {
    // 升级前的配置既无节点也无Hub ID，不能因此破坏本地工具链。
    if data.node_hub_id.is_empty() && data.node_machines.is_empty() {
        return Ok(());
    }
    if !valid_id(&data.node_hub_id) {
        return Err("Invalid stored node Hub identity".into());
    }
    let mut ids = HashSet::new();
    let mut names = HashSet::new();
    let mut credentials = HashSet::new();
    for machine in &data.node_machines {
        if !valid_id(&machine.id)
            || !valid_name(&machine.name)
            || machine.name.contains(['/', '\\'])
            || machine.credential_sha256.len() != 64
            || !machine
                .credential_sha256
                .bytes()
                .all(|b| b.is_ascii_hexdigit())
            || !ids.insert(&machine.id)
            || !names.insert(machine.name.to_ascii_lowercase())
            || !credentials.insert(&machine.credential_sha256)
        {
            return Err("Invalid stored node registry".into());
        }
        validate_projects(&machine.projects)?;
    }
    Ok(())
}

/// 由 daemon 内的操作员入口调用；不覆盖同名、同 ID 或共用凭据的登记。
pub fn register_machine(
    name: &str,
    id: Option<&str>,
    credential: &str,
) -> Result<RegisteredMachine, String> {
    DataStore::update_file(|data| {
        register_in(data, name, id, credential).map_err(AppError::Message)
    })
    .map_err(|error| error.to_string())
}

fn register_in(
    data: &mut AppData,
    name: &str,
    id: Option<&str>,
    credential: &str,
) -> Result<RegisteredMachine, String> {
    validate_registry(data)?;
    validate_credential(credential)?;
    let name = name.trim();
    if !valid_name(name) || name.contains(['/', '\\']) {
        return Err(
            "Node name must be 1..=128 bytes without control characters or path separators".into(),
        );
    }
    let id = id
        .map(str::to_owned)
        .unwrap_or_else(|| uuid::Uuid::new_v4().simple().to_string());
    if !valid_id(&id) {
        return Err("Node machine_id must be 32 lowercase hexadecimal characters".into());
    }
    let hash = credential_hash(credential);
    if data.node_machines.iter().any(|m| {
        m.id == id
            || m.name.eq_ignore_ascii_case(name)
            || digest_matches(&m.credential_sha256, &hash)
    }) {
        return Err("Node ID, name or credential is already registered; existing registration was not changed".into());
    }
    let shared = data
        .shared_secrets
        .values()
        .chain(data.app_secrets.values().flat_map(|m| m.values()))
        .chain(data.workspace_secrets.values().flat_map(|m| m.values()));
    if shared
        .into_iter()
        .any(|s| !s.is_empty() && digest_matches(&credential_hash(s), &hash))
        || data.grants.iter().any(|g| {
            [&g.bearer_token, &g.oauth_password]
                .iter()
                .any(|s| !s.is_empty() && digest_matches(&credential_hash(s), &hash))
        })
    {
        return Err(
            "Node credential must be separate from existing service and grant credentials".into(),
        );
    }
    if data.node_hub_id.is_empty() {
        data.node_hub_id = uuid::Uuid::new_v4().simple().to_string();
    }
    data.node_machines.push(MachineRecord {
        id: id.clone(),
        name: name.into(),
        credential_sha256: hash,
        projects: Vec::new(),
    });
    Ok(RegisteredMachine {
        hub_id: data.node_hub_id.clone(),
        machine_id: id,
        name: name.into(),
    })
}

pub fn snapshot() -> Result<(String, Vec<MachineRecord>), String> {
    DataStore::read_file(|data| {
        validate_registry(data).map_err(AppError::Message)?;
        Ok((data.node_hub_id.clone(), data.node_machines.clone()))
    })
    .map_err(|_| "Node registry is unavailable".into())
}

pub fn authenticate(machine_id: &str, credential: &str) -> Result<AuthenticatedMachine, String> {
    if !valid_id(machine_id) || validate_credential(credential).is_err() {
        return Err(AUTH_FAILED.into());
    }
    let (hub_id, machines) = snapshot().map_err(|_| AUTH_FAILED)?;
    if !valid_id(&hub_id) {
        return Err(AUTH_FAILED.into());
    }
    let hash = credential_hash(credential);
    let machine = machines
        .into_iter()
        .find(|m| m.id == machine_id && digest_matches(&m.credential_sha256, &hash))
        .ok_or(AUTH_FAILED)?;
    Ok(AuthenticatedMachine { hub_id, machine })
}

pub fn update_projects(machine_id: &str, projects: &[NodeProject]) -> Result<(), String> {
    validate_projects(projects)?;
    DataStore::update_file(|data| {
        validate_registry(data).map_err(AppError::Message)?;
        let machine = data
            .node_machines
            .iter_mut()
            .find(|m| m.id == machine_id)
            .ok_or_else(|| AppError::Message("Node registration no longer exists".into()))?;
        machine.projects = projects.to_vec();
        Ok(())
    })
    .map_err(|_| "Cannot save node project catalog".into())
}
