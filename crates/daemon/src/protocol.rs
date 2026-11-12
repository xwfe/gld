//! 本机 IPC 数据契约；实现由 gld-protocol 统一维护。

pub use gld_protocol::daemon::{
    DaemonInfo, ErrorCode, Request, Response, RpcError, FIRST_PROTOCOL_WITH_GENERATIONS,
    PROTOCOL_VERSION,
};
