//! 本机服务和隧道请求共用的选择类型；资源定位由应用侧适配。

/// 一个工作区可运行的两种服务。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ServiceKind {
    Mcp,
    Actions,
}

impl ServiceKind {
    pub const ALL: [ServiceKind; 2] = [ServiceKind::Mcp, ServiceKind::Actions];

    pub fn parse(value: &str) -> Result<Self, ParseServiceKindError> {
        match value.trim().to_ascii_lowercase().as_str() {
            "mcp" => Ok(Self::Mcp),
            "actions" | "action" => Ok(Self::Actions),
            other => Err(ParseServiceKindError(other.to_owned())),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mcp => "mcp",
            Self::Actions => "actions",
        }
    }

    /// 面向用户的中文标签（带尾随空格，便于拼接“本地 MCP 端口 …”）。
    pub fn label(self) -> &'static str {
        match self {
            Self::Mcp => "本地 MCP ",
            Self::Actions => "本地 Actions ",
        }
    }

    pub fn tunnel_kind(self) -> TunnelServiceKind {
        match self {
            Self::Mcp => TunnelServiceKind::Mcp,
            Self::Actions => TunnelServiceKind::Actions,
        }
    }

    pub fn stderr_log_name(self) -> &'static str {
        match self {
            Self::Mcp => "stderr.log",
            Self::Actions => "actions-stderr.log",
        }
    }
}

impl std::fmt::Display for ServiceKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TunnelServiceKind {
    Mcp,
    Actions,
}

impl TunnelServiceKind {
    pub fn parse(service: &str) -> Result<Self, ParseServiceKindError> {
        ServiceKind::parse(service).map(ServiceKind::tunnel_kind)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mcp => "mcp",
            Self::Actions => "actions",
        }
    }
}

/// 服务名无法识别；应用侧可将其转换为自己的错误类型。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseServiceKindError(String);

impl std::fmt::Display for ParseServiceKindError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "未知服务「{}」（可选 mcp | actions）", self.0)
    }
}

impl std::error::Error for ParseServiceKindError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_aliases_and_errors_keep_their_spelling() {
        for input in ["actions", "action", " ACTION "] {
            assert_eq!(ServiceKind::parse(input).unwrap(), ServiceKind::Actions);
            assert_eq!(
                TunnelServiceKind::parse(input).unwrap(),
                TunnelServiceKind::Actions
            );
        }
        assert_eq!(ServiceKind::parse(" MCP ").unwrap(), ServiceKind::Mcp);
        assert_eq!(
            TunnelServiceKind::parse("mcp").unwrap(),
            TunnelServiceKind::Mcp
        );
        assert_eq!(
            ServiceKind::parse(" Other ").unwrap_err().to_string(),
            "未知服务「other」（可选 mcp | actions）"
        );
        assert_eq!(
            TunnelServiceKind::parse(" Other ").unwrap_err().to_string(),
            "未知服务「other」（可选 mcp | actions）"
        );
    }
}
