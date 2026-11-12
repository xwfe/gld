//! 确定性的调用主体、授权范围与写入判定。
//!
//! 本层只处理显式传入的数据，不读文件、不起进程、不访问网络或全局状态。
//! 凭据验证、持久化与时间检查由调用侧负责；构造或反序列化这些类型不代表身份已验。
//! 调用侧必须在完成鉴权后建立 [`AuthContext`]，并保留原有的主体与作用域语义。

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

mod caller;
mod context;
mod grant;
pub mod write_class;

pub use caller::{Caller, LOCAL_SCOPE};
pub use context::{AuthContext, Principal};
pub use grant::Grant;
