//! `gld` 二进制入口。实现都在 lib（`crates/cli/src/lib.rs`）里。

use clap::Parser;
use gld::{Cli, CliError, ExitCode};

/// 把 SIGPIPE 恢复成系统默认行为。
///
/// Rust 启动时会把 SIGPIPE 设成忽略，于是管道被提前关掉时——`gld completions zsh | head`、
/// `gld ws fields | less` 只看几行就退出——往 stdout 写会变成 EPIPE 错误，
/// `println!` 和 clap_complete 都是直接 panic，用户看到的是一大段 Rust 崩溃信息，
/// 看着像 gld 出了 bug，其实只是管道正常关闭了。
///
/// 恢复默认后进程会被 SIGPIPE 安静杀掉，跟 `ls | head` 一样。
#[cfg(unix)]
fn quit_quietly_on_broken_pipe() {
    // SAFETY: 在 main 最开头调用，此时还没有别的线程在跑。
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
}

#[cfg(not(unix))]
fn quit_quietly_on_broken_pipe() {}

fn main() {
    quit_quietly_on_broken_pipe();
    let cli = Cli::parse();
    // `--home` 必须在任何数据目录解析之前生效。
    if let Some(home) = &cli.global.home {
        std::env::set_var(gld_core::home::HOME_ENV, home);
    }

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("gld-cli")
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("错误：无法初始化异步运行时：{error}");
            std::process::exit(ExitCode::Failure.code());
        }
    };

    let outcome = runtime.block_on(gld::run(cli));
    // 直连模式下可能有后台任务（端口探测等）还挂着，不等它们。
    runtime.shutdown_timeout(std::time::Duration::from_millis(200));

    match outcome {
        Ok(()) => {}
        Err(CliError { message, exit }) => {
            if !message.is_empty() {
                eprintln!("错误：{message}");
            }
            std::process::exit(exit.code());
        }
    }
}
