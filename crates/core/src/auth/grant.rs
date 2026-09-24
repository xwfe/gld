//! 只开部分项目的凭据（RFC-0007，审查 D07）。
//!
//! 服务口令和服务的 bearer 令牌管全部项目。grant 是另外发的钥匙：自带一把口令（给
//! OAuth 授权页）和一个令牌（给 bearer），用哪把进来，这次请求就只看得到那几个项目。
//! 范围绑在凭据上而不是 `client_id` 上，因为知道口令的人能自己动态注册客户端，
//! `client_id` 事先不知道。
//!
//! 这里只管"凭据 → 范围"。范围怎么落到项目列表和工具上在 [`crate::hub`]。

use serde::{Deserialize, Serialize};

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
}

/// 验鉴权时去哪儿找 grant。
pub trait GrantBook: Send + Sync {
    /// 令牌里写的 id 还在不在。不在就是撤销了。
    fn by_id(&self, id: &str) -> Option<Grant>;
    /// OAuth 授权页填的口令是不是某个 grant 的。
    fn by_password(&self, password: &str) -> Option<Grant>;
    /// bearer 请求头里的令牌是不是某个 grant 的。
    fn by_bearer_token(&self, token: &str) -> Option<Grant>;
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
        Self::find(|record| record.grant.id == id)
    }
    fn by_password(&self, password: &str) -> Option<Grant> {
        matching_secret(password, |record| &record.oauth_password)
    }
    fn by_bearer_token(&self, token: &str) -> Option<Grant> {
        matching_secret(token, |record| &record.bearer_token)
    }
}

/// 按钥匙找 grant：逐条做定长比较，空钥匙谁都不匹配。
fn matching_secret(offered: &str, secret: impl Fn(&GrantRecord) -> &String) -> Option<Grant> {
    if offered.is_empty() {
        return None;
    }
    StoredGrants::find(|record| {
        let expected = secret(record);
        !expected.is_empty() && constant_time_eq_str(offered, expected)
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
        self.find(|record| record.grant.id == id)
    }
    fn by_password(&self, password: &str) -> Option<Grant> {
        self.find(|record| !password.is_empty() && record.oauth_password == password)
    }
    fn by_bearer_token(&self, token: &str) -> Option<Grant> {
        self.find(|record| !token.is_empty() && record.bearer_token == token)
    }
}
