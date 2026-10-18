//! 测试取端口的 `free_port` 不发 gld 自己的默认端口（审查 §36）。
//!
//! 那一段（hub 28764、全局入口 28765、工作区 MCP 从 28766 往上）写进配置时还没人在听，
//! "连一下"查不出来；发出去之后测试再把它手动设给别的服务，gld 就报端口重复。
mod common;

use common::env::free_port;

#[test]
fn free_port_never_hands_out_gld_default_ports() {
    // 一轮正好把 20000–32000 走一遍：老实现必然经过 28765。
    for _ in 0..12_000 {
        let port = free_port();
        assert!(
            !(28_700..=28_899).contains(&port),
            "发了 gld 的默认端口 {port}"
        );
    }
}
