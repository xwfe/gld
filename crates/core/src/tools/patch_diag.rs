//! 补丁对不上的时候，"对不上在哪儿"。
//!
//! 单独一个文件，是因为这里全是**纯计算**：给一份文件内容、一段没对上的
//! 上下文和一个行号，算出它可能在哪、附近现在长什么样、该重读哪一段。
//! 落盘、回滚、权限都不在这里，分开写才好单独测（审查 P01、A08）。
//!
//! 行号一律是**原文件的 1-based 闭区间**——模型拿到之后要做的事是
//! `read_file(path, start_line, end_line)`，那就只能是磁盘上那份文件的行号。
//! 一批补丁里前面的 hunk 已经把行挪过了，转换在 `patch.rs` 里做完再进来。

use serde_json::{json, Value};

/// 摘录最多给几行原文。够看清上下文就行，诊断不是用来传文件内容的。
const EXCERPT_LINES: usize = 9;

/// 摘录里单行最多给多少个字符。压缩过的 JS、长表格一行能有几万字符，
/// 整行塞进错误里会把响应撑爆。
const EXCERPT_LINE_CHARS: usize = 200;

/// 建议重读的范围，以中心行为准上下各留几行。比摘录宽：摘录是"我看到的"，
/// 建议范围是"你重建这段补丁需要看的"。
const SUGGESTED_CONTEXT_LINES: usize = 12;

/// 最多列几个候选位置。
pub(crate) const MAX_CANDIDATES: usize = 5;

/// 一次失败最多给几条诊断。超过就只给前面这些，并把 `diagnostics_truncated`
/// 标成 true——不能让一个 500 文件的补丁把错误响应变成一份报告。
pub(crate) const MAX_DIAGNOSTICS: usize = 20;

/// 一段 hunk 没对上的原始信息，由 `patch.rs` 在匹配失败的当场填。
///
/// 行号已经换算成原文件的 1-based 行号。
#[derive(Debug, Clone)]
pub(crate) struct HunkMiss {
    pub(crate) code: &'static str,
    pub(crate) reason_code: &'static str,
    pub(crate) message: String,
    /// 这个文件的第几段，从 0 开始。
    pub(crate) hunk_index: usize,
    /// 补丁头声称这段在哪儿（`@@ -a,b`）。没有行号的补丁是 None。
    pub(crate) expected_range: Option<(usize, usize)>,
    /// 这段上下文现在可能在哪儿。
    pub(crate) candidate_ranges: Vec<(usize, usize)>,
    /// 摘录以哪一行为中心。
    pub(crate) center_line: usize,
}

/// 一条完整诊断：哪个文件、哪一段、为什么、现在长什么样、该重读哪里。
#[derive(Debug, Clone)]
pub(crate) struct Diagnostic {
    pub(crate) code: &'static str,
    pub(crate) reason_code: &'static str,
    pub(crate) message: String,
    pub(crate) file: String,
    pub(crate) operation: &'static str,
    /// 这条诊断是拿什么当"原文"比出来的：`file_on_disk` 是磁盘上那份，
    /// `earlier_in_this_patch` 是同一批补丁里前一段改完的结果。后者的行号
    /// 和磁盘文件对不上，模型得先知道这件事，不然会以为 read_file 读错了。
    pub(crate) baseline: &'static str,
    pub(crate) hunk_index: Option<usize>,
    pub(crate) expected_range: Option<(usize, usize)>,
    pub(crate) candidate_ranges: Vec<(usize, usize)>,
    pub(crate) excerpt: Option<Excerpt>,
    pub(crate) suggested_read_range: Option<(usize, usize)>,
}

/// 文件现在那一段的真实内容。
#[derive(Debug, Clone)]
pub(crate) struct Excerpt {
    start_line: usize,
    end_line: usize,
    lines: Vec<String>,
    /// 有几行因为太长被截断了。
    truncated_lines: usize,
}

impl Diagnostic {
    /// 文件级的失败（Add 指向已有文件、文件不存在……）：没有具体某一段，
    /// 也就没有行号和摘录。
    pub(crate) fn file_level(
        code: &'static str,
        reason_code: &'static str,
        message: String,
        file: String,
        operation: &'static str,
    ) -> Self {
        Self {
            code,
            reason_code,
            message,
            file,
            operation,
            baseline: "file_on_disk",
            hunk_index: None,
            expected_range: None,
            candidate_ranges: Vec::new(),
            excerpt: None,
            suggested_read_range: None,
        }
    }

    /// 某一段没对上：把摘录和建议重读范围算出来。
    pub(crate) fn from_hunk_miss(
        file: String,
        operation: &'static str,
        baseline: &'static str,
        original: &str,
        miss: HunkMiss,
    ) -> Self {
        let lines = original_lines(original);
        let excerpt = excerpt_around(&lines, miss.center_line);
        let suggested = suggested_read_range(&lines, &miss);
        Self {
            code: miss.code,
            reason_code: miss.reason_code,
            message: miss.message,
            file,
            operation,
            baseline,
            hunk_index: Some(miss.hunk_index),
            expected_range: miss.expected_range,
            candidate_ranges: miss.candidate_ranges,
            excerpt,
            suggested_read_range: suggested,
        }
    }

    pub(crate) fn to_value(&self) -> Value {
        json!({
            "file": self.file,
            "operation": self.operation,
            "baseline": self.baseline,
            "reason_code": self.reason_code,
            "code": self.code,
            "message": self.message,
            "hunk_index": self.hunk_index,
            "expected_range": range_value(self.expected_range),
            "candidate_ranges": self
                .candidate_ranges
                .iter()
                .map(|range| range_value(Some(*range)))
                .collect::<Vec<_>>(),
            "actual_excerpt": self.excerpt.as_ref().map(Excerpt::to_value),
            "suggested_read_range": range_value(self.suggested_read_range)
        })
    }
}

impl Excerpt {
    fn to_value(&self) -> Value {
        json!({
            "start_line": self.start_line,
            "end_line": self.end_line,
            "lines": self.lines,
            "truncated_lines": self.truncated_lines
        })
    }
}

fn range_value(range: Option<(usize, usize)>) -> Value {
    match range {
        Some((start, end)) => json!({ "start_line": start, "end_line": end }),
        None => Value::Null,
    }
}

fn original_lines(original: &str) -> Vec<&str> {
    if original.is_empty() {
        Vec::new()
    } else {
        original
            .split_terminator('\n')
            .map(|line| line.trim_end_matches('\r'))
            .collect()
    }
}

/// 以 `center`（1-based）为中心取一段原文。文件是空的就没有摘录。
fn excerpt_around(lines: &[&str], center: usize) -> Option<Excerpt> {
    if lines.is_empty() {
        return None;
    }
    let half = EXCERPT_LINES / 2;
    let center = center.clamp(1, lines.len());
    let start = center.saturating_sub(half).max(1);
    let end = (start + EXCERPT_LINES - 1).min(lines.len());
    let mut truncated_lines = 0;
    let body = lines[start - 1..end]
        .iter()
        .map(|line| {
            if line.chars().count() > EXCERPT_LINE_CHARS {
                truncated_lines += 1;
                let head: String = line.chars().take(EXCERPT_LINE_CHARS).collect();
                format!("{head}…")
            } else {
                (*line).to_string()
            }
        })
        .collect::<Vec<_>>();
    Some(Excerpt {
        start_line: start,
        end_line: end,
        lines: body,
        truncated_lines,
    })
}

/// 重建这段补丁该重读哪一段。以候选位置优先——上下文真在那儿的时候，
/// 补丁头上那个行号已经不作数了。
fn suggested_read_range(lines: &[&str], miss: &HunkMiss) -> Option<(usize, usize)> {
    if lines.is_empty() {
        return None;
    }
    let (from, to) = match miss.candidate_ranges.first() {
        Some((start, end)) => (*start, *end),
        None => match miss.expected_range {
            Some((start, end)) => (start, end),
            None => (miss.center_line, miss.center_line),
        },
    };
    let start = from.saturating_sub(SUGGESTED_CONTEXT_LINES).max(1);
    let end = (to + SUGGESTED_CONTEXT_LINES).min(lines.len()).max(start);
    Some((start, end))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn miss(center: usize, candidates: Vec<(usize, usize)>) -> HunkMiss {
        HunkMiss {
            code: "PATCH_FAILED",
            reason_code: "context_not_found",
            message: "nope".into(),
            hunk_index: 0,
            expected_range: None,
            candidate_ranges: candidates,
            center_line: center,
        }
    }

    #[test]
    fn an_excerpt_is_centred_on_the_line_that_did_not_match() {
        let text = (1..=40)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let diagnostic = Diagnostic::from_hunk_miss(
            "a.txt".into(),
            "update",
            "file_on_disk",
            &text,
            miss(20, Vec::new()),
        );
        let excerpt = diagnostic.excerpt.expect("excerpt");
        assert_eq!(excerpt.start_line, 16);
        assert_eq!(excerpt.end_line, 24);
        assert_eq!(excerpt.lines.first().map(String::as_str), Some("line 16"));
        assert_eq!(excerpt.lines.len(), 9);
    }

    /// 文件头尾附近不能越界，也不能因为 clamp 少给行。
    #[test]
    fn an_excerpt_at_the_top_of_a_short_file_stays_inside_it() {
        let diagnostic = Diagnostic::from_hunk_miss(
            "a.txt".into(),
            "update",
            "file_on_disk",
            "one\ntwo\nthree\n",
            miss(1, Vec::new()),
        );
        let excerpt = diagnostic.excerpt.expect("excerpt");
        assert_eq!((excerpt.start_line, excerpt.end_line), (1, 3));
        assert_eq!(excerpt.lines, vec!["one", "two", "three"]);
    }

    /// 超长行截断，并且说清楚截了几行——否则模型会拿截断的内容当原文，
    /// 照着写出来的补丁一样对不上。
    #[test]
    fn a_very_long_line_is_cut_and_counted() {
        let long = "x".repeat(5_000);
        let diagnostic = Diagnostic::from_hunk_miss(
            "a.txt".into(),
            "update",
            "file_on_disk",
            &format!("head\n{long}\ntail\n"),
            miss(2, Vec::new()),
        );
        let excerpt = diagnostic.excerpt.expect("excerpt");
        assert_eq!(excerpt.truncated_lines, 1);
        assert_eq!(excerpt.lines[1].chars().count(), EXCERPT_LINE_CHARS + 1);
    }

    /// 有候选位置时，建议重读的是候选那一段，不是补丁头上那个过时的行号。
    #[test]
    fn the_suggested_range_follows_the_candidate_not_the_stale_header() {
        let text = (1..=200)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut m = miss(10, vec![(120, 123)]);
        m.expected_range = Some((10, 13));
        let diagnostic =
            Diagnostic::from_hunk_miss("a.txt".into(), "update", "file_on_disk", &text, m);
        assert_eq!(diagnostic.suggested_read_range, Some((108, 135)));
    }

    #[test]
    fn an_empty_file_has_no_excerpt_and_no_suggested_range() {
        let diagnostic = Diagnostic::from_hunk_miss(
            "a.txt".into(),
            "update",
            "file_on_disk",
            "",
            miss(1, Vec::new()),
        );
        assert!(diagnostic.excerpt.is_none());
        assert!(diagnostic.suggested_read_range.is_none());
    }
}
