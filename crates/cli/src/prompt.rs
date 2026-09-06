//! 交互式追问。
//!
//! 只在缺了一个"没有它就没法往下走"的值时用——比如 Cloudflare 命名隧道的
//! Tunnel Token。能从参数或已有配置拿到的东西不许在这里问：非交互环境
//! （CI、脚本、被别的程序调起来）问了也没人答，只会把命令挂死。

use std::io::{BufRead, IsTerminal, Write};

use crate::error::{CliError, CliResult};

/// 追问一个敏感值。终端上输入时不回显（Unix），读完换行。
///
/// 非交互环境不问，直接拿 `hint` 报错——所以 `hint` 必须自成一段完整的话：
/// 缺的是什么、不交互的话该怎么给。`question` 是交互时的提示语，末尾通常带
/// 冒号，直接当报错标题读起来不像句子。
pub fn secret(question: &str, hint: &str) -> CliResult<String> {
    if !std::io::stdin().is_terminal() {
        return Err(CliError::new(hint));
    }
    eprint!("{question} ");
    std::io::stderr().flush()?;

    let _echo = EchoOff::engage();
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer)?;
    // 关了回显的话，用户按下的回车也没显示出来，这里补一个换行，
    // 否则后面的输出会接在提示语屁股后面。
    eprintln!();
    Ok(answer.trim().to_string())
}

/// 在作用域内关掉终端回显，析构时恢复原状（包括中途 `?` 提前返回）。
///
/// Unix 走 termios。其它平台拿不到这个开关，输入会显示在屏幕上——
/// 值仍然读得到，只是会留在终端回滚里，所以那边额外提示一句。
struct EchoOff {
    #[cfg(unix)]
    restore: Option<libc::termios>,
}

impl EchoOff {
    #[cfg(unix)]
    fn engage() -> Self {
        use std::os::fd::AsRawFd;

        let fd = std::io::stdin().as_raw_fd();
        let mut current = unsafe { std::mem::zeroed::<libc::termios>() };
        if unsafe { libc::tcgetattr(fd, &mut current) } != 0 {
            return Self { restore: None };
        }
        let mut quiet = current;
        quiet.c_lflag &= !libc::ECHO;
        if unsafe { libc::tcsetattr(fd, libc::TCSAFLUSH, &quiet) } != 0 {
            return Self { restore: None };
        }
        Self {
            restore: Some(current),
        }
    }

    #[cfg(not(unix))]
    fn engage() -> Self {
        eprintln!("（这个终端上输入的内容会显示出来）");
        Self {}
    }
}

impl Drop for EchoOff {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(original) = self.restore.take() {
            use std::os::fd::AsRawFd;
            let fd = std::io::stdin().as_raw_fd();
            unsafe { libc::tcsetattr(fd, libc::TCSAFLUSH, &original) };
        }
    }
}
