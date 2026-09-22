//! 起一个真服务、按项目调工具的辅助。
//!
//! RFC-0004 之后只有一个 MCP 服务，项目挂在它下面：客户端连的是服务的端口，
//! 每次调用用 `workspace` 参数选项目。以前各测试自己拼"项目端口 + 项目 token"，
//! 现在统一走这里，免得每个文件各写一版、哪天调用形状变了只改到一处。

use serde_json::Value;

use super::env::{free_port, Env};
use super::http::{post_json, Reply};

/// 一个跑起来的服务，和测试项目在服务里的名字。
pub struct Service {
    pub port: u16,
    /// bearer 认证时的 token；noauth 时是空串。
    pub token: String,
    pub workspace: String,
}

impl Env {
    /// 把测试项目以 `name` 加进服务，认证设成 `auth`，服务起在一个空闲端口上。
    pub fn serve(&self, name: &str, auth: &str) -> Service {
        let port = free_port();
        self.ok(&["add", ".", "--name", name]);
        self.ok(&["upgrade", "--port", &port.to_string(), "--auth", auth]);
        let token = if auth == "bearer" {
            self.service_secret("bearer_token")
        } else {
            String::new()
        };
        self.ok(&["start"]);
        Service {
            port,
            token,
            workspace: name.to_string(),
        }
    }

    /// 服务的一项凭据（明文）。
    pub fn service_secret(&self, key: &str) -> String {
        self.json(&["--json", "secret", "ls", key, "--reveal"])[0]["value"]
            .as_str()
            .unwrap_or_else(|| panic!("服务凭据 {key} 读不出来"))
            .to_string()
    }
}

impl Service {
    fn bearer(&self) -> Option<&str> {
        (!self.token.is_empty()).then_some(self.token.as_str())
    }

    /// 发一条 JSON-RPC，返回原始回包。
    pub fn rpc(&self, method: &str, params: Value) -> Reply {
        let body = serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": method, "params": params
        })
        .to_string();
        post_json(self.port, "/mcp", &body, self.bearer())
    }

    /// 对测试项目调一个工具（自动带上 `workspace`），返回工具自己的结构化结果。
    pub fn call_tool(&self, name: &str, mut arguments: Value) -> Value {
        arguments["workspace"] = Value::String(self.workspace.clone());
        self.call_raw(name, arguments)
    }

    /// 同上，但参数原样发出去（测"不带 workspace"之类的情况用）。
    pub fn call_raw(&self, name: &str, arguments: Value) -> Value {
        let reply = self.rpc(
            "tools/call",
            serde_json::json!({ "name": name, "arguments": arguments }),
        );
        assert_eq!(reply.status, 200, "HTTP 层就失败了：{}", reply.body);
        let text = reply.json()["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("没有工具结果：{}", reply.body))
            .to_string();
        serde_json::from_str(&text).expect("工具结果是 JSON")
    }
}
