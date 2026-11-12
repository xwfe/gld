//! 文件写入判定的兼容入口，规则由 `gld-policy` 统一实现。

pub use gld_policy::write_class::{is_git_internal, Verdict, WriteClass};
