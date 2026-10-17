//! 只开部分项目的凭据（RFC-0007，审查 D07）。
//!
//! 服务口令和服务的 bearer 令牌管全部项目。grant 是另外发的钥匙：自带一把口令（给
//! OAuth 授权页）和一个令牌（给 bearer），用哪把进来，这次请求就只看得到那几个项目。
//! 范围绑在凭据上而不是 `client_id` 上，因为知道口令的人能自己动态注册客户端，
//! `client_id` 事先不知道。
//!
//! 这里只管"凭据 → 范围"。范围怎么落到项目列表和工具上在 [`crate::hub`]。

use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const DEFAULT_BROWSER_ORIGIN: &str = "chrome-extension://kngiafgkdnlkgmefdafaibkibegkcaef";
pub const DEFAULT_BROWSER_GRANT_TTL_SECONDS: u64 = 8 * 60 * 60;
pub const MAX_BROWSER_GRANT_TTL_SECONDS: u64 = 24 * 60 * 60;

use super::bearer::constant_time_eq_str;
use crate::data::DataStore;

/// 一个 grant 的范围。跟着 [`super::AuthContext`] 往下走，**不带钥匙本身**。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grant {
    /// 随机 id，写进令牌里。不用名字：删了再建一个同名的，旧令牌不能跟着复活。
    pub id: String,
    pub name: String,
    /// 开放的成员 id（本地工作区或远端 ccnm 成员）。
    pub workspaces: Vec<String>,
    /// 只读：工具按 read-only 工具集取交集，远端只给读的那几个。
    #[serde(default)]
    pub read_only: bool,
}

impl Grant {
    pub fn allows(&self, member_id: &str) -> bool {
        self.workspaces.iter().any(|id| id == member_id)
    }
}

/// 数据文件里的一条：范围加上它自己的两把钥匙。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GrantRecord {
    #[serde(flatten)]
    pub grant: Grant,
    pub oauth_password: String,
    pub bearer_token: String,
    #[serde(default)]
    pub created_at: String,
    /// 浏览器专用短期钥匙。原始值只在创建时返回，不落盘。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser: Option<BrowserCredential>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowserCredential {
    pub token_sha256: String,
    pub expires_at_unix_secs: u64,
    pub allowed_origins: Vec<String>,
}

impl BrowserCredential {
    pub fn new(token: &str, expires_at_unix_secs: u64, allowed_origins: Vec<String>) -> Self {
        Self {
            token_sha256: browser_token_hash(token),
            expires_at_unix_secs,
            allowed_origins,
        }
    }

    fn matches(&self, token: &str, origin: Option<&str>, now: u64) -> bool {
        !token.is_empty()
            && now < self.expires_at_unix_secs
            && origin.is_none_or(|offered| self.allowed_origins.iter().any(|item| item == offered))
            && constant_time_eq_str(&browser_token_hash(token), &self.token_sha256)
    }
}

fn browser_token_hash(token: &str) -> String {
    format!("{:x}", Sha256::digest(token.as_bytes()))
}

fn unix_now() -> Option<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|time| time.as_secs())
}

/// 验鉴权时去哪儿找 grant。
pub trait GrantBook: Send + Sync {
    /// 令牌里写的 id 还在不在。不在就是撤销了。
    fn by_id(&self, id: &str) -> Option<Grant>;
    /// OAuth 授权页填的口令是不是某个 grant 的。
    fn by_password(&self, password: &str) -> Option<Grant>;
    /// bearer 请求头里的令牌是不是某个 grant 的。
    fn by_bearer_token(&self, token: &str) -> Option<Grant>;
    /// 只供浏览器兼容路由使用。Origin 缺省的扩展后台请求仍靠能力令牌鉴权。
    fn by_browser_token(&self, _token: &str, _origin: Option<&str>) -> Option<Grant> {
        None
    }
}

/// 不发 grant 的入口（工作区自己的监听器、GPT Actions）：谁都查不到。
pub struct NoGrants;

impl GrantBook for NoGrants {
    fn by_id(&self, _: &str) -> Option<Grant> {
        None
    }
    fn by_password(&self, _: &str) -> Option<Grant> {
        None
    }
    fn by_bearer_token(&self, _: &str) -> Option<Grant> {
        None
    }
}

/// 服务用的：每次都读数据文件。
///
/// 不缓存，撤销才能在下一次请求就生效，不用重启服务。服务口令、服务令牌、没写 grant
/// 的令牌先验、先放行，走不到这里，全权那条路的开销不变；但对不上服务令牌的 bearer
/// 请求、填错口令的授权页都会读一次数据文件——挂公网被扫的时候这是多出来的开销。
/// 数据文件读不出来一律当"查不到"：鉴权出错的方向只能是拒绝。
pub struct StoredGrants;

impl StoredGrants {
    fn find(matches: impl Fn(&GrantRecord) -> bool) -> Option<Grant> {
        DataStore::read_file(|data| Ok(data.grants.clone()))
            .ok()?
            .into_iter()
            .find(|record| matches(record))
            .map(|record| record.grant)
    }
}

impl GrantBook for StoredGrants {
    fn by_id(&self, id: &str) -> Option<Grant> {
        Self::find(|record| record.browser.is_none() && record.grant.id == id)
    }
    fn by_password(&self, password: &str) -> Option<Grant> {
        matching_secret(password, |record| &record.oauth_password)
    }
    fn by_bearer_token(&self, token: &str) -> Option<Grant> {
        matching_secret(token, |record| &record.bearer_token)
    }

    fn by_browser_token(&self, token: &str, origin: Option<&str>) -> Option<Grant> {
        let now = unix_now()?;
        Self::find(|record| {
            record
                .browser
                .as_ref()
                .is_some_and(|key| key.matches(token, origin, now))
        })
    }
}

/// 按钥匙找 grant：逐条做定长比较，空钥匙谁都不匹配。
fn matching_secret(offered: &str, secret: impl Fn(&GrantRecord) -> &String) -> Option<Grant> {
    if offered.is_empty() {
        return None;
    }
    StoredGrants::find(|record| {
        let expected = secret(record);
        record.browser.is_none() && !expected.is_empty() && constant_time_eq_str(offered, expected)
    })
}

/// 测试里用的固定一组 grant。
#[cfg(test)]
pub struct FixedGrants(pub std::sync::Mutex<Vec<GrantRecord>>);

#[cfg(test)]
impl FixedGrants {
    pub fn new(records: Vec<GrantRecord>) -> Self {
        Self(std::sync::Mutex::new(records))
    }

    pub fn revoke(&self, id: &str) {
        self.0
            .lock()
            .unwrap()
            .retain(|record| record.grant.id != id);
    }

    fn find(&self, matches: impl Fn(&GrantRecord) -> bool) -> Option<Grant> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .find(|record| matches(record))
            .map(|record| record.grant.clone())
    }
}

#[cfg(test)]
impl GrantBook for FixedGrants {
    fn by_id(&self, id: &str) -> Option<Grant> {
        self.find(|record| record.browser.is_none() && record.grant.id == id)
    }
    fn by_password(&self, password: &str) -> Option<Grant> {
        self.find(|record| {
            record.browser.is_none() && !password.is_empty() && record.oauth_password == password
        })
    }

    fn by_bearer_token(&self, token: &str) -> Option<Grant> {
        self.find(|record| {
            record.browser.is_none() && !token.is_empty() && record.bearer_token == token
        })
    }

    fn by_browser_token(&self, token: &str, origin: Option<&str>) -> Option<Grant> {
        let now = unix_now()?;
        self.find(|record| {
            record
                .browser
                .as_ref()
                .is_some_and(|key| key.matches(token, origin, now))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_key_checks_secret_expiry_and_present_origin() {
        let key =
            BrowserCredential::new("test-secret", 100, vec!["https://allowed.example".into()]);
        assert!(key.matches("test-secret", None, 99));
        assert!(key.matches("test-secret", Some("https://allowed.example"), 99));
        assert!(!key.matches("test-secret", Some("https://other.example"), 99));
        assert!(!key.matches("test-secret", Some("null"), 99));
        assert!(!key.matches("test-secret", Some(""), 99));
        assert!(!key.matches("test-secret", None, 100));
        assert!(!key.matches("wrong", None, 99));
        assert!(!key.matches("", None, 99));
        assert!(!serde_json::to_string(&key).unwrap().contains("test-secret"));
    }

    #[test]
    fn old_records_work_and_browser_records_have_no_legacy_channel() {
        let old: GrantRecord = serde_json::from_value(serde_json::json!({
            "id": "old", "name": "old", "workspaces": ["api"], "read_only": true,
            "oauth_password": "old-password", "bearer_token": "old-bearer"
        }))
        .unwrap();
        assert!(old.browser.is_none());
        let browser = GrantRecord {
            grant: Grant {
                id: "browser".into(),
                name: "browser".into(),
                workspaces: vec!["api".into()],
                read_only: true,
            },
            // 即使记录损坏写入了旧字段，browser 类型也不能进入永久认证通道。
            oauth_password: "unexpected-password".into(),
            bearer_token: "unexpected-bearer".into(),
            created_at: String::new(),
            browser: Some(BrowserCredential::new(
                "url-key",
                unix_now().unwrap() + 60,
                vec![],
            )),
        };
        let grants = FixedGrants::new(vec![old, browser]);
        assert_eq!(grants.by_password("old-password").unwrap().id, "old");
        assert_eq!(grants.by_bearer_token("old-bearer").unwrap().id, "old");
        assert!(grants.by_id("browser").is_none());
        assert!(grants.by_password("unexpected-password").is_none());
        assert!(grants.by_bearer_token("unexpected-bearer").is_none());
        assert!(grants.by_password("").is_none());
        assert!(grants.by_bearer_token("").is_none());
        assert_eq!(
            grants.by_browser_token("url-key", None).unwrap().id,
            "browser"
        );
        grants.revoke("browser");
        assert!(grants.by_browser_token("url-key", None).is_none());
    }
}
