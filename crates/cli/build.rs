//! Windows 上把 `gld` 主线程的栈设成 8 MiB，和 macOS / Linux 一样。
//!
//! Windows 主线程默认只有 1 MiB。debug 构建的栈帧大，`gld add` 在那里直接栈溢出
//! （退出码 0xC00000FD，stderr 只有一句 `thread 'main' has overflowed its stack`）：
//! 2026-09-25 第一次在 Windows CI 上真跑 gld 时 9 条端到端测试挂了 7 条。以前 Windows
//! 只做编译检查，从没真正运行过。release 构建栈帧小，未必触发，但栈余量同样只有别的平台的
//! 八分之一，一起改。只影响主线程；守护进程和 tokio 的工作线程各自有栈大小。

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    const STACK: u32 = 8 * 1024 * 1024;
    match std::env::var("CARGO_CFG_TARGET_ENV").as_deref() {
        Ok("msvc") => println!("cargo:rustc-link-arg-bins=/STACK:{STACK}"),
        Ok("gnu") => println!("cargo:rustc-link-arg-bins=-Wl,--stack,{STACK}"),
        _ => {}
    }
}
