//! 宿主和执行代之间的身份转换与管道适配；纯消息由 gld-protocol 定义。

use std::io::{self, BufRead, Write};
use std::sync::Mutex;

use serde::Serialize;

pub use gld_protocol::generation::{
    GenerationFrame, HostFrame, HostOp, Op, WireAuth, WireGrant, WireNative, WirePrincipal,
};

use crate::auth::{AuthContext, Grant, Principal};
use crate::nodes::registry::NativeMember;

/// 单条消息上限。工具回包本身有界（图片预览、命令输出都有上限），这里只防对端发疯。
pub const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;

/// 把宿主已核实的身份放进私有通道，显式保留不参与持久化的 browser 标记。
pub fn auth_to_wire(auth: &AuthContext) -> WireAuth {
    WireAuth {
        principal: match auth.principal() {
            Principal::Anonymous => WirePrincipal::Anonymous,
            Principal::SharedSecret => WirePrincipal::SharedSecret,
            Principal::OAuthClient { client_id } => WirePrincipal::OauthClient {
                client_id: client_id.clone(),
            },
            Principal::OpenAiTunnel => WirePrincipal::OpenAiTunnel,
        },
        scope: auth.scope().to_string(),
        grant: auth.grant().map(|grant| WireGrant {
            id: grant.id.clone(),
            name: grant.name.clone(),
            workspaces: grant.workspaces.clone(),
            read_only: grant.read_only,
            browser: grant.browser,
        }),
    }
}

/// 还原私有通道中的鉴权结果；网络请求仍须先经过宿主认证。
pub fn auth_from_wire(auth: WireAuth) -> AuthContext {
    let principal = match auth.principal {
        WirePrincipal::Anonymous => Principal::Anonymous,
        WirePrincipal::SharedSecret => Principal::SharedSecret,
        WirePrincipal::OauthClient { client_id } => Principal::OAuthClient { client_id },
        WirePrincipal::OpenAiTunnel => Principal::OpenAiTunnel,
    };
    AuthContext::new(principal, auth.scope).with_grant(auth.grant.map(|grant| Grant {
        id: grant.id,
        name: grant.name,
        workspaces: grant.workspaces,
        read_only: grant.read_only,
        browser: grant.browser,
    }))
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
