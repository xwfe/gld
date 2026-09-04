//! 守护进程日志：带时间戳写到 stderr。
//!
//! `gld daemon start` 会把守护进程的 stderr 重定向到 `logs/daemon.log`，
//! 前台 `gld daemon run` 则直接打在终端上。不引入日志框架，够用就好。

use std::time::{SystemTime, UNIX_EPOCH};

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

/// `YYYY-MM-DDTHH:MM:SSZ`（UTC）。手写 civil-from-days，避免只为时间戳引一个 crate。
pub fn timestamp() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format_unix(secs)
}

pub fn format_unix(secs: u64) -> String {
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let (hour, minute, second) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let (year, month, day) = civil_from_days(days as i64);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

// Howard Hinnant 的 days → (y, m, d) 算法。
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_known_instants() {
        assert_eq!(format_unix(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_unix(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(format_unix(1_700_000_000), "2023-11-14T22:13:20Z");
    }
}
