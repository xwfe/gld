//! 每台机器只保留一条已认证连接，项目句柄绑定机器与目标项目。
//! 网络等待不持有登记表的锁；断线后不重放调用。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::auth::{AuthContext, Principal};
use crate::bridge::peer::PeerError;
use crate::bridge::session::Offered;
use crate::tools::workspace::{tool_err, WorkspaceError};

use super::config::{self, AuthenticatedMachine, NodeProject};
use super::connection::{Connection, ConnectionError};
use super::hub_key::MessageSigner;
use super::identity::forwarded_meta;

const HANDSHAKE: Duration = Duration::from_secs(10);
/// 工具自己最多等 30 秒（`exec_command` 的 yield 上限），回包在路上再给 30 秒。超时等于断线：
/// 连接被关、节点重连、调用方拿到“结果不明”，所以必须比工具等待长，慢网下宁可多等。
/// 这是上限，调用一回包就返回。不能超过节点的 `IDLE_TIMEOUT`：调用期间 Hub 不发 Ping，
/// 节点那么久收不到任何帧就会自己断开。
pub(super) const CALL: Duration = Duration::from_secs(60);

#[derive(Clone)]
pub struct NativeMember {
    pub id: String,
    pub name: String,
    pub machine_id: String,
    pub remote_workspace_id: String,
    pub online: bool,
    pub revoked: bool,
    pub tool_profile: String,
}

impl NativeMember {
    pub fn listed(&self) -> Value {
        json!({
            "id": self.id, "name": self.name, "kind": "node",
            "machine_id": self.machine_id, "online": self.online, "revoked": self.revoked,
            "tool_profile": self.tool_profile, "read_only": self.tool_profile != "file-write"
        })
    }
}

struct Entry {
    generation: String,
    authenticated: AuthenticatedMachine,
    connection: Option<Arc<Connection>>,
    offered: Option<Offered>,
    retired: bool,
}

#[derive(Default)]
pub struct Nodes {
    entries: Mutex<HashMap<String, Entry>>,
    stopped: AtomicBool,
}

/// 先占名额再 Upgrade；握手失败或连接结束时只释放自己的代次。
pub struct Reservation {
    nodes: Weak<Nodes>,
    machine_id: String,
    generation: String,
}

impl Nodes {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reserve(
        self: &Arc<Self>,
        authenticated: AuthenticatedMachine,
    ) -> Result<Reservation, &'static str> {
        let id = authenticated.machine.id.clone();
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        if self.stopped.load(Ordering::Acquire) {
            return Err("HUB_STOPPING");
        }
        // 鉴权到占名额之间可能发生轮换；在同一登记锁下重新检查当前代次。
        if !config::is_current(&authenticated).unwrap_or(false) {
            return Err("INVALID_NODE_CREDENTIALS");
        }
        if let Some(entry) = entries.get(&id) {
            if !(entry.retired && entry.connection.is_none())
                && !entry.connection.as_ref().is_some_and(|c| c.is_closed())
            {
                return Err("MACHINE_ALREADY_CONNECTED");
            }
        }
        let generation = uuid::Uuid::new_v4().simple().to_string();
        entries.insert(
            id.clone(),
            Entry {
                generation: generation.clone(),
                authenticated,
                connection: None,
                offered: None,
                retired: false,
            },
        );
        Ok(Reservation {
            nodes: Arc::downgrade(self),
            machine_id: id,
            generation,
        })
    }

    pub fn members(&self) -> Result<Vec<NativeMember>, String> {
        let (hub_id, machines) = config::snapshot()?;
        let entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let mut members = Vec::new();
        for machine in machines {
            let online = entries.get(&machine.id).is_some_and(|entry| {
                !machine.revoked
                    && !entry.retired
                    && entry.authenticated.hub_id == hub_id
                    && entry.authenticated.machine.credential_sha256 == machine.credential_sha256
                    && entry.authenticated.machine.credential_generation
                        == machine.credential_generation
                    && entry.offered.is_some()
                    && entry.connection.as_ref().is_some_and(|c| !c.is_closed())
            });
            for project in &machine.projects {
                members.push(NativeMember {
                    id: workspace_handle(&machine.id, &project.id),
                    name: format!("{}/{}", machine.name, project.name),
                    machine_id: machine.id.clone(),
                    remote_workspace_id: project.id.clone(),
                    online,
                    revoked: machine.revoked,
                    tool_profile: project.tool_profile.clone(),
                });
            }
        }
        Ok(members)
    }

    pub fn call(
        &self,
        auth: &AuthContext,
        member: &NativeMember,
        name: &str,
        mut args: Value,
    ) -> Value {
        if !auth.is_authenticated() || auth.grant().is_some() {
            return failure(
                "NODE_AUTH_REQUIRED",
                "Native nodes require full service authentication.",
                false,
            );
        }
        let (hub_id, machines) = match config::snapshot() {
            Ok(snapshot) => snapshot,
            Err(_) => {
                return failure(
                    "NODE_REGISTRY_UNAVAILABLE",
                    "Cannot read the node registry.",
                    true,
                )
            }
        };
        if machines
            .iter()
            .any(|m| m.id == member.machine_id && m.revoked)
        {
            return failure(
                "MACHINE_REVOKED",
                "This machine credential was revoked; nothing was sent.",
                false,
            );
        }
        let live = {
            let entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
            entries.get(&member.machine_id).and_then(|entry| {
                let registered = machines.iter().any(|m| {
                    m.id == member.machine_id
                        && !m.revoked
                        && m.credential_sha256 == entry.authenticated.machine.credential_sha256
                        && m.credential_generation
                            == entry.authenticated.machine.credential_generation
                        && m.projects
                            .iter()
                            .any(|p| p.id == member.remote_workspace_id)
                });
                if entry.retired || entry.authenticated.hub_id != hub_id || !registered {
                    return None;
                }
                Some((entry.connection.clone()?, entry.offered.clone()?))
            })
        };
        let Some((connection, offered)) = live else {
            return failure(
                "MACHINE_OFFLINE",
                "The selected machine is offline; nothing was sent.",
                true,
            );
        };
        if connection.is_closed() {
            return failure(
                "MACHINE_OFFLINE",
                "The selected machine is offline; nothing was sent.",
                true,
            );
        }
        let Some(arguments) = args.as_object_mut() else {
            return failure(
                "INVALID_ARGUMENT",
                "Tool arguments must be an object.",
                false,
            );
        };
        arguments.insert("workspace".into(), json!(member.remote_workspace_id));
        if let Err(error) = offered.check(name, &args) {
            return peer_failure(error);
        }
        let meta = match forwarded_meta(auth, &hub_id) {
            Ok(meta) => meta,
            Err(_) => {
                return failure(
                    "NODE_AUTH_REQUIRED",
                    "Native nodes require full service authentication.",
                    false,
                )
            }
        };
        match connection.try_with_peer(|peer| peer.call_tool_with_meta(name, args, meta, CALL)) {
            Ok(result) => tag_result(result, member),
            Err(ConnectionError::Busy) => failure(
                "MACHINE_BUSY",
                "This machine connection is busy; this request was not sent.",
                true,
            ),
            Err(ConnectionError::Closed) => failure(
                "MACHINE_OFFLINE",
                "The selected machine is offline; nothing was sent.",
                true,
            ),
            Err(ConnectionError::Peer(error)) => peer_failure(error),
        }
    }

    /// 持久凭据变更后收回失效连接。锁内读当前代次，避免较早的清理误关新连接。
    pub fn close_stale(&self, machine_id: &str) -> Result<(), String> {
        let connection = {
            let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
            let Some(entry) = entries.get_mut(machine_id) else {
                return Ok(());
            };
            if config::is_current(&entry.authenticated)? {
                return Ok(());
            }
            entry.retired = true;
            entry.connection.clone()
        };
        // 实际 socket 关闭后才可复用名额；不让该机器的有界调用占登记表锁。
        if let Some(connection) = connection {
            connection.close();
        }
        Ok(())
    }

    pub fn close_all(&self) {
        self.stopped.store(true, Ordering::Release);
        let connections: Vec<_> = self
            .entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .filter_map(|entry| entry.connection.clone())
            .collect();
        for connection in connections {
            connection.close();
        }
    }
}

impl Reservation {
    pub async fn accept(self, socket: axum::extract::ws::WebSocket, signer: MessageSigner) {
        let connection = Connection::new(socket, signer);
        let Some(nodes) = self.nodes.upgrade() else {
            let _ = tokio::task::spawn_blocking(move || connection.close()).await;
            return;
        };
        let attached = {
            let mut entries = nodes.entries.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(entry) = entries.get_mut(&self.machine_id).filter(|entry| {
                entry.generation == self.generation
                    && !entry.retired
                    && !nodes.stopped.load(Ordering::Acquire)
            }) {
                entry.connection = Some(connection.clone());
                true
            } else {
                false
            }
        };
        if !attached {
            let _ = tokio::task::spawn_blocking(move || connection.close()).await;
            return;
        }
        let initialized = {
            let connection = connection.clone();
            let nodes = nodes.clone();
            let machine_id = self.machine_id.clone();
            let generation = self.generation.clone();
            tokio::task::spawn_blocking(move || {
                nodes.initialize(&machine_id, &generation, &connection)
            })
            .await
        };
        if !matches!(initialized, Ok(Ok(()))) {
            let closing = connection.clone();
            let _ = tokio::task::spawn_blocking(move || closing.close()).await;
            return;
        }
        connection.monitor().await;
    }
}

impl Nodes {
    fn initialize(
        &self,
        machine_id: &str,
        generation: &str,
        connection: &Arc<Connection>,
    ) -> Result<(), String> {
        let authenticated = {
            let entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
            entries
                .get(machine_id)
                .filter(|e| e.generation == generation && !e.retired)
                .ok_or("node connection replaced")?
                .authenticated
                .clone()
        };
        let discovery = AuthContext::new(Principal::SharedSecret, "node-inventory");
        let meta = forwarded_meta(&discovery, &authenticated.hub_id)?;
        let (offered, projects) = connection
            .try_with_peer(|peer| {
                peer.initialize("gld-hub", HANDSHAKE)?;
                let tools = peer.list_tools(HANDSHAKE)?;
                let offered = Offered::from_handshake(&tools, peer.server_info());
                offered.check("list_workspaces", &json!({}))?;
                let result =
                    peer.call_tool_with_meta("list_workspaces", json!({}), meta, HANDSHAKE)?;
                let projects = parse_projects(&result).map_err(PeerError::Handshake)?;
                Ok((offered, projects))
            })
            .map_err(|_| "node initialization failed".to_string())?;
        config::update_projects(&authenticated, &projects)?;
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let entry = entries
            .get_mut(machine_id)
            .filter(|e| e.generation == generation && !e.retired)
            .ok_or("node connection replaced")?;
        entry.offered = Some(offered);
        Ok(())
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        if let Some(nodes) = self.nodes.upgrade() {
            let mut entries = nodes.entries.lock().unwrap_or_else(|e| e.into_inner());
            if entries
                .get(&self.machine_id)
                .is_some_and(|e| e.generation == self.generation)
            {
                entries.remove(&self.machine_id);
            }
        }
    }
}

fn parse_projects(result: &Value) -> Result<Vec<NodeProject>, String> {
    if result["isError"] == true || result["structuredContent"]["ok"] != true {
        return Err("node project discovery failed".into());
    }
    let items = result["structuredContent"]["workspaces"]
        .as_array()
        .filter(|items| items.len() <= 256)
        .ok_or("invalid node project list")?;
    let mut projects = Vec::new();
    for item in items {
        if item["kind"] != "local" {
            return Err("a node may share only its own local projects".into());
        }
        let id = item["id"].as_str().ok_or("node project id missing")?;
        let name = item["name"].as_str().ok_or("node project name missing")?;
        if id.len() != 32
            || !id
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || name.is_empty()
            || name.len() > 256
            || name.chars().any(char::is_control)
            || projects.iter().any(|p: &NodeProject| p.id == id)
        {
            return Err("invalid or duplicate node project".into());
        }
        projects.push(NodeProject {
            id: id.into(),
            name: name.into(),
            tool_profile: match item.get("read_only") {
                // 老节点没有显式文件写授权字段；其本机 profile 不能当成远端写权限。
                None | Some(Value::Bool(true)) => "read-only",
                Some(Value::Bool(false)) => "file-write",
                Some(_) => return Err("invalid node project read_only flag".into()),
            }
            .into(),
        });
    }
    Ok(projects)
}

pub fn workspace_handle(machine_id: &str, workspace_id: &str) -> String {
    let encoded = serde_json::to_vec(&(machine_id, workspace_id)).expect("string pair");
    let digest = format!("{:x}", Sha256::digest(encoded));
    format!("n_{}", &digest[..32])
}

fn tag_result(mut result: Value, member: &NativeMember) -> Value {
    let old = result.get("structuredContent").cloned();
    if let Some(structured) = result
        .get_mut("structuredContent")
        .and_then(Value::as_object_mut)
    {
        structured.insert("hub_workspace".into(), member.listed());
    }
    // 只同步 structuredContent 的等值 JSON 文本副本，不重写远端正文或图片。
    let updated = result.get("structuredContent").cloned();
    if let (Some(old), Some(updated), Some(content)) = (
        old,
        updated,
        result.get_mut("content").and_then(Value::as_array_mut),
    ) {
        for block in content {
            if block["type"] == "text"
                && block["text"].as_str().is_some_and(|text| {
                    serde_json::from_str::<Value>(text).is_ok_and(|value| value == old)
                })
            {
                block["text"] = json!(updated.to_string());
            }
        }
    }
    if let Some(object) = result.as_object_mut() {
        let meta = object.entry("_meta").or_insert_with(|| json!({}));
        if !meta.is_object() {
            *meta = json!({});
        }
        meta["gld/workspace"] = member.listed();
    }
    result
}

fn failure(code: &'static str, message: &str, retryable: bool) -> Value {
    crate::tools::workspace::wrap_tool_result(tool_err(WorkspaceError::Tool {
        code,
        message: message.into(),
        category: "runtime",
        retryable,
    }))
}

fn peer_failure(error: PeerError) -> Value {
    match error {
        PeerError::Unsupported { argument: Some(_), .. } =>
            failure("NODE_ARGUMENT_UNSUPPORTED", "This node does not accept one of the supplied arguments; nothing was sent.", false),
        PeerError::Unsupported { .. } =>
            failure("NODE_TOOL_UNSUPPORTED", "This node does not offer this tool; nothing was sent.", false),
        PeerError::Remote { .. } =>
            failure("NODE_REQUEST_REJECTED", "The node rejected the JSON-RPC request.", false),
        PeerError::Write(error) if error.kind() == std::io::ErrorKind::InvalidInput =>
            failure("NODE_REQUEST_TOO_LARGE", "The request exceeds the node message limit; nothing was sent.", false),
        PeerError::Write(error) if error.kind() == std::io::ErrorKind::NotConnected =>
            failure("MACHINE_OFFLINE", "The selected machine is offline; nothing was sent.", true),
        _ => failure("NODE_OUTCOME_UNKNOWN", "The connection failed before a valid reply arrived. The outcome is unknown; the request was not replayed.", false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_handles_are_stable_and_machine_scoped() {
        assert_eq!(
            workspace_handle("one", "api"),
            workspace_handle("one", "api")
        );
        assert_ne!(
            workspace_handle("one", "api"),
            workspace_handle("two", "api")
        );
        assert_ne!(workspace_handle("a:b", "c"), workspace_handle("a", "b:c"));
        assert!(workspace_handle("one", "api").starts_with("n_"));
    }

    #[test]
    fn discovery_rejects_duplicate_or_nonlocal_projects() {
        let local = json!({"id":"a".repeat(32),"name":"api","kind":"local"});
        let wrap = |items: Value| json!({"structuredContent":{"ok":true,"workspaces":items}});
        assert!(parse_projects(&wrap(json!([local.clone()]))).is_ok());
        assert!(parse_projects(&wrap(json!([local.clone(), local]))).is_err());
        assert!(parse_projects(&wrap(
            json!([{"id":"b".repeat(32),"name":"api","kind":"node"}])
        ))
        .is_err());
        assert!(parse_projects(&json!({"isError":true})).is_err());
    }

    #[test]
    fn discovery_defaults_legacy_profiles_to_read_only_and_requires_boolean_permission() {
        let mut local =
            json!({"id":"a".repeat(32),"name":"api","kind":"local","tool_profile":"advanced"});
        let discover = |item: &Value| {
            parse_projects(&json!({"structuredContent":{"ok":true,"workspaces":[item]}}))
        };
        assert_eq!(discover(&local).unwrap()[0].tool_profile, "read-only");
        local["read_only"] = json!(false);
        assert_eq!(discover(&local).unwrap()[0].tool_profile, "file-write");
        local["read_only"] = json!(true);
        assert_eq!(discover(&local).unwrap()[0].tool_profile, "read-only");
        for invalid in [json!("false"), json!(0), Value::Null, json!({})] {
            local["read_only"] = invalid;
            assert!(discover(&local).is_err());
        }
    }

    #[test]
    fn routed_result_keeps_images_and_errors_and_replaces_only_the_workspace_tag() {
        let member = NativeMember {
            id: "n_hub".into(),
            name: "machine/api".into(),
            machine_id: "machine".into(),
            remote_workspace_id: "target".into(),
            online: true,
            revoked: false,
            tool_profile: "read-only".into(),
        };
        let structured =
            json!({"ok":false,"hub_workspace":{"id":"target"},"error":{"code":"OUTSIDE"}});
        let image = json!({"type":"image","data":"eA==","mimeType":"image/png"});
        let result = tag_result(
            json!({
                "structuredContent":structured,
                "content":[{"type":"text","text":structured.to_string()},image],
                "isError":true,
            }),
            &member,
        );
        assert_eq!(result["structuredContent"]["hub_workspace"]["id"], "n_hub");
        assert_eq!(result["structuredContent"]["error"]["code"], "OUTSIDE");
        assert_eq!(result["isError"], true);
        assert_eq!(result["content"][1], image);
        assert_eq!(
            serde_json::from_str::<Value>(result["content"][0]["text"].as_str().unwrap()).unwrap(),
            result["structuredContent"]
        );
    }
}
