//! 每台机器只保留一条已认证连接，项目句柄绑定机器与目标项目。
//! 网络等待不持有登记表的锁；断线后不重放调用。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use serde_json::Value;
use sha2::{Digest, Sha256};

use gld_core::auth::AuthContext;
use gld_core::bridge::session::Offered;
use gld_core::hub::NodeBridge;
use gld_core::nodes::config::{self, AuthenticatedMachine};
use gld_core::nodes::hub_key::MessageSigner;
use gld_core::nodes::NativeMember;
use gld_core::tools::workspace::{tool_err, wrap_tool_result, WorkspaceError};

use super::connection::{Connection, ConnectionError};

const HANDSHAKE: Duration = Duration::from_secs(10);
const AUTH_NOT_READY_MESSAGE: &str =
    "Native node authorization forwarding is not connected; no workspace tool was called.";

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
        _auth: &AuthContext,
        _member: &NativeMember,
        _name: &str,
        _args: Value,
    ) -> Value {
        failure("AUTH_NOT_READY", AUTH_NOT_READY_MESSAGE, false)
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
        // 一起关：每条都可能要等在途调用收尾，一条条来就成了机器数乘等待时间。
        std::thread::scope(|scope| {
            for connection in &connections {
                scope.spawn(|| connection.close());
            }
        });
    }
}

impl NodeBridge for Nodes {
    fn members(&self) -> Result<Vec<NativeMember>, String> {
        Nodes::members(self)
    }

    fn call(&self, auth: &AuthContext, member: &NativeMember, tool: &str, args: Value) -> Value {
        Nodes::call(self, auth, member, tool, args)
    }
}

impl Reservation {
    pub(super) async fn accept(self, socket: axum::extract::ws::WebSocket, signer: MessageSigner) {
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
    fn validate_reservation(&self, machine_id: &str, generation: &str) -> Result<(), String> {
        let authenticated = {
            let entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
            if self.stopped.load(Ordering::Acquire) {
                return Err("HUB_STOPPING".into());
            }
            entries
                .get(machine_id)
                .filter(|entry| entry.generation == generation && !entry.retired)
                .ok_or("NODE_CONNECTION_REPLACED")?
                .authenticated
                .clone()
        };
        if !config::is_current(&authenticated)? {
            return Err("INVALID_NODE_CREDENTIALS".into());
        }
        Ok(())
    }

    fn initialize(
        &self,
        machine_id: &str,
        generation: &str,
        connection: &Arc<Connection>,
    ) -> Result<(), String> {
        self.validate_reservation(machine_id, generation)?;
        // Protocol negotiation needs no caller authority. Project discovery must wait for
        // the trusted forwarding contract; it must never impersonate a local operator.
        connection
            .with_peer(|peer| {
                peer.initialize("gld-hub", HANDSHAKE)?;
                peer.list_tools(HANDSHAKE)?;
                Ok(())
            })
            .map_err(|error| {
                match error {
                    ConnectionError::Busy => "NODE_PROTOCOL_BUSY",
                    ConnectionError::Closed => "NODE_CONNECTION_CLOSED",
                    ConnectionError::Peer(_) => "NODE_PROTOCOL_HANDSHAKE_FAILED",
                }
                .to_string()
            })?;
        self.validate_reservation(machine_id, generation)?;
        Err(format!("AUTH_NOT_READY: {AUTH_NOT_READY_MESSAGE}"))
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

fn workspace_handle(machine_id: &str, workspace_id: &str) -> String {
    let encoded = serde_json::to_vec(&(machine_id, workspace_id)).expect("string pair");
    let digest = format!("{:x}", Sha256::digest(encoded));
    format!("n_{}", &digest[..32])
}

fn failure(code: &'static str, message: &str, retryable: bool) -> Value {
    wrap_tool_result(tool_err(WorkspaceError::Tool {
        code,
        message: message.into(),
        category: "runtime",
        retryable,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::test_home::run_isolated;
    use gld_core::auth::{
        ActionsSubject, CredentialKind, Grant, GrantPermissions, GrantStatus, McpSubject,
        ProjectScope,
    };
    use serde_json::json;

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
    fn no_identity_can_use_the_retired_node_forwarding_path() {
        let nodes = Nodes::new();
        let member = NativeMember {
            id: "n_project".into(),
            name: "node/project".into(),
            machine_id: "machine".into(),
            remote_workspace_id: "project".into(),
            online: true,
            revoked: false,
            tool_profile: "commands".into(),
        };
        let grant = Grant {
            id: "grant-id".into(),
            name: "fixture".into(),
            scope: ProjectScope::AllProjects,
            permissions: GrantPermissions {
                write: true,
                ..GrantPermissions::default()
            },
            status: GrantStatus::Active,
        };
        for auth in [
            AuthContext::local_operator(),
            AuthContext::verified_mcp(
                "hub",
                grant.clone(),
                CredentialKind::Token,
                McpSubject::Token,
            )
            .unwrap(),
            AuthContext::verified_mcp(
                "hub",
                grant.clone(),
                CredentialKind::OAuth,
                McpSubject::OAuth {
                    client_id: "client".into(),
                },
            )
            .unwrap(),
            AuthContext::verified_mcp("hub", grant, CredentialKind::Url, McpSubject::Url).unwrap(),
            AuthContext::actions("actions", "project", ActionsSubject::ApiKey, true).unwrap(),
            AuthContext::actions("actions", "project", ActionsSubject::Anonymous, true).unwrap(),
        ] {
            let result = nodes.call(&auth, &member, "exec_command", json!({"cmd": "unused"}));
            assert_eq!(
                result["structuredContent"]["error"]["code"],
                "AUTH_NOT_READY"
            );
            assert_eq!(result["isError"], true);
        }
    }

    #[test]
    fn readiness_rechecks_current_credentials_and_reservation_generation() {
        run_isolated(
            module_path!(),
            "readiness_rechecks_current_credentials_and_reservation_generation",
            || {
                let machine = "22222222222222222222222222222222";
                let first_token = "synthetic-node-credential-0000000000000001";
                let second_token = "synthetic-node-credential-0000000000000002";
                config::register_machine("test-node", Some(machine), first_token).unwrap();
                let nodes = Arc::new(Nodes::new());
                let first = nodes
                    .reserve(config::authenticate(machine, first_token).unwrap())
                    .unwrap();
                assert!(nodes
                    .validate_reservation(machine, &first.generation)
                    .is_ok());
                assert_eq!(
                    nodes
                        .validate_reservation(machine, "different-generation")
                        .unwrap_err(),
                    "NODE_CONNECTION_REPLACED"
                );

                config::rotate_credential(machine, second_token).unwrap();
                assert_eq!(
                    nodes
                        .validate_reservation(machine, &first.generation)
                        .unwrap_err(),
                    "INVALID_NODE_CREDENTIALS"
                );
                nodes.close_stale(machine).unwrap();
                let fresh = nodes
                    .reserve(config::authenticate(machine, second_token).unwrap())
                    .unwrap();
                let current_generation = fresh.generation.clone();
                drop(first);
                assert!(nodes
                    .validate_reservation(machine, &current_generation)
                    .is_ok());

                config::revoke_machine(machine).unwrap();
                assert_eq!(
                    nodes
                        .validate_reservation(machine, &current_generation)
                        .unwrap_err(),
                    "INVALID_NODE_CREDENTIALS"
                );
                nodes.close_stale(machine).unwrap();
                assert_eq!(
                    nodes
                        .validate_reservation(machine, &current_generation)
                        .unwrap_err(),
                    "NODE_CONNECTION_REPLACED"
                );
                nodes.close_all();
                assert_eq!(
                    nodes
                        .validate_reservation(machine, &current_generation)
                        .unwrap_err(),
                    "HUB_STOPPING"
                );
                drop(fresh);
            },
        );
    }
}
