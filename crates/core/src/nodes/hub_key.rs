//! Hub 向节点证明身份（RFC-0013 §7.3）。配对时节点钉住 Hub 的 Ed25519 公钥：
//! 握手时 Hub 对节点给的随机 nonce 签名，之后 Hub 发出的每条消息再按序号签名。
//! 能为 Hub 域名签出证书、或在托管反代上转发的人，因此冒充不了 Hub，也插不进、改不了调用；
//! 他们仍看得到内容、能丢弃或延迟消息，这些不在这里的保护范围内。

use std::sync::Arc;

use ring::rand::{SecureRandom, SystemRandom};
use ring::signature::{Ed25519KeyPair, KeyPair, UnparsedPublicKey, ED25519};

pub const NONCE_HEADER: &str = "x-gld-node-nonce";
pub const PROOF_HEADER: &str = "x-gld-hub-proof";
const PROOF_CONTEXT: &str = "gld-node-hub-proof-v1";
const MESSAGE_CONTEXT: &str = "gld-node-message-v1";
/// 追加在 Hub 消息 JSON 末尾。节点按这个后缀切回签名前的原文，不靠重新序列化 JSON。
const SIGNATURE_MEMBER: &str = ",\"gld_sig\":\"";
const SIGNATURE_HEX: usize = 128;
const REJECTED: &str = "Hub message signature or order is invalid";

#[cfg(test)]
#[path = "hub_key_tests.rs"]
mod tests;

fn lower_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn decode_hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2)
        || !text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).ok())
        .collect()
}

pub fn valid_public_key(text: &str) -> bool {
    text.len() == 64 && decode_hex(text).is_some()
}

pub fn new_nonce() -> Result<String, String> {
    let mut bytes = [0_u8; 32];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| "Cannot generate a node connection nonce".to_string())?;
    Ok(lower_hex(&bytes))
}

/// PKCS#8 私钥的十六进制，存进 Hub 的数据文件；换掉它所有节点都要重新配对。
pub fn generate_private_key() -> Result<String, String> {
    Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
        .map(|document| lower_hex(document.as_ref()))
        .map_err(|_| "Cannot generate the node Hub key".into())
}

/// 一条连接的身份：签名覆盖它，换 Hub、换机器或换一条连接都不能复用旧签名。
#[derive(Clone)]
pub struct Binding {
    hub_id: String,
    machine_id: String,
    nonce: String,
}

impl Binding {
    pub fn new(hub_id: &str, machine_id: &str, nonce: &str) -> Result<Self, String> {
        if !super::config::valid_id(hub_id)
            || !super::config::valid_id(machine_id)
            || nonce.len() != 64
            || decode_hex(nonce).is_none()
        {
            return Err("Invalid node connection binding".into());
        }
        Ok(Self {
            hub_id: hub_id.into(),
            machine_id: machine_id.into(),
            nonce: nonce.into(),
        })
    }

    pub fn nonce(&self) -> &str {
        &self.nonce
    }

    /// 各段都是定长十六进制或十进制序号，用 \0 分隔不会有歧义。
    fn signed_bytes(&self, context: &str, seq: Option<u64>, body: &str) -> Vec<u8> {
        let mut out = Vec::with_capacity(160 + body.len());
        for part in [context, &self.hub_id, &self.machine_id, &self.nonce] {
            out.extend_from_slice(part.as_bytes());
            out.push(0);
        }
        if let Some(seq) = seq {
            out.extend_from_slice(seq.to_string().as_bytes());
            out.push(0);
        }
        out.extend_from_slice(body.as_bytes());
        out
    }
}

pub struct HubKey(Ed25519KeyPair);

impl HubKey {
    pub fn from_stored(private_key: &str) -> Result<Self, String> {
        decode_hex(private_key)
            .and_then(|der| Ed25519KeyPair::from_pkcs8(&der).ok())
            .map(Self)
            .ok_or_else(|| "Invalid stored node Hub key".into())
    }

    pub fn public_key(&self) -> String {
        lower_hex(self.0.public_key().as_ref())
    }

    pub fn handshake_proof(&self, binding: &Binding) -> String {
        lower_hex(
            self.0
                .sign(&binding.signed_bytes(PROOF_CONTEXT, None, ""))
                .as_ref(),
        )
    }
}

fn verify(public_key: &str, message: &[u8], signature: &str) -> bool {
    match (decode_hex(public_key), decode_hex(signature)) {
        (Some(key), Some(signature)) if key.len() == 32 && signature.len() == 64 => {
            UnparsedPublicKey::new(&ED25519, key)
                .verify(message, &signature)
                .is_ok()
        }
        _ => false,
    }
}

pub fn verify_handshake(public_key: &str, binding: &Binding, proof: &str) -> bool {
    verify(
        public_key,
        &binding.signed_bytes(PROOF_CONTEXT, None, ""),
        proof,
    )
}

/// Hub 侧，每条连接一个，按发送顺序从 1 编号。
pub struct MessageSigner {
    key: Arc<HubKey>,
    binding: Binding,
    next_seq: u64,
}

impl MessageSigner {
    pub fn new(key: Arc<HubKey>, binding: Binding) -> Self {
        Self {
            key,
            binding,
            next_seq: 1,
        }
    }

    /// 超过 max_bytes 不占用序号：调用方报告“未发送”，下一条仍能对上节点期望的序号。
    pub fn sign(&mut self, line: &str, max_bytes: usize) -> Result<String, &'static str> {
        let body = line
            .strip_suffix('}')
            .filter(|body| body.starts_with('{') && body.len() > 1)
            .ok_or("node message must be a non-empty JSON object; it was not sent")?;
        let signature = self.key.0.sign(&self.binding.signed_bytes(
            MESSAGE_CONTEXT,
            Some(self.next_seq),
            line,
        ));
        let text = format!(
            "{body}{SIGNATURE_MEMBER}{}.{}\"}}",
            self.next_seq,
            lower_hex(signature.as_ref())
        );
        if text.len() > max_bytes {
            return Err("node message exceeds the size limit; it was not sent");
        }
        self.next_seq += 1;
        Ok(text)
    }
}

/// 节点侧：签名和序号都对上才交出 Hub 原文；不符说明链路上有人改过、插过或重放过消息。
pub struct MessageVerifier {
    public_key: String,
    binding: Binding,
    next_seq: u64,
}

impl MessageVerifier {
    pub fn new(public_key: &str, binding: Binding) -> Self {
        Self {
            public_key: public_key.into(),
            binding,
            next_seq: 1,
        }
    }

    pub fn verify(&mut self, text: &str) -> Result<String, &'static str> {
        // 取最后一处：签名段是 Hub 最后追加的，原文里同名键或字符串都在它之前。
        let start = text.rfind(SIGNATURE_MEMBER).ok_or(REJECTED)?;
        let tag = text[start + SIGNATURE_MEMBER.len()..]
            .strip_suffix("\"}")
            .ok_or(REJECTED)?;
        let (seq, signature) = tag.split_once('.').ok_or(REJECTED)?;
        if seq != self.next_seq.to_string() || signature.len() != SIGNATURE_HEX {
            return Err(REJECTED);
        }
        let original = format!("{}}}", &text[..start]);
        if !verify(
            &self.public_key,
            &self
                .binding
                .signed_bytes(MESSAGE_CONTEXT, Some(self.next_seq), &original),
            signature,
        ) {
            return Err(REJECTED);
        }
        self.next_seq += 1;
        Ok(original)
    }
}
