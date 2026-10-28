//! 原生节点接入。当前只有传输适配；配对、鉴权和产品路由由后续切片接入。
//! 复用 bridge::peer 与现有 handle_request，不维护第二套 JSON-RPC 或消息恢复协议。

pub mod transport;
