//! 终端输出：表格、键值对、状态着色、`--json` 直出。
//!
//! 着色只在 stdout 是终端且未设置 `NO_COLOR` 时生效；`--json` 模式下所有
//! 命令都只打印一份 JSON，方便脚本消费。

use std::io::IsTerminal;

use serde::Serialize;
use unicode_width::UnicodeWidthStr;

#[derive(Debug, Clone, Copy)]
pub struct Output {
    pub json: bool,
    color: bool,
}

impl Output {
    pub fn new(json: bool, no_color: bool) -> Self {
        let color = !json
            && !no_color
            && std::env::var_os("NO_COLOR").is_none()
            && std::io::stdout().is_terminal();
        Self { json, color }
    }

    /// `--json` 时打印 JSON 并返回 true；否则返回 false 让调用方走人类可读渲染。
    pub fn json_or<T: Serialize>(&self, value: &T) -> bool {
        if self.json {
            match serde_json::to_string_pretty(value) {
                Ok(text) => println!("{text}"),
                Err(error) => eprintln!("错误：无法序列化输出：{error}"),
            }
        }
        self.json
    }

    pub fn line(&self, text: impl AsRef<str>) {
        if !self.json {
            println!("{}", text.as_ref());
        }
    }

    /// 提示信息走 stderr，不污染 stdout（尤其是 `--json` 时）。
    pub fn note(&self, text: impl AsRef<str>) {
        eprintln!("{}", self.dim(text.as_ref()));
    }

    /// 键名接受 `&str` 与 `String`：详情里的凭据标签是拼出来的（要缩进对齐）。
    pub fn kv<K: AsRef<str>>(&self, rows: &[(K, String)]) {
        if self.json {
            return;
        }
        let width = rows
            .iter()
            .map(|(key, _)| UnicodeWidthStr::width(key.as_ref()))
            .max()
            .unwrap_or(0);
        for (key, value) in rows {
            let key = key.as_ref();
            let pad = width - UnicodeWidthStr::width(key);
            let mut lines = value.lines();
            let first = lines.next().unwrap_or("");
            println!("{}{}  {}", self.dim(key), " ".repeat(pad), first);
            for continuation in lines {
                println!("{}  {}", " ".repeat(width), continuation);
            }
        }
    }

    pub fn table(&self, headers: &[&str], rows: &[Vec<String>]) {
        if self.json {
            return;
        }
        let columns = headers.len();
        let mut widths: Vec<usize> = headers.iter().map(|h| UnicodeWidthStr::width(*h)).collect();
        for row in rows {
            for (index, cell) in row.iter().enumerate().take(columns) {
                widths[index] =
                    widths[index].max(UnicodeWidthStr::width(strip_ansi(cell).as_str()));
            }
        }
        let render = |cells: &[String], bold: bool| {
            let mut line = String::new();
            for (index, cell) in cells.iter().enumerate().take(columns) {
                let pad = widths[index] - UnicodeWidthStr::width(strip_ansi(cell).as_str());
                let text = if bold { self.bold(cell) } else { cell.clone() };
                line.push_str(&text);
                if index + 1 < columns {
                    line.push_str(&" ".repeat(pad + 2));
                }
            }
            line.trim_end().to_string()
        };
        let header_cells: Vec<String> = headers.iter().map(|h| h.to_string()).collect();
        println!("{}", render(&header_cells, true));
        for row in rows {
            println!("{}", render(row, false));
        }
    }

    pub fn state(&self, state: &str) -> String {
        match state {
            "running" => self.green(state),
            "starting" | "stopping" => self.yellow(state),
            "error" => self.red(state),
            _ => self.dim(state),
        }
    }

    pub fn ok_mark(&self, ok: bool) -> String {
        if ok {
            self.green("✓")
        } else {
            self.red("✗")
        }
    }

    pub fn bold(&self, text: &str) -> String {
        self.paint("1", text)
    }

    pub fn dim(&self, text: &str) -> String {
        self.paint("2", text)
    }

    pub fn green(&self, text: &str) -> String {
        self.paint("32", text)
    }

    pub fn yellow(&self, text: &str) -> String {
        self.paint("33", text)
    }

    pub fn red(&self, text: &str) -> String {
        self.paint("31", text)
    }

    fn paint(&self, code: &str, text: &str) -> String {
        if self.color {
            format!("\x1b[{code}m{text}\x1b[0m")
        } else {
            text.to_string()
        }
    }
}

fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            for next in chars.by_ref() {
                if next == 'm' {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// 密钥脱敏：保留前后各 4 位。
pub fn mask(secret: &str) -> String {
    let chars: Vec<char> = secret.chars().collect();
    if chars.len() <= 8 {
        return "*".repeat(chars.len().max(4));
    }
    let head: String = chars[..4].iter().collect();
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("{head}…{tail}")
}

pub fn yes_no(value: bool) -> &'static str {
    if value {
        "是"
    } else {
        "否"
    }
}

pub fn or_dash(value: &str) -> String {
    if value.trim().is_empty() {
        "-".into()
    } else {
        value.to_string()
    }
}

/// 把历史档案里的时间戳渲染成人能读的。
///
/// 存在文件里的是 `unix:1788485624` 这种内部编码（改它会动到已经写下的
/// markdown，所以只在显示这一层处理）。直接把它打到表格里，读的人得自己
/// 去换算——列表里真正想知道的其实只是"多久以前"。
/// 认不出来的格式原样返回，不猜。
pub fn human_time(raw: &str) -> String {
    let Some(seconds) = raw
        .strip_prefix("unix:")
        .and_then(|value| value.trim().parse::<u64>().ok())
    else {
        return raw.to_string();
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(seconds);
    // 时钟回拨或档案来自别的机器时可能是未来时间，别显示成负数。
    let ago = now.saturating_sub(seconds);
    match ago {
        0..=59 => "刚刚".to_string(),
        60..=3599 => format!("{} 分钟前", ago / 60),
        3600..=86_399 => format!("{} 小时前", ago / 3600),
        _ => format!("{} 天前", ago / 86_400),
    }
}

pub fn human_duration(secs: u64) -> String {
    let (d, h, m, s) = (
        secs / 86_400,
        (secs % 86_400) / 3600,
        (secs % 3600) / 60,
        secs % 60,
    );
    if d > 0 {
        format!("{d}d {h}h {m}m")
    } else if h > 0 {
        format!("{h}h {m}m {s}s")
    } else if m > 0 {
        format!("{m}m {s}s")
    } else {
        format!("{s}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mask_keeps_ends_only() {
        assert_eq!(mask("abcdefghijkl"), "abcd…ijkl");
        assert_eq!(mask("short"), "*****");
    }

    #[test]
    fn human_time_renders_unix_stamps_as_relative() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert_eq!(human_time(&format!("unix:{now}")), "刚刚");
        assert_eq!(human_time(&format!("unix:{}", now - 300)), "5 分钟前");
        assert_eq!(human_time(&format!("unix:{}", now - 7200)), "2 小时前");
        assert_eq!(human_time(&format!("unix:{}", now - 3 * 86_400)), "3 天前");
        // 未来时间（时钟回拨、档案来自别的机器）不能变成负数。
        assert_eq!(human_time(&format!("unix:{}", now + 600)), "刚刚");
        // 不认识的格式原样返回。
        assert_eq!(human_time("2026-09-04"), "2026-09-04");
        assert_eq!(human_time("unix:abc"), "unix:abc");
    }

    #[test]
    fn strip_ansi_removes_escape_sequences() {
        assert_eq!(strip_ansi("\x1b[32mok\x1b[0m"), "ok");
    }
}
