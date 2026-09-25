//! 一次工具调用是谁发起的。
//!
//! 为什么要有这么个东西：命令会话（`exec_command` 起的后台命令，以及拿着
//! `session_id` 去读的 `read_output` / `write_stdin` / `kill_session`）按
//! **目录 + 调用方**分表。没有调用方这一半的话，一个入口上所有连接共用一张
//! 表——经 hub 连进来的两个 OAuth 客户端，A 拿着 B 的 `session_id` 就能读到
//! B 的命令输出，那是实打实的越权。
//!
//! 主体从哪儿来：两条 MCP 路和 GPT Actions 都是监听器验完鉴权得到的
//! [`crate::auth::AuthContext`]（Actions 的入口名是 `actions`）；命令行 `gld tool call`、
//! 守护进程内部调用和测试是 [`Caller::local`]。**网络进来的请求不能落到 `local`**：
//! Actions 以前就是，和命令行互相读得到对方的命令输出。
//!
//! # 分得开什么、分不开什么
//!
//! - **分得开**：同一个入口上的不同 OAuth 客户端（`client_id` 进 key）。
//! - **分得开**：hub 和工作区自己的监听器。两者的凭据本来就不共用
//!   （见 [`crate::hub::HUB_SECRET_KEYS`]：工作区凭据泄露不该连带打开
//!   hub），所以拿着工作区令牌的人读不到经 hub 起的命令，这是对的。
//! - **分不开**：同一个入口上的多个匿名连接（`auth_type=noauth`）。匿名就是
//!   没身份，`noauth:<入口>` 是同一个主体。操作员选了 noauth 就是自己决定把
//!   这个端口敞开，会话也一样敞开。
//! - **分不开**：同一个入口上共用一条 bearer 令牌的多个客户端。共享密钥本来
//!   就是一个主体，要分得开就得给每个客户端单独注册 OAuth 客户端。

use crate::auth::AuthContext;

/// 命令行、守护进程内部调用和测试用的入口名。
///
/// 不会跟鉴权那边的标识撞上：那些一律带冒号（`noauth:…` / `bearer:…` /
/// `oauth:…`），本机这支两段都是纯 `local`。
pub const LOCAL_SCOPE: &str = "local";

/// 一次工具调用的主体。
///
/// 会话表按它分，所以两个 `Caller` 相等 = 两次调用看得见彼此的命令会话。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Caller {
    key: String,
    scope: String,
    /// 用 grant 的钥匙进来的，记下是哪个：`gld grant rm` 要按它停命令（RFC-0007）。
    /// 它已经在 `key` 里了，单独放一格是为了不去拆字符串。
    grant: Option<String>,
}

impl Caller {
    /// 本机操作员：命令行 `gld tool call`、守护进程内部调用、测试。
    ///
    /// 和任何网络连接都不是同一个主体——本机能跑 gld 的人权限本来就更高，
    /// 把两者并成一个会让"匿名连上来的客户端"读到命令行起的命令输出。
    pub fn local() -> Self {
        Self {
            key: LOCAL_SCOPE.to_string(),
            scope: LOCAL_SCOPE.to_string(),
            grant: None,
        }
    }

    /// 一条过了鉴权的 MCP 连接。
    ///
    /// 用 [`AuthContext::tag`] 当 key：它已经是"写进日志和远端连接代次"的那个
    /// 主体标识（远端 bridge 按它分连接，RFC-0002 5.3），会话表跟它走口径一致。
    /// **它永远不含令牌本身**，所以拿它当 HashMap 的键不会把凭据留在内存里的
    /// 第二个地方。
    pub fn from_auth(auth: &AuthContext) -> Self {
        Self {
            key: auth.tag(),
            scope: auth.scope().to_string(),
            grant: auth.grant().map(|grant| grant.id.clone()),
        }
    }

    pub fn grant_id(&self) -> Option<&str> {
        self.grant.as_deref()
    }

    /// 主体的标识，也写进运行记录（[`crate::tools::runs::RunRecord::caller`]）：重启之后
    /// 同一个主体读得到自己的记录，别的主体读不到。不含令牌本身。
    pub fn key(&self) -> &str {
        &self.key
    }

    /// 哪个入口：`hub`、某个工作区 id，或者 [`LOCAL_SCOPE`]。
    ///
    /// 停掉一个入口时按它清会话，见
    /// [`crate::tools::workspace_runtime::WorkspaceRuntime::terminate_sessions_in_scope`]。
    pub fn scope(&self) -> &str {
        &self.scope
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Principal;

    /// 本机那支绝不能和任何一种网络主体撞上。撞上的后果是匿名连接读得到
    /// 命令行起的命令输出。
    #[test]
    fn the_local_caller_is_never_a_network_caller() {
        let local = Caller::local();
        for auth in [
            AuthContext::anonymous(LOCAL_SCOPE),
            AuthContext::new(Principal::SharedSecret, LOCAL_SCOPE),
            AuthContext::new(
                Principal::OAuthClient {
                    client_id: LOCAL_SCOPE.into(),
                },
                LOCAL_SCOPE,
            ),
        ] {
            assert_ne!(
                local,
                Caller::from_auth(&auth),
                "本机主体和 {} 认成了同一个人",
                auth.tag()
            );
        }
    }

    /// 同一个入口上的两个 OAuth 客户端是两个主体——这是这次分表要挡的那件事。
    #[test]
    fn two_oauth_clients_on_one_entry_are_two_callers() {
        let client = |id: &str| {
            Caller::from_auth(&AuthContext::new(
                Principal::OAuthClient {
                    client_id: id.into(),
                },
                "hub",
            ))
        };
        assert_ne!(client("alpha"), client("beta"));
        assert_eq!(client("alpha"), client("alpha"), "同一个客户端重连换了主体");
    }

    /// 同一种凭据、两个入口，也是两个主体：hub 的令牌和工作区的令牌不共用。
    #[test]
    fn the_same_credential_kind_on_two_entries_is_two_callers() {
        let bearer =
            |scope: &str| Caller::from_auth(&AuthContext::new(Principal::SharedSecret, scope));
        assert_ne!(bearer("hub"), bearer("9f2c"));
        assert_eq!(bearer("hub").scope(), "hub");
    }
}
