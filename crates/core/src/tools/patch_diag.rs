//! 补丁对不上的时候，"对不上在哪儿"。
//!
//! 单独一个文件，是因为这里全是**纯计算**：给一份文件内容、一段没对上的
//! 上下文和一个行号，算出它可能在哪、附近现在长什么样、该重读哪一段。
//! 落盘、回滚、权限都不在这里，分开写才好单独测（审查 P01、A08）。
//!
//! 行号一律是**原文件的 1-based 闭区间**——模型拿到之后要做的事是
//! `read_file(path, start_line, end_line)`，那就只能是磁盘上那份文件的行号。
//! 一批补丁里前面的 hunk 已经把行挪过了，转换在 `patch.rs` 里做完再进来。

use std::borrow::Cow;

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

/// 算"差在哪"最多比多少次行（文件行数 × 这段补丁的行数）。只在失败时算，
/// 但一个几万行的文件配一段几百行的 hunk 也不该让报错慢下来；超了就不算，
/// `near_miss` 不出现。
const NEAR_MISS_BUDGET: usize = 2_000_000;

/// 一段 hunk 精确对不上之后，"差在哪"。
///
/// 回答的是 gld 自己该不该改的问题：差的只是行尾空白、缩进或花引号这类字符，
/// 说明比对可以更宽；差的是实打实的内容，那是补丁写错了。两类失败日志里分不开，
/// 就只能对着一串 `PATCH_FAILED` 猜。只有行号和计数，不带文件内容——它会
/// 进 operations.jsonl，而那里只记码不记内容（见 `dispatch.rs`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NearMiss {
    /// 放宽比较之后整段对得上。`kind` 是放宽到哪一级才对上的。
    Loose {
        kind: &'static str,
        start_line: usize,
        end_line: usize,
    },
    /// 放宽了也对不上：最像的那一处有几行一样（空行不算，它们到处都对得上）。
    Partial {
        start_line: Option<usize>,
        matched_lines: usize,
        compared_lines: usize,
        /// 那一处第一行不一样的，是这段补丁旧内容（上下文加删除行）的第几行，从 1 数。
        first_differing_line: Option<usize>,
    },
}

/// 把一行变成比较用的样子。
type LineKey = fn(&str) -> Cow<'_, str>;

/// 放宽比较的几级，从松到更松。每一级都包含前一级，报的是第一级对上的。
const LOOSE_LEVELS: [(&str, LineKey); 3] = [
    ("trailing_whitespace", trailing_whitespace_key),
    ("indentation", indentation_key),
    ("punctuation", punctuation_key),
];

fn trailing_whitespace_key(line: &str) -> Cow<'_, str> {
    Cow::Borrowed(line.trim_end())
}

fn indentation_key(line: &str) -> Cow<'_, str> {
    Cow::Borrowed(line.trim())
}

/// 模型常把 `'` `"` `-` 和空格写成排版用的变体（或者反过来，文件里本来就是
/// 变体），看着一样，逐字比就是不一样。
fn punctuation_key(line: &str) -> Cow<'_, str> {
    let trimmed = line.trim();
    if trimmed.is_ascii() {
        return Cow::Borrowed(trimmed);
    }
    Cow::Owned(
        trimmed
            .chars()
            .map(|c| match c {
                '\u{2010}'..='\u{2015}' | '\u{2212}' => '-',
                '\u{2018}'..='\u{201B}' => '\'',
                '\u{201C}'..='\u{201F}' => '"',
                '\u{00A0}' | '\u{2002}'..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}' => ' ',
                other => other,
            })
            .collect(),
    )
}

impl NearMiss {
    /// 精确比对失败之后算"差在哪"。`to_line` 把 `lines` 里的 0-based 下标换成
    /// 原文件的 1-based 行号（前面的 hunk 可能已经挪过行）。
    pub(crate) fn find(
        lines: &[String],
        pattern: &[String],
        to_line: impl Fn(usize) -> usize,
    ) -> Option<Self> {
        if pattern.is_empty() || lines.len().saturating_mul(pattern.len()) > NEAR_MISS_BUDGET {
            return None;
        }
        for (kind, key) in LOOSE_LEVELS {
            if let Some(index) = find_with(lines, pattern, key) {
                let start_line = to_line(index);
                return Some(Self::Loose {
                    kind,
                    start_line,
                    end_line: start_line + pattern.len() - 1,
                });
            }
        }
        Some(closest_partial(lines, pattern, to_line))
    }

    fn to_value(&self) -> Value {
        match self {
            Self::Loose {
                kind,
                start_line,
                end_line,
            } => json!({ "kind": kind, "start_line": start_line, "end_line": end_line }),
            Self::Partial {
                start_line,
                matched_lines,
                compared_lines,
                first_differing_line,
            } => json!({
                "kind": "partial",
                "start_line": start_line,
                "matched_lines": matched_lines,
                "compared_lines": compared_lines,
                "first_differing_line": first_differing_line
            }),
        }
    }
}

fn find_with(lines: &[String], pattern: &[String], key: LineKey) -> Option<usize> {
    if pattern.len() > lines.len() {
        return None;
    }
    let file = lines.iter().map(|line| key(line)).collect::<Vec<_>>();
    let wanted = pattern.iter().map(|line| key(line)).collect::<Vec<_>>();
    (0..=file.len() - wanted.len()).find(|&i| file[i..i + wanted.len()] == wanted[..])
}

/// 每个起点都试一遍，数补丁里的非空行有几行和文件对得上（按去掉首尾空白比），
/// 取最多的那一处。
fn closest_partial(
    lines: &[String],
    pattern: &[String],
    to_line: impl Fn(usize) -> usize,
) -> NearMiss {
    let compared = pattern
        .iter()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(offset, _)| offset)
        .collect::<Vec<_>>();
    let same = |start: usize, offset: usize| {
        lines
            .get(start + offset)
            .is_some_and(|line| line.trim() == pattern[offset].trim())
    };
    let mut best: Option<(usize, usize)> = None;
    for start in 0..lines.len() {
        let matched = compared
            .iter()
            .filter(|&&offset| same(start, offset))
            .count();
        if matched > best.map_or(0, |(_, most)| most) {
            best = Some((start, matched));
        }
    }
    NearMiss::Partial {
        start_line: best.map(|(start, _)| to_line(start)),
        matched_lines: best.map_or(0, |(_, matched)| matched),
        compared_lines: compared.len(),
        // 这里连空行也算：非空行全对上、差在一个空行上的时候，得指得出是哪一行。
        first_differing_line: best.and_then(|(start, _)| {
            (0..pattern.len())
                .find(|&offset| !same(start, offset))
                .map(|offset| offset + 1)
        }),
    }
}

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
    /// 精确比对对不上时，差在哪。只有"上下文找不到 / 漂走了"两种会算。
    pub(crate) near_miss: Option<NearMiss>,
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
    /// 版本冲突时：补丁说文件当时是哪个版本，现在又是哪个版本。
    ///
    /// 两个都是 `Option<String>`，因为"当时不存在"和"现在不存在"都要能表达：
    /// 新建一个文件时带的前置条件就是"这个路径应当没有东西"。
    pub(crate) expected_version: Option<Option<String>>,
    pub(crate) actual_version: Option<Option<String>>,
    pub(crate) near_miss: Option<NearMiss>,
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
            expected_version: None,
            actual_version: None,
            near_miss: None,
        }
    }

    /// 文件在读它之后被人写过（或者要新建的路径上已经有东西了）。
    ///
    /// 和"上下文对不上"分开报：那个是补丁本身写错了，这个是补丁没错、**世界
    /// 变了**——模型下一步该做的是重读文件，而不是去琢磨自己的 hunk。
    pub(crate) fn version_conflict(
        message: String,
        file: String,
        operation: &'static str,
        expected: Option<String>,
        actual: Option<String>,
    ) -> Self {
        Self {
            expected_version: Some(expected),
            actual_version: Some(actual),
            ..Self::file_level(
                "FILE_VERSION_CONFLICT",
                "file_changed_since_read",
                message,
                file,
                operation,
            )
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
            expected_version: None,
            actual_version: None,
            near_miss: miss.near_miss,
        }
    }

    pub(crate) fn to_value(&self) -> Value {
        let mut value = json!({
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
            "suggested_read_range": range_value(self.suggested_read_range),
            // 没有版本冲突时这两格不出现，别让每条诊断都挂两个 null。
            "expected_version": self.expected_version.clone().map(version_value),
            "actual_version": self.actual_version.clone().map(version_value)
        });
        if let Some(near_miss) = &self.near_miss {
            value["near_miss"] = near_miss.to_value();
        }
        value
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

/// `None` 是"这个路径上什么都没有"，不是"不知道"。
fn version_value(version: Option<String>) -> Value {
    version.map(Value::String).unwrap_or(Value::Null)
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
            near_miss: None,
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

    fn owned(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|line| line.to_string()).collect()
    }

    fn near_miss(file: &[&str], hunk: &[&str]) -> Option<NearMiss> {
        NearMiss::find(&owned(file), &owned(hunk), |index| index + 1)
    }

    /// 每一级各钉一个：报的必须是**第一级**对上的，不然"只差行尾空白"会被
    /// 报成"缩进不同"，看日志的人会去查错方向。
    #[test]
    fn a_loose_match_names_the_first_level_that_fits() {
        let file = ["fn a() {", "    let x = 1;", "}"];
        assert_eq!(
            near_miss(&file, &["fn a() {  ", "    let x = 1;"]),
            Some(NearMiss::Loose {
                kind: "trailing_whitespace",
                start_line: 1,
                end_line: 2
            })
        );
        assert_eq!(
            near_miss(&file, &["fn a() {", "\tlet x = 1;"]),
            Some(NearMiss::Loose {
                kind: "indentation",
                start_line: 1,
                end_line: 2
            })
        );
        assert_eq!(
            near_miss(
                &["say(\"hi\") - ok"],
                &["say(\u{201C}hi\u{201D}) \u{2014} ok"]
            ),
            Some(NearMiss::Loose {
                kind: "punctuation",
                start_line: 1,
                end_line: 1
            })
        );
    }

    /// 放宽了也对不上：指出最像的那一处、对上几行、第一行不一样的是补丁里的第几行。
    #[test]
    fn without_a_loose_match_the_closest_place_is_counted() {
        let file = ["a", "b", "", "x", "y", "z", "w"];
        assert_eq!(
            near_miss(&file, &["x", "y", "CHANGED", "w"]),
            Some(NearMiss::Partial {
                start_line: Some(4),
                matched_lines: 3,
                compared_lines: 4,
                first_differing_line: Some(3),
            })
        );
    }

    /// 一行都不像：没有位置可指，空行也不能凑数。
    #[test]
    fn an_unrelated_hunk_matches_nothing_even_through_blank_lines() {
        assert_eq!(
            near_miss(&["a", "", "b"], &["", "nope"]),
            Some(NearMiss::Partial {
                start_line: None,
                matched_lines: 0,
                compared_lines: 1,
                first_differing_line: None,
            })
        );
    }

    /// 文件和 hunk 大到比对次数超预算就不算——失败路径也不能慢。
    #[test]
    fn a_huge_comparison_is_skipped() {
        let file = vec!["line".to_string(); 100_000];
        let hunk = vec!["other".to_string(); 30];
        assert_eq!(NearMiss::find(&file, &hunk, |index| index + 1), None);
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
