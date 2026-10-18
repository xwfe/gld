//! 运行日志的写入与轮转。
//!
//! 每个工作区在 `~/.config/gld/logs/<workspace-id>/` 下有几个日志文件：
//! `mcp-requests.log`（每次 MCP 请求 2-4 行，被鉴权挡下的 1 行）、`stdout.log`、`stderr.log`，
//! 用了隧道还会有 `frpc-*.log` / `cloudflared.log`。
//!
//! 守护进程会连续跑几周，这些文件必须有上限，否则一个话痨的客户端能把
//! 磁盘写满。策略是最简单的一代轮转：单个文件超过 [`MAX_LOG_BYTES`] 就
//! 改名为 `<名字>.1`（覆盖上一代），然后从空文件继续写。所以每个日志
//! 名字最多占 2 × [`MAX_LOG_BYTES`]。
//!
//! 轮转发生在写入之前，用的是已打开句柄的大小，不是先 stat 再决定：
//! 多个进程同时写同一个文件时，前者可能多写几 KB 才触发轮转，这没关系；
//! 但绝不会因为看到过期的大小而漏掉轮转。
//!
//! Unix 上如果轮转时另一个线程正持有旧句柄，它接下来的几行会落进 `.1`。
//! 日志不是账本，丢几行到上一代文件里可以接受，换来的是不用加锁。

use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

/// 单个日志文件的上限。超过后轮转，只保留一代。
pub const MAX_LOG_BYTES: u64 = 4 * 1024 * 1024;

/// 某个工作区的日志目录。
pub fn log_dir_for_profile(profile_id: &str) -> PathBuf {
    crate::home::logs_dir()
        .map(|logs| logs.join(profile_id))
        .unwrap_or_else(|_| PathBuf::from("logs").join(profile_id))
}

/// 往工作区日志追加一行。写不进去就静默放弃——记日志失败不该影响正在跑的服务。
///
/// 每行前面加 UTC 时间（`2026-10-18T02:05:54Z `）。以前没有，事后查"那次调用是几点"只能
/// 拿行号去和别的记录对（审查 §28）。
pub fn append_profile_log(profile_id: &str, file_name: &str, line: &str) {
    let log_dir = log_dir_for_profile(profile_id);
    if fs::create_dir_all(&log_dir).is_err() {
        return;
    }
    append_line(
        &log_dir.join(file_name),
        &format!("{} {line}", utc_timestamp()),
    );
}

/// `YYYY-MM-DDTHH:MM:SSZ`（UTC）。手写 civil-from-days，避免只为时间戳引一个 crate；
/// 守护进程日志也用它（`gld-daemon` 的 `logging::timestamp`）。
pub fn utc_timestamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
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

/// 往指定文件追加一行，必要时先轮转。
pub fn append_line(path: &Path, line: &str) {
    let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) else {
        return;
    };
    if file.metadata().map(|meta| meta.len()).unwrap_or(0) >= MAX_LOG_BYTES {
        drop(file);
        rotate(path);
        let Ok(reopened) = OpenOptions::new().create(true).append(true).open(path) else {
            return;
        };
        file = reopened;
    }
    let _ = writeln!(file, "{line}");
}

/// 文件超过上限就轮转。给守护进程自己的日志用：它的 stdout / stderr 是
/// 启动时重定向过去的，进程内没法在写入时插手，只能在每次启动前检查。
pub fn rotate_if_oversized(path: &Path) {
    let oversized = fs::metadata(path)
        .map(|meta| meta.len() >= MAX_LOG_BYTES)
        .unwrap_or(false);
    if oversized {
        rotate(path);
    }
}

fn rotate(path: &Path) {
    let mut rotated = OsString::from(path.as_os_str());
    rotated.push(".1");
    // rename 会覆盖已存在的 .1，正是我们要的“只保留一代”。
    let _ = fs::rename(path, PathBuf::from(rotated));
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

    fn write_bytes(path: &Path, count: usize) {
        fs::write(path, "x".repeat(count)).expect("write");
    }

    #[test]
    fn appends_without_rotating_below_the_cap() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("a.log");
        append_line(&path, "first");
        append_line(&path, "second");
        assert_eq!(fs::read_to_string(&path).unwrap(), "first\nsecond\n");
        assert!(!temp.path().join("a.log.1").exists());
    }

    #[test]
    fn rotates_once_the_file_reaches_the_cap() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("big.log");
        write_bytes(&path, MAX_LOG_BYTES as usize);

        append_line(&path, "after rotation");

        // 新文件只剩这一行，老内容整体搬到 .1。
        assert_eq!(fs::read_to_string(&path).unwrap(), "after rotation\n");
        let rotated = temp.path().join("big.log.1");
        assert_eq!(
            fs::metadata(&rotated).unwrap().len(),
            MAX_LOG_BYTES,
            "上一代应保留完整内容"
        );
    }

    #[test]
    fn only_one_generation_is_kept() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("c.log");

        write_bytes(&path, MAX_LOG_BYTES as usize);
        append_line(&path, "gen-1");
        write_bytes(&path, MAX_LOG_BYTES as usize);
        append_line(&path, "gen-2");

        assert_eq!(fs::read_to_string(&path).unwrap(), "gen-2\n");
        // 第二次轮转覆盖了第一次的 .1；不会出现 .2。
        assert!(!temp.path().join("c.log.2").exists());
        assert_eq!(
            fs::metadata(temp.path().join("c.log.1")).unwrap().len(),
            MAX_LOG_BYTES
        );
    }

    #[test]
    fn rotate_if_oversized_leaves_small_files_alone() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("d.log");
        write_bytes(&path, 16);
        rotate_if_oversized(&path);
        assert!(!temp.path().join("d.log.1").exists());

        write_bytes(&path, MAX_LOG_BYTES as usize);
        rotate_if_oversized(&path);
        assert!(temp.path().join("d.log.1").exists());
        assert!(!path.exists(), "轮转后原文件由下一次写入重建");
    }

    #[test]
    fn a_missing_directory_is_ignored_instead_of_panicking() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("no-such-dir").join("e.log");
        append_line(&path, "dropped");
        assert!(!path.exists());
    }
}
