//! 显式节点登记与凭据生命周期。Hub 只保存凭据摘要，撤销和轮换保留机器身份及项目目录。

use std::collections::HashSet;
use std::fmt;
use std::net::IpAddr;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::data::{AppData, DataStore};
use crate::error::AppError;

use super::hub_key::HubKey;

const MAX_PROJECTS: usize = 256;
pub(super) const MAX_CONFIG_BYTES: u64 = 64 * 1024;
const AUTH_FAILED: &str = "Invalid node credentials";

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;

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
    /// 防止凭据 A → B → A 后，A 的旧握手快照重新生效。
    #[serde(default)]
    pub credential_generation: u64,
    #[serde(default)]
    pub revoked: bool,
    #[serde(default)]
    pub projects: Vec<NodeProject>,
}

impl fmt::Debug for MachineRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MachineRecord")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("credential_generation", &self.credential_generation)
            .field("revoked", &self.revoked)
            .field("projects", &self.projects)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisteredMachine {
    pub hub_id: String,
    /// 节点配置的 hub_public_key；目标机用它核验每次握手和 Hub 发来的每条消息。
    pub hub_public_key: String,
    pub machine_id: String,
    pub name: String,
    pub revoked: bool,
}

#[derive(Debug, Clone)]
pub struct AuthenticatedMachine {
    pub hub_id: String,
    pub machine: MachineRecord,
}

/// 只有目标机显式 `--run` 的项目才开放的工具。`read_output`、`list_runs` 只读自己的记录，
/// 已在只读工具表里；`exec_health_check` 不算：跑命令的 AI 用 `check_exec_environment` 就够。
pub fn runs_commands(name: &str) -> bool {
    matches!(name, "exec_command" | "write_stdin" | "kill_session")
}

/// 目标机操作员放开的范围，一级包一级：共享（读）⊃ 文件写入 ⊃ 跑命令。
/// 能跑命令就能改任何文件，所以不设“能跑命令但不能写”这一档，否则看着比实际安全。
#[derive(Clone, Debug, Default)]
pub struct NodeAccess {
    pub shared: Vec<String>,
    pub writable: Vec<String>,
    pub runnable: Vec<String>,
}

impl NodeAccess {
    pub fn validate(&self) -> Result<(), String> {
        validate_workspace_ids(&self.shared)?;
        let mut writable = HashSet::new();
        if self
            .writable
            .iter()
            .any(|id| !self.shared.contains(id) || !writable.insert(id))
        {
            return Err(
                "Node writable_workspaces must contain distinct IDs from workspaces".into(),
            );
        }
        let mut runnable = HashSet::new();
        if self
            .runnable
            .iter()
            .any(|id| !self.writable.contains(id) || !runnable.insert(id))
        {
            return Err(
                "Node runnable_workspaces must contain distinct IDs from writable_workspaces"
                    .into(),
            );
        }
        Ok(())
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeConfig {
    pub hub_url: String,
    pub hub_id: String,
    /// 配对时钉住的 Hub 公钥。只认 TLS 证书时，能为该域名签出证书的人就能冒充 Hub（RFC-0013 §7.3）。
    pub hub_public_key: String,
    pub machine_id: String,
    pub credential: String,
    pub workspaces: Vec<String>,
    /// 本机操作员明确允许文件写入的共享项目；缺省保持只读，不授予命令或管理能力。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub writable_workspaces: Vec<String>,
    /// 本机操作员明确允许跑命令的项目，必须也在 `writable_workspaces` 里。命令以这个账号的身份跑，
    /// 不是沙箱：能调用 Hub 的人等于拿到这个账号。缺省不开。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runnable_workspaces: Vec<String>,
    #[serde(default)]
    pub ca_certificate: Option<PathBuf>,
}

impl fmt::Debug for NodeConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NodeConfig")
            .field("hub_id", &self.hub_id)
            .field("machine_id", &self.machine_id)
            .field("workspaces", &self.workspaces)
            .field("writable_workspaces", &self.writable_workspaces)
            .field("runnable_workspaces", &self.runnable_workspaces)
            .field("credential", &"[redacted]")
            .finish_non_exhaustive()
    }
}

impl NodeConfig {
    pub fn load(path: &Path) -> Result<Self, String> {
        super::local_config::read_config(path)
    }

    pub fn access(&self) -> NodeAccess {
        NodeAccess {
            shared: self.workspaces.clone(),
            writable: self.writable_workspaces.clone(),
            runnable: self.runnable_workspaces.clone(),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if !valid_id(&self.hub_id) || !valid_id(&self.machine_id) {
            return Err(
                "Node hub_id and machine_id must be 32 lowercase hexadecimal characters".into(),
            );
        }
        if !super::hub_key::valid_public_key(&self.hub_public_key) {
            return Err("Node hub_public_key must be the 64-character lowercase hexadecimal key from the invitation or Hub registration".into());
        }
        validate_credential(&self.credential)?;
        self.access().validate()?;
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
        parse_hub_url(&self.hub_url)
    }
}

/// 节点能接受的 Hub 地址。发邀请时也查一遍：等到目标机上才报错，邀请和时间都白费了。
pub fn parse_hub_url(hub_url: &str) -> Result<reqwest::Url, String> {
    let url = reqwest::Url::parse(hub_url).map_err(|_| "Invalid node Hub URL")?;
    if url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("Node Hub URL must have a host and no credentials, query or fragment".into());
    }
    match url.scheme() {
        "https" | "wss" => {}
        "http" | "ws"
            if url
                .host_str()
                .and_then(|host| host.trim_matches(['[', ']']).parse::<IpAddr>().ok())
                .is_some_and(|ip| ip.is_loopback()) => {}
        _ => {
            return Err(
                "Nodes require HTTPS/WSS; plain HTTP/WS is allowed only on a literal loopback IP"
                    .into(),
            )
        }
    }
    Ok(url)
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

pub(super) fn credential_hash(credential: &str) -> String {
    format!("{:x}", Sha256::digest(credential.as_bytes()))
}

pub(super) fn digest_matches(expected: &str, actual: &str) -> bool {
    expected.len() == 64
        && actual.len() == 64
        && expected
            .bytes()
            .zip(actual.bytes())
            .fold(0_u8, |diff, (a, b)| diff | (a ^ b))
            == 0
}

pub(super) fn valid_name(name: &str) -> bool {
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
                    "core" | "compact" | "advanced" | "read-only" | "file-write" | "commands"
                )
        })
    {
        return Err("Invalid node project catalog".into());
    }
    Ok(())
}

pub(super) fn validate_registry(data: &AppData) -> Result<(), String> {
    // 升级前的配置既无节点也无Hub ID，不能因此破坏本地工具链。
    if data.node_hub_id.is_empty() && data.node_machines.is_empty() {
        return Ok(());
    }
    if !valid_id(&data.node_hub_id) {
        return Err("Invalid stored node Hub identity".into());
    }
    // 空私钥只出现在早期开发数据里；下次登记、邀请、撤销或轮换时补上，补上前节点连不上。
    if !data.node_hub_key.is_empty() {
        HubKey::from_stored(&data.node_hub_key)?;
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

pub(super) fn register_in(
    data: &mut AppData,
    name: &str,
    id: Option<&str>,
    credential: &str,
) -> Result<RegisteredMachine, String> {
    validate_registry(data)?;
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
    if data
        .node_machines
        .iter()
        .any(|m| m.id == id || m.name.eq_ignore_ascii_case(name))
    {
        return Err(
            "Node ID or name is already registered; existing registration was not changed".into(),
        );
    }
    let hash = unused_credential_hash(data, credential)?;
    let hub_public_key = ensure_hub_identity(data)?;
    data.node_machines.push(MachineRecord {
        id: id.clone(),
        name: name.into(),
        credential_sha256: hash,
        credential_generation: 0,
        revoked: false,
        projects: Vec::new(),
    });
    Ok(RegisteredMachine {
        hub_id: data.node_hub_id.clone(),
        hub_public_key,
        machine_id: id,
        name: name.into(),
        revoked: false,
    })
}

/// Hub ID 与签名私钥一起生成；返回节点要钉住的公钥。
pub(super) fn ensure_hub_identity(data: &mut AppData) -> Result<String, String> {
    if data.node_hub_id.is_empty() {
        data.node_hub_id = uuid::Uuid::new_v4().simple().to_string();
    }
    if data.node_hub_key.is_empty() {
        data.node_hub_key = super::hub_key::generate_private_key()?;
    }
    hub_public_key(data)
}

pub(super) fn hub_public_key(data: &AppData) -> Result<String, String> {
    HubKey::from_stored(&data.node_hub_key).map(|key| key.public_key())
}

/// 握手时取一次；私钥只在 Hub 进程内存里，不进日志、响应或节点配置。
pub fn signing_key() -> Result<HubKey, String> {
    DataStore::read_file(|data| {
        validate_registry(data).map_err(AppError::Message)?;
        HubKey::from_stored(&data.node_hub_key).map_err(AppError::Message)
    })
    .map_err(|_| "Node Hub key is unavailable".into())
}

/// 注册和轮换共用检查；已撤销记录的原凭据也不能分配给别的节点。
fn unused_credential_hash(data: &AppData, credential: &str) -> Result<String, String> {
    validate_credential(credential)?;
    let hash = credential_hash(credential);
    if data
        .node_machines
        .iter()
        .any(|m| digest_matches(&m.credential_sha256, &hash))
    {
        return Err(
            "Node credential is already registered; existing registration was not changed".into(),
        );
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
    Ok(hash)
}

/// 先使凭据持久失效；调用方随后关闭正在使用旧认证的连接。
pub fn revoke_machine(machine_id: &str) -> Result<RegisteredMachine, String> {
    DataStore::update_file(|data| revoke_in(data, machine_id).map_err(AppError::Message))
        .map_err(|error| error.to_string())
}

fn revoke_in(data: &mut AppData, machine_id: &str) -> Result<RegisteredMachine, String> {
    validate_registry(data)?;
    let index = machine_index(data, machine_id)?;
    let hub_public_key = ensure_hub_identity(data)?;
    let machine = &mut data.node_machines[index];
    if !machine.revoked {
        machine.credential_generation = next_generation(machine)?;
        machine.revoked = true;
    }
    Ok(RegisteredMachine {
        hub_id: data.node_hub_id.clone(),
        hub_public_key,
        machine_id: machine.id.clone(),
        name: machine.name.clone(),
        revoked: true,
    })
}

/// 原子更换专用凭据并恢复接入；稳定身份和项目目录保持不变。
pub fn rotate_credential(machine_id: &str, credential: &str) -> Result<RegisteredMachine, String> {
    DataStore::update_file(|data| {
        rotate_in(data, machine_id, credential).map_err(AppError::Message)
    })
    .map_err(|error| error.to_string())
}

fn rotate_in(
    data: &mut AppData,
    machine_id: &str,
    credential: &str,
) -> Result<RegisteredMachine, String> {
    validate_registry(data)?;
    let index = machine_index(data, machine_id)?;
    let hash = unused_credential_hash(data, credential)?;
    let hub_public_key = ensure_hub_identity(data)?;
    let machine = &mut data.node_machines[index];
    let generation = next_generation(machine)?;
    machine.credential_sha256 = hash;
    machine.credential_generation = generation;
    machine.revoked = false;
    Ok(RegisteredMachine {
        hub_id: data.node_hub_id.clone(),
        hub_public_key,
        machine_id: machine.id.clone(),
        name: machine.name.clone(),
        revoked: false,
    })
}

fn machine_index(data: &AppData, machine_id: &str) -> Result<usize, String> {
    if !valid_id(machine_id) {
        return Err("Node machine_id must be 32 lowercase hexadecimal characters".into());
    }
    data.node_machines
        .iter()
        .position(|machine| machine.id == machine_id)
        .ok_or_else(|| "Node registration does not exist".into())
}

fn next_generation(machine: &MachineRecord) -> Result<u64, String> {
    machine
        .credential_generation
        .checked_add(1)
        .ok_or_else(|| "Node credential generation is exhausted".into())
}

pub fn snapshot() -> Result<(String, Vec<MachineRecord>), String> {
    DataStore::read_file(|data| {
        validate_registry(data).map_err(AppError::Message)?;
        Ok((data.node_hub_id.clone(), data.node_machines.clone()))
    })
    .map_err(|_| "Node registry is unavailable".into())
}

pub fn authenticate(machine_id: &str, credential: &str) -> Result<AuthenticatedMachine, String> {
    DataStore::read_file(|data| {
        authenticate_in(data, machine_id, credential).map_err(AppError::Message)
    })
    .map_err(|_| AUTH_FAILED.into())
}

fn authenticate_in(
    data: &AppData,
    machine_id: &str,
    credential: &str,
) -> Result<AuthenticatedMachine, String> {
    if !valid_id(machine_id) || validate_credential(credential).is_err() {
        return Err(AUTH_FAILED.into());
    }
    validate_registry(data).map_err(|_| AUTH_FAILED)?;
    if !valid_id(&data.node_hub_id) {
        return Err(AUTH_FAILED.into());
    }
    let hash = credential_hash(credential);
    let machine = data
        .node_machines
        .iter()
        .find(|m| m.id == machine_id && !m.revoked && digest_matches(&m.credential_sha256, &hash))
        .ok_or(AUTH_FAILED)?;
    Ok(AuthenticatedMachine {
        hub_id: data.node_hub_id.clone(),
        machine: machine.clone(),
    })
}

/// Upgrade 名额登记和关闭旧连接时复核，不能把握手前拍下的认证永久当有效。
pub fn is_current(authenticated: &AuthenticatedMachine) -> Result<bool, String> {
    DataStore::read_file(|data| {
        validate_registry(data).map_err(AppError::Message)?;
        Ok(is_current_in(data, authenticated))
    })
    .map_err(|_| "Node registry is unavailable".into())
}

fn is_current_in(data: &AppData, authenticated: &AuthenticatedMachine) -> bool {
    data.node_hub_id == authenticated.hub_id
        && data.node_machines.iter().any(|machine| {
            machine.id == authenticated.machine.id
                && !machine.revoked
                && machine.credential_generation == authenticated.machine.credential_generation
                && digest_matches(
                    &machine.credential_sha256,
                    &authenticated.machine.credential_sha256,
                )
        })
}

pub fn update_projects(
    authenticated: &AuthenticatedMachine,
    projects: &[NodeProject],
) -> Result<(), String> {
    DataStore::update_file(|data| {
        update_projects_in(data, authenticated, projects).map_err(AppError::Message)
    })
    .map_err(|_| "Cannot save node project catalog".into())
}

fn update_projects_in(
    data: &mut AppData,
    authenticated: &AuthenticatedMachine,
    projects: &[NodeProject],
) -> Result<(), String> {
    validate_registry(data)?;
    validate_projects(projects)?;
    // 与目录落盘持同一把数据锁；旧握手不能覆盖撤销或轮换之后的新目录。
    if !is_current_in(data, authenticated) {
        return Err("Node credentials changed before discovery completed".into());
    }
    let index = machine_index(data, &authenticated.machine.id)?;
    data.node_machines[index].projects = projects.to_vec();
    Ok(())
}
