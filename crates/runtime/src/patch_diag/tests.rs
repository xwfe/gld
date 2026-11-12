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
    let diagnostic = Diagnostic::from_hunk_miss("a.txt".into(), "update", "file_on_disk", &text, m);
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
