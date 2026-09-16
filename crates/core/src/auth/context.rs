//! 一条请求是**怎么通过鉴权的**，验完之后带着往下走。
//!
//! 在这之前，鉴权只决定放行还是 401，进到 hub 里就什么都不剩了。远端成员
//! 需要这份东西：RFC-0002 5.3「不能只按 workspace ID 缓存长期 bridge」——
//! 一条通往远端的连接属于某个主体，不是属于某个工作区名字。
//!
//! ## 它不是身份
//!
//! **这里说的是「这次请求过了哪种鉴权」，不是「这是哪个自然人」。**gld 没有
//! 多用户目录：
//!
//! - bearer 是一串所有客户端共用的密钥，谁拿到谁就是"通过"；
//! - OAuth 那边验的也是同一个操作员口令，签出来的令牌里只有注册过的
//!   `client_id`，没有 `sub`。`client_id` 是一个 MCP 客户端，不是一个人。
//!
//! 所以任何地方都不能拿它做人与人之间的隔离，也不能对外宣传多租户
//! （RFC-0002 5.3 原话：「共享 bearer 密钥代表共享授权，不承诺识别不同
//! 自然人」）。它能做的是两件实在事：**把不同主体的远端连接分开**，
//! 以及**把「这次有没有过鉴权」这个事实往下传**。

/// 已经通过鉴权的调用主体。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Principal {
    /// 没配鉴权（`auth_type = noauth`）。端口开着谁都能连。
    Anonymous,
    /// 过了共享 bearer 密钥。
    SharedSecret,
    /// 过了 OAuth 访问令牌。`client_id` 来自令牌里已验签的 claim。
    OAuthClient { client_id: String },
}

/// 一条请求验完鉴权之后剩下的东西。
///
/// **不存令牌**。构造它的地方拿得到原始令牌，但那串东西到这里就该被丢掉：
/// 它会进日志、进错误消息、进连接代次的字符串（验收项 H02「原始凭据不进
/// 输出/日志」）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthContext {
    principal: Principal,
    /// 这道鉴权守的是哪个入口：`hub`，或者某个工作区 id。
    scope: String,
}

impl AuthContext {
    pub fn new(principal: Principal, scope: impl Into<String>) -> Self {
        AuthContext {
            principal,
            scope: scope.into(),
        }
    }

    /// 没配鉴权的入口。
    pub fn anonymous(scope: impl Into<String>) -> Self {
        Self::new(Principal::Anonymous, scope)
    }

    pub fn principal(&self) -> &Principal {
        &self.principal
    }

    pub fn scope(&self) -> &str {
        &self.scope
    }

    /// 这次请求确实过了一道鉴权。
    ///
    /// RFC-0002 5.3：第一版 remote coding 要求可验证的认证身份，noauth 不开放
    /// 该能力。只读不受这条限制——操作员选了 noauth 就是自己决定把端口敞开。
    pub fn is_authenticated(&self) -> bool {
        !matches!(self.principal, Principal::Anonymous)
    }

    /// 写进日志、错误和连接代次的短标识。**永远不含令牌本身。**
    ///
    /// OAuth 那支带上 client_id：不同的注册客户端拿到不同的远端连接，
    /// 而不是共用一条按 workspace id 缓存的。
    pub fn tag(&self) -> String {
        match &self.principal {
            Principal::Anonymous => format!("noauth:{}", self.scope),
            Principal::SharedSecret => format!("bearer:{}", self.scope),
            Principal::OAuthClient { client_id } => {
                format!("oauth:{}:{client_id}", self.scope)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_noauth_counts_as_unauthenticated() {
        assert!(!AuthContext::anonymous("hub").is_authenticated());
        assert!(AuthContext::new(Principal::SharedSecret, "hub").is_authenticated());
        assert!(AuthContext::new(
            Principal::OAuthClient {
                client_id: "c1".into()
            },
            "hub"
        )
        .is_authenticated());
    }

    /// 不同主体的标识必须不同，否则远端连接会被两个主体共用。
    #[test]
    fn different_principals_get_different_tags() {
        let shared = AuthContext::new(Principal::SharedSecret, "hub").tag();
        let one = AuthContext::new(
            Principal::OAuthClient {
                client_id: "chatgpt".into(),
            },
            "hub",
        )
        .tag();
        let other = AuthContext::new(
            Principal::OAuthClient {
                client_id: "claude".into(),
            },
            "hub",
        )
        .tag();
        assert_ne!(one, other);
        assert_ne!(one, shared);
        assert_ne!(shared, AuthContext::anonymous("hub").tag());
    }

    /// 同一个入口、同一种鉴权，标识要稳定——否则每次请求都重开一条 bridge。
    #[test]
    fn the_same_principal_on_the_same_scope_is_stable() {
        let tag = || AuthContext::new(Principal::SharedSecret, "hub").tag();
        assert_eq!(tag(), tag());
        assert_ne!(
            tag(),
            AuthContext::new(Principal::SharedSecret, "ws-1").tag()
        );
    }
}
