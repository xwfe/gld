//! 操作员生成短期邀请；目标机用已落盘的请求 ID 与专用凭据完成一次登记。
//! 请求 ID 同时作为机器 ID，丢失响应后只恢复原登记，不重新签发凭据。

use std::collections::HashSet;
use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::data::{AppData, DataStore};
use crate::error::AppError;

use super::config::{self, RegisteredMachine};

const INVITATION_LIFETIME: u64 = 600;
const MAX_INVITATIONS: usize = 64;
const MAX_FAILED_ATTEMPTS: u8 = 8;

#[cfg(test)]
#[path = "pairing_tests.rs"]
mod tests;

#[derive(Clone, Serialize, Deserialize)]
pub struct InvitationRecord {
    pub id: String,
    pub name: String,
    pub invitation_sha256: String,
    pub expires_at: u64,
    pub failed_attempts: u8,
}

impl fmt::Debug for InvitationRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InvitationRecord")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("expires_at", &self.expires_at)
            .field("failed_attempts", &self.failed_attempts)
            .finish_non_exhaustive()
    }
}

#[derive(Serialize)]
pub struct Invitation {
    pub hub_id: String,
    pub invitation: String,
    pub name: String,
    /// Unix 时间戳，单位为秒；从生成时起有效十分钟。
    pub expires_at: u64,
}

impl fmt::Debug for Invitation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Invitation")
            .field("hub_id", &self.hub_id)
            .field("name", &self.name)
            .field("expires_at", &self.expires_at)
            .field("invitation", &"[redacted]")
            .finish()
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairRequest {
    pub request_id: String,
    pub credential: String,
}

impl fmt::Debug for PairRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PairRequest")
            .field("request_id", &self.request_id)
            .field("credential", &"[redacted]")
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairError {
    InvalidRequest,
    InvalidInvitation,
    Conflict,
    Unavailable,
}

impl fmt::Display for PairError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidRequest => "Invalid node pairing request",
            Self::InvalidInvitation => "Node invitation is invalid, expired, used or locked",
            Self::Conflict => "Node pairing conflicts with an existing registration or credential",
            Self::Unavailable => "Node pairing registry is unavailable",
        })
    }
}

impl std::error::Error for PairError {}

struct Token<'a> {
    hub_id: &'a str,
    invitation_id: &'a str,
}

fn parse_token(token: &str) -> Result<Token<'_>, PairError> {
    let mut parts = token.split('.');
    let hub_id = parts.next().ok_or(PairError::InvalidInvitation)?;
    let invitation_id = parts.next().ok_or(PairError::InvalidInvitation)?;
    let secret = parts.next().ok_or(PairError::InvalidInvitation)?;
    if parts.next().is_some()
        || !config::valid_id(hub_id)
        || !config::valid_id(invitation_id)
        || !lower_hex(secret, 64)
    {
        return Err(PairError::InvalidInvitation);
    }
    Ok(Token {
        hub_id,
        invitation_id,
    })
}

pub fn invitation_hub_id(token: &str) -> Result<String, String> {
    parse_token(token)
        .map(|token| token.hub_id.to_owned())
        .map_err(|error| error.to_string())
}

fn lower_hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn timestamp() -> Result<u64, String> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|time| time.as_secs())
        .map_err(|_| "Cannot read the node pairing clock".into())
}

fn validate_invitations(data: &AppData) -> Result<(), String> {
    let mut ids = HashSet::new();
    let mut names = HashSet::new();
    if data.node_invitations.len() > MAX_INVITATIONS
        || (!data.node_invitations.is_empty() && !config::valid_id(&data.node_hub_id))
        || data.node_invitations.iter().any(|invite| {
            !config::valid_id(&invite.id)
                || !config::valid_name(&invite.name)
                || invite.name.contains(['/', '\\'])
                || !lower_hex(&invite.invitation_sha256, 64)
                || invite.failed_attempts > MAX_FAILED_ATTEMPTS
                || !ids.insert(&invite.id)
                || !names.insert(invite.name.to_ascii_lowercase())
        })
    {
        return Err("Invalid stored node invitations".into());
    }
    Ok(())
}

/// 只由本机操作员入口调用。相同名称的新邀请替换尚未使用的旧邀请。
pub fn invite(name: &str) -> Result<Invitation, String> {
    DataStore::update_file(|data| {
        let now = timestamp().map_err(AppError::Message)?;
        invite_in(data, name, now).map_err(AppError::Message)
    })
    .map_err(|error| error.to_string())
}

fn invite_in(data: &mut AppData, name: &str, now: u64) -> Result<Invitation, String> {
    config::validate_registry(data)?;
    validate_invitations(data)?;
    let name = name.trim();
    if !config::valid_name(name) || name.contains(['/', '\\']) {
        return Err(
            "Node name must be 1..=128 bytes without control characters or path separators".into(),
        );
    }
    if data
        .node_machines
        .iter()
        .any(|machine| machine.name.eq_ignore_ascii_case(name))
    {
        return Err("Node name is already registered; use its existing configuration".into());
    }
    let expires_at = now
        .checked_add(INVITATION_LIFETIME)
        .ok_or("Node invitation expiry is out of range")?;
    data.node_invitations.retain(|invite| {
        invite.expires_at > now
            && invite.failed_attempts < MAX_FAILED_ATTEMPTS
            && !invite.name.eq_ignore_ascii_case(name)
    });
    if data.node_invitations.len() >= MAX_INVITATIONS {
        return Err("Too many active node invitations; wait for one to expire".into());
    }
    if data.node_hub_id.is_empty() {
        data.node_hub_id = uuid::Uuid::new_v4().simple().to_string();
    }
    let id = uuid::Uuid::new_v4().simple().to_string();
    let invitation = format!(
        "{}.{}.{}{}",
        data.node_hub_id,
        id,
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple(),
    );
    data.node_invitations.push(InvitationRecord {
        id,
        name: name.into(),
        invitation_sha256: config::credential_hash(&invitation),
        expires_at,
        failed_attempts: 0,
    });
    Ok(Invitation {
        hub_id: data.node_hub_id.clone(),
        invitation,
        name: name.into(),
        expires_at,
    })
}

pub fn pair(token: &str, request: PairRequest) -> Result<RegisteredMachine, PairError> {
    let mut outcome = None;
    let mut changed = false;
    let saved = DataStore::update_file(|data| {
        // 锁等待可能跨过邀请期限；拿到事务锁之后才判断当前时间。
        let now = timestamp().map_err(AppError::Message)?;
        outcome = Some(pair_in(data, token, &request, now, &mut changed));
        if changed {
            Ok(())
        } else {
            // update_file 的闭包 Err 会保留原文件；无变化的拒绝或恢复不制造写盘。
            Err(AppError::Message(
                "Node pairing made no persistent change".into(),
            ))
        }
    });
    if changed && saved.is_err() {
        return Err(PairError::Unavailable);
    }
    // 只有进入过事务的结果才有效；读盘/加锁失败不会执行上面的闭包。
    outcome.unwrap_or(Err(PairError::Unavailable))
}

fn pair_in(
    data: &mut AppData,
    token: &str,
    request: &PairRequest,
    now: u64,
    changed: &mut bool,
) -> Result<RegisteredMachine, PairError> {
    if !config::valid_id(&request.request_id)
        || config::validate_credential(&request.credential).is_err()
    {
        return Err(PairError::InvalidRequest);
    }
    let parsed = parse_token(token)?;
    config::validate_registry(data).map_err(|_| PairError::Unavailable)?;
    validate_invitations(data).map_err(|_| PairError::Unavailable)?;
    if parsed.hub_id != data.node_hub_id {
        return Err(PairError::InvalidInvitation);
    }
    let credential_hash = config::credential_hash(&request.credential);
    if let Some(machine) = data
        .node_machines
        .iter()
        .find(|machine| machine.id == request.request_id)
    {
        if machine.revoked
            || machine.credential_generation != 0
            || !config::digest_matches(&machine.credential_sha256, &credential_hash)
        {
            return Err(PairError::Conflict);
        }
        return Ok(RegisteredMachine {
            hub_id: data.node_hub_id.clone(),
            machine_id: machine.id.clone(),
            name: machine.name.clone(),
            revoked: false,
        });
    }
    let index = data
        .node_invitations
        .iter()
        .position(|invite| invite.id == parsed.invitation_id)
        .ok_or(PairError::InvalidInvitation)?;
    let invitation = &mut data.node_invitations[index];
    if invitation.expires_at <= now || invitation.failed_attempts >= MAX_FAILED_ATTEMPTS {
        return Err(PairError::InvalidInvitation);
    }
    if !config::digest_matches(
        &invitation.invitation_sha256,
        &config::credential_hash(token),
    ) {
        invitation.failed_attempts += 1;
        *changed = true;
        return Err(PairError::InvalidInvitation);
    }
    let name = invitation.name.clone();
    let result = config::register_in(data, &name, Some(&request.request_id), &request.credential)
        .map_err(|_| PairError::Conflict)?;
    data.node_invitations.remove(index);
    *changed = true;
    Ok(result)
}
