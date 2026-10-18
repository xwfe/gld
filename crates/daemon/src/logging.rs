//! 守护进程日志：带时间戳写到 stderr。
//!
//! `gld daemon start` 会把守护进程的 stderr 重定向到 `logs/daemon.log`，
//! 前台 `gld daemon run` 则直接打在终端上。不引入日志框架，够用就好。

#[derive(Debug, Clone, Copy)]
pub enum Level {
    Info,
    Warn,
    Error,
}

impl Level {
    fn tag(self) -> &'static str {
        match self {
            Level::Info => "INFO",
            Level::Warn => "WARN",
            Level::Error => "ERROR",
        }
    }
}

pub fn log(level: Level, message: impl AsRef<str>) {
    eprintln!("{} {:5} {}", timestamp(), level.tag(), message.as_ref());
}

pub fn info(message: impl AsRef<str>) {
    log(Level::Info, message);
}

pub fn warn(message: impl AsRef<str>) {
    log(Level::Warn, message);
}

pub fn error(message: impl AsRef<str>) {
    log(Level::Error, message);
}

/// `YYYY-MM-DDTHH:MM:SSZ`（UTC），和 MCP 请求日志用同一个格式（`gld_core::logs`）。
pub fn timestamp() -> String {
    gld_core::logs::utc_timestamp()
}

#[cfg(test)]
mod tests {
    #[test]
    fn timestamps_are_utc_seconds() {
        let stamp = super::timestamp();
        assert_eq!(stamp.len(), "2023-11-14T22:13:20Z".len(), "{stamp}");
        assert!(stamp.ends_with('Z'), "{stamp}");
    }
}
