use alloc::{string::String, vec::Vec};

use serde::{Deserialize, Serialize};

/// 一个 grant 的范围。跟着 [`crate::AuthContext`] 往下走，**不带钥匙本身**。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Grant {
    /// 随机 id，写进令牌里。不用名字：删了再建一个同名的，旧令牌不能跟着复活。
    pub id: String,
    pub name: String,
    /// 开放的成员 id（本地工作区或远端 ccnm 成员）。
    pub workspaces: Vec<String>,
    /// 只读：工具按 read-only 工具集取交集，远端只给读的那几个。
    #[serde(default)]
    pub read_only: bool,
    /// 浏览器专用短期地址发的。不落盘，查 grant 时按记录里有没有 `browser` 现算：
    /// 委派要分得出它（RFC-0009 第 5 节），而数据文件里本来就有这个事实，存两份迟早对不上。
    #[serde(skip)]
    pub browser: bool,
}

impl Grant {
    pub fn allows(&self, member_id: &str) -> bool {
        self.workspaces.iter().any(|id| id == member_id)
    }

    /// 能不能把活委派给本机 Agent（RFC-0009 第 5 节）：只有 `gld grant add --write` 发的。
    /// 只读的本来就调不到；浏览器地址嵌在网页扩展里、谁拿到 URL 谁就能用，带了 `--write` 也不给起全权的 Agent。
    pub fn may_delegate(&self) -> bool {
        !self.read_only && !self.browser
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use serde_json::json;

    #[test]
    fn grant_wire_keeps_defaults_without_serializing_browser_authority() {
        let mut grant: Grant = serde_json::from_value(json!({
            "id": "grant-id",
            "name": "grant-name",
            "workspaces": ["workspace-id"],
            "browser": true
        }))
        .unwrap();
        assert!(!grant.read_only);
        assert!(!grant.browser);

        grant.browser = true;
        assert_eq!(
            serde_json::to_value(&grant).unwrap(),
            json!({
                "id": "grant-id",
                "name": "grant-name",
                "workspaces": ["workspace-id"],
                "read_only": false
            })
        );
    }

    #[test]
    fn scope_membership_and_agent_delegation_are_explicit() {
        for (read_only, browser, may_delegate) in [
            (false, false, true),
            (false, true, false),
            (true, false, false),
            (true, true, false),
        ] {
            let grant = Grant {
                id: "grant-id".into(),
                name: "grant-name".into(),
                workspaces: vec!["workspace-id".into()],
                read_only,
                browser,
            };
            assert!(grant.allows("workspace-id"));
            assert!(!grant.allows("workspace"));
            assert_eq!(grant.may_delegate(), may_delegate);
        }
    }
}
