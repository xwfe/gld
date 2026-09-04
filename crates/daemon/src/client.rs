//! 命令行侧的 IPC 客户端：连上 socket，发一行请求，读一行响应。

use std::time::Duration;

use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::io::BufReader;

use crate::ipc;
use crate::lifecycle::DaemonPaths;
use crate::protocol::{DaemonInfo, Request, Response, RpcError};

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// 连不上 socket：守护进程没在跑（或 socket 路径不对）。
    #[error("守护进程未运行（{0}）")]
    NotRunning(std::io::Error),
    #[error("等待守护进程响应超时（{0:?}）")]
    Timeout(Duration),
    #[error("与守护进程通信失败：{0}")]
    Io(std::io::Error),
    #[error("守护进程返回了无法解析的数据：{0}")]
    Malformed(String),
    #[error("{0}")]
    Rpc(RpcError),
}

#[derive(Debug, Clone)]
pub struct Client {
    paths: DaemonPaths,
    timeout: Duration,
}

impl Client {
    pub fn new(paths: DaemonPaths) -> Self {
        Self {
            paths,
            timeout: Duration::from_secs(30),
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn paths(&self) -> &DaemonPaths {
        &self.paths
    }

    /// 发送请求，返回守护进程给的 JSON 结果。
    pub async fn call(&self, request: &Request) -> Result<Value, ClientError> {
        let deadline = tokio::time::sleep(self.timeout);
        tokio::pin!(deadline);
        tokio::select! {
            result = self.call_inner(request) => result,
            _ = &mut deadline => Err(ClientError::Timeout(self.timeout)),
        }
    }

    /// 发送请求并把结果反序列化成具体类型。
    pub async fn call_typed<T: DeserializeOwned>(
        &self,
        request: &Request,
    ) -> Result<T, ClientError> {
        let value = self.call(request).await?;
        serde_json::from_value(value).map_err(|error| ClientError::Malformed(error.to_string()))
    }

    pub async fn daemon_info(&self) -> Result<DaemonInfo, ClientError> {
        self.call_typed(&Request::DaemonInfo).await
    }

    async fn call_inner(&self, request: &Request) -> Result<Value, ClientError> {
        let mut stream = ipc::connect(&self.paths.socket)
            .await
            .map_err(ClientError::NotRunning)?;
        let line = serde_json::to_string(request)
            .map_err(|error| ClientError::Malformed(error.to_string()))?;
        let (read_half, mut write_half) = tokio::io::split(&mut stream);
        ipc::write_line(&mut write_half, &line)
            .await
            .map_err(ClientError::Io)?;
        let mut reader = BufReader::new(read_half);
        let reply = ipc::read_line(&mut reader)
            .await
            .map_err(ClientError::Io)?
            .ok_or_else(|| ClientError::Malformed("连接被守护进程关闭，没有收到响应".into()))?;
        let response: Response = serde_json::from_str(&reply)
            .map_err(|error| ClientError::Malformed(format!("{error}: {reply}")))?;
        response.into_result().map_err(ClientError::Rpc)
    }
}
