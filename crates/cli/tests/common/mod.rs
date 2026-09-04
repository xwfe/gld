//! 集成测试共用的三件套。
//!
//! - [`env`]：拉起真实 `gld` 二进制的运行环境（独立 GLD_HOME + 临时项目目录）
//! - [`http`]：直接说 HTTP 的小客户端，绕开环境里的代理
//! - [`docs`]：校验文本里出现的 `gld ...` 是不是真命令
//!
//! 这个模块会被编进每个集成测试的二进制，而每个二进制只用到其中一部分，
//! 所以没用到的那部分必然触发 dead_code。这是 tests/common 的固有情况，
//! 在这里统一关掉（lint 级别会传到子模块）。
#![allow(dead_code)]

pub mod docs;
pub mod env;
pub mod http;
