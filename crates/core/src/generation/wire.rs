//! 宿主和执行代之间的身份转换与管道适配；纯消息由 gld-protocol 定义。

use std::io::{self, BufRead, Write};
use std::sync::Mutex;

use serde::Serialize;

pub use gld_protocol::generation::{
    GenerationFrame, HostFrame, HostOp, Op, WireActionsSubject, WireAuth, WireCredentialKind,
    WireGrant, WireGrantPermissions, WireGrantStatus, WireMcpSubject, WireNative, WireProjectScope,
    WireServiceCapabilities,
};

use crate::nodes::NativeMember;
use gld_policy::{
    ActionsSubject, AuthContext, CallerIdentity, CredentialKind, Grant, GrantPermissions,
    GrantStatus, McpSubject, ProjectScope, ServiceCapabilities,
};

/// 单条消息上限。工具回包本身有界，这里限制私有帧的内存使用。
pub const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;

/// 只编码网络主体；可信本机调用使用独立的 LocalTool 操作。
pub fn auth_to_wire(auth: &AuthContext) -> Result<WireAuth, String> {
    let entry = auth.scope().to_string();
    match auth.id().identity() {
        CallerIdentity::LocalOperator => Err("Local operator is not a network identity".into()),
        CallerIdentity::McpClient { subject, .. } => {
            let grant = auth.grant().ok_or("MCP grant authority is missing")?;
            let credential_kind = match auth
                .credential_kind()
                .ok_or("Credential purpose is missing")?
            {
                CredentialKind::OAuth => WireCredentialKind::OAuth,
                CredentialKind::Token => WireCredentialKind::Token,
                CredentialKind::Url => WireCredentialKind::Url,
            };
            let subject = match subject {
                McpSubject::OAuth { client_id } => WireMcpSubject::OAuth {
                    client_id: client_id.clone(),
                },
                McpSubject::Token => WireMcpSubject::Token,
                McpSubject::Url => WireMcpSubject::Url,
                McpSubject::Tunnel { binding_id } => WireMcpSubject::Tunnel {
                    binding_id: binding_id.clone(),
                },
            };
            Ok(WireAuth::Mcp {
                entry,
                subject,
                credential_kind,
                grant: grant_to_wire(grant),
            })
        }
        CallerIdentity::ActionsClient { subject, .. } => {
            let subject = match subject {
                ActionsSubject::Anonymous => WireActionsSubject::Anonymous,
                ActionsSubject::ApiKey => WireActionsSubject::ApiKey,
                ActionsSubject::OAuth { client_id } => WireActionsSubject::OAuth {
                    client_id: client_id.clone(),
                },
            };
            Ok(WireAuth::Actions {
                entry,
                workspace_id: auth
                    .actions_workspace_id()
                    .ok_or("Actions workspace is missing")?
                    .to_string(),
                subject,
                write: !auth.read_only(),
            })
        }
    }
}

/// 私有通道仍必须校验用途、范围、状态和主体组合，不能把缺失字段当全权。
pub fn auth_from_wire(auth: WireAuth) -> Result<AuthContext, String> {
    let auth = match auth {
        WireAuth::Mcp {
            entry,
            subject,
            credential_kind,
            grant,
        } => {
            let kind = match credential_kind {
                WireCredentialKind::OAuth => CredentialKind::OAuth,
                WireCredentialKind::Token => CredentialKind::Token,
                WireCredentialKind::Url => CredentialKind::Url,
            };
            let subject = match subject {
                WireMcpSubject::OAuth { client_id } => McpSubject::OAuth { client_id },
                WireMcpSubject::Token => McpSubject::Token,
                WireMcpSubject::Url => McpSubject::Url,
                WireMcpSubject::Tunnel { binding_id } => McpSubject::Tunnel { binding_id },
            };
            AuthContext::verified_mcp(entry, grant_from_wire(grant), kind, subject)
        }
        WireAuth::Actions {
            entry,
            workspace_id,
            subject,
            write,
        } => {
            let subject = match subject {
                WireActionsSubject::Anonymous => ActionsSubject::Anonymous,
                WireActionsSubject::ApiKey => ActionsSubject::ApiKey,
                WireActionsSubject::OAuth { client_id } => ActionsSubject::OAuth { client_id },
            };
            AuthContext::actions(entry, workspace_id, subject, write)
        }
    };
    auth.map_err(|error| error.to_string())
}

fn grant_to_wire(grant: &Grant) -> WireGrant {
    WireGrant {
        id: grant.id.clone(),
        name: grant.name.clone(),
        scope: match &grant.scope {
            ProjectScope::AllProjects => WireProjectScope::AllProjects,
            ProjectScope::Selected(ids) => WireProjectScope::Selected(ids.clone()),
        },
        permissions: WireGrantPermissions {
            write: grant.permissions.write,
            service: WireServiceCapabilities {
                user_prompts: grant.permissions.service.user_prompts,
                machine_mcp: grant.permissions.service.machine_mcp,
                managed_workspaces: grant.permissions.service.managed_workspaces,
            },
        },
        status: match grant.status {
            GrantStatus::Active => WireGrantStatus::Active,
            GrantStatus::Revoked => WireGrantStatus::Revoked,
        },
    }
}

fn grant_from_wire(grant: WireGrant) -> Grant {
    Grant {
        id: grant.id,
        name: grant.name,
        scope: match grant.scope {
            WireProjectScope::AllProjects => ProjectScope::AllProjects,
            WireProjectScope::Selected(ids) => ProjectScope::Selected(ids),
        },
        permissions: GrantPermissions {
            write: grant.permissions.write,
            service: ServiceCapabilities {
                user_prompts: grant.permissions.service.user_prompts,
                machine_mcp: grant.permissions.service.machine_mcp,
                managed_workspaces: grant.permissions.service.managed_workspaces,
            },
        },
        status: match grant.status {
            WireGrantStatus::Active => GrantStatus::Active,
            WireGrantStatus::Revoked => GrantStatus::Revoked,
        },
    }
}

impl From<&NativeMember> for WireNative {
    fn from(member: &NativeMember) -> Self {
        Self {
            id: member.id.clone(),
            name: member.name.clone(),
            machine_id: member.machine_id.clone(),
            remote_workspace_id: member.remote_workspace_id.clone(),
            online: member.online,
            revoked: member.revoked,
            tool_profile: member.tool_profile.clone(),
        }
    }
}

impl From<WireNative> for NativeMember {
    fn from(member: WireNative) -> Self {
        Self {
            id: member.id,
            name: member.name,
            machine_id: member.machine_id,
            remote_workspace_id: member.remote_workspace_id,
            online: member.online,
            revoked: member.revoked,
            tool_profile: member.tool_profile,
        }
    }
}

/// 一行一条地发。多个线程共用一条管道，一整行要在一把锁里写完，否则两条消息会交错。
pub struct FrameWriter<W> {
    inner: Mutex<W>,
}

impl<W: Write> FrameWriter<W> {
    pub fn new(writer: W) -> Self {
        Self {
            inner: Mutex::new(writer),
        }
    }

    #[cfg(test)]
    pub fn into_inner(self) -> W {
        self.inner
            .into_inner()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn send<T: Serialize>(&self, frame: &T) -> io::Result<()> {
        let mut line = serde_json::to_vec(frame).map_err(io::Error::other)?;
        if line.len() >= MAX_FRAME_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("消息 {} 字节，超过上限 {MAX_FRAME_BYTES}", line.len()),
            ));
        }
        line.push(b'\n');
        let mut writer = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        writer.write_all(&line)?;
        writer.flush()
    }
}

/// 读一行。对端关了返回 `Ok(None)`。超过上限当错误：继续读只会把内存吃光。
pub fn read_frame<R: BufRead>(reader: &mut R) -> io::Result<Option<String>> {
    let mut line = Vec::new();
    loop {
        let buffer = reader.fill_buf()?;
        if buffer.is_empty() {
            return Ok((!line.is_empty()).then(|| String::from_utf8_lossy(&line).into_owned()));
        }
        let (chunk, done) = match buffer.iter().position(|byte| *byte == b'\n') {
            Some(index) => (&buffer[..index], true),
            None => (buffer, false),
        };
        if line.len() + chunk.len() > MAX_FRAME_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("消息超过上限 {MAX_FRAME_BYTES} 字节"),
            ));
        }
        line.extend_from_slice(chunk);
        let consumed = chunk.len() + usize::from(done);
        reader.consume(consumed);
        if done {
            return Ok(Some(String::from_utf8_lossy(&line).into_owned()));
        }
    }
}
