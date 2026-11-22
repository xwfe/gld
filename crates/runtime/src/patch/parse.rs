//! Patch parsing and in-memory hunk application.

use serde_json::json;

use crate::patch_diag::{HunkMiss, NearMiss, MAX_CANDIDATES};
use crate::workspace::WorkspaceError;

use super::report::patch_format;
use super::validation::patch_failed;

#[derive(Debug)]
pub(super) struct FilePatch {
    pub(super) path: String,
    pub(super) hunks: Vec<Hunk>,
    pub(super) is_new_file: bool,
    pub(super) is_deleted: bool,
}

#[derive(Debug, Default)]
pub(super) struct Hunk {
    pub(super) lines: Vec<HunkLine>,
    /// 0-based index in the original file where the old lines start, from a
    /// unified `@@ -a,b` header. For `-a,0` (pure insertion) it is the line
    /// after line `a`.
    pub(super) old_start: Option<usize>,
    /// Codex `@@ <line>` header: the old lines are looked for after the
    /// first line equal to this one.
    pub(super) anchor: Option<String>,
    /// `@@ -a,b` 里的 `b`：这个 hunk **声称**动了多少行旧内容。
    /// 裸 `@@` 没有这个数，是 `None`。
    pub(super) declared_old_count: Option<usize>,
}

#[derive(Debug)]
pub(super) enum HunkLine {
    Context(String),
    Add(String),
    Remove(String),
}

pub(super) fn parse_unified_diff(patch: &str) -> Result<Vec<FilePatch>, WorkspaceError> {
    if patch_format(patch) == "codex" {
        return parse_codex_patch(patch);
    }

    let mut files = Vec::new();
    let mut current: Option<FilePatch> = None;
    let mut current_hunk: Option<Hunk> = None;

    for line in patch.lines() {
        if line.starts_with("--- ") {
            if let Some(h) = current_hunk.take() {
                if let Some(ref mut f) = current {
                    f.hunks.push(h);
                }
            }
            if let Some(f) = current.take() {
                files.push(f);
            }
            let path = parse_diff_path(line.strip_prefix("--- ").unwrap_or(""));
            current = Some(FilePatch {
                path,
                hunks: Vec::new(),
                is_new_file: line.contains("/dev/null"),
                is_deleted: false,
            });
        } else if line.starts_with("+++ ") {
            if let Some(ref mut f) = current {
                let new_path = parse_diff_path(line.strip_prefix("+++ ").unwrap_or(""));
                if !new_path.is_empty() && new_path != "/dev/null" {
                    f.path = new_path;
                }
                if line.contains("/dev/null") {
                    f.is_deleted = true;
                }
            }
        } else if line.starts_with("@@") {
            if let Some(h) = current_hunk.take() {
                if let Some(ref mut f) = current {
                    f.hunks.push(h);
                }
            }
            current_hunk = Some(Hunk {
                old_start: unified_old_start(line),
                declared_old_count: unified_old_count(line),
                ..Hunk::default()
            });
        } else if let Some(ref mut hunk) = current_hunk {
            if let Some(rest) = line.strip_prefix('+') {
                hunk.lines.push(HunkLine::Add(rest.to_string()));
            } else if let Some(rest) = line.strip_prefix('-') {
                hunk.lines.push(HunkLine::Remove(rest.to_string()));
            } else if let Some(rest) = line.strip_prefix(' ') {
                hunk.lines.push(HunkLine::Context(rest.to_string()));
            } else if line.is_empty() {
                hunk.lines.push(HunkLine::Context(String::new()));
            }
        }
    }
    if let Some(h) = current_hunk.take() {
        if let Some(ref mut f) = current {
            f.hunks.push(h);
        }
    }
    if let Some(f) = current.take() {
        files.push(f);
    }
    for file in &files {
        for hunk in &file.hunks {
            check_declared_old_count(&file.path, hunk)?;
        }
    }
    Ok(files)
}

/// `@@ -a,b` 说动了 b 行旧内容，正文里却不是 b 行：当场拒，别按正文办。
///
/// 为什么不"按正文办就完了"——正文确实是真正的指令，但数字对不上意味着**写补丁
/// 的人对这个文件的认识和文件本身不一样**。`@@ -10,5 @@` 只给了 3 行删除，作者
/// 以为自己换掉了 5 行；我们照 3 行办、还回一个 ok，他不会知道（审查 A07）。
/// `git apply` 在这种补丁上也是报 "corrupt patch"。
///
/// 数不准就别写数字：裸 `@@` 一直支持，报错里也是这么说的。
fn check_declared_old_count(path: &str, hunk: &Hunk) -> Result<(), WorkspaceError> {
    let Some(declared) = hunk.declared_old_count else {
        return Ok(());
    };
    let actual = hunk
        .lines
        .iter()
        .filter(|line| matches!(line, HunkLine::Remove(_) | HunkLine::Context(_)))
        .count();
    if declared == actual {
        return Ok(());
    }
    Err(WorkspaceError::ToolDetails {
        code: "PATCH_FAILED",
        message: format!(
            "{path}: the hunk header says {declared} old line(s) but the hunk body has {actual} (removed plus context). Fix the count, or write a bare @@ header and let the context decide."
        ),
        category: "validation",
        retryable: false,
        details: json!({
            "reason_code": "hunk_count_mismatch",
            "path": path,
            "declared_old_lines": declared,
            "actual_old_lines": actual,
            "files_changed": false
        }),
    })
}

fn parse_codex_patch(patch: &str) -> Result<Vec<FilePatch>, WorkspaceError> {
    let mut files = Vec::new();
    let mut current: Option<FilePatch> = None;
    let mut current_hunk: Option<Hunk> = None;
    let mut ended = false;

    for raw_line in patch.lines() {
        let line = raw_line.trim_end_matches('\r');
        if line == "*** Begin Patch" {
            continue;
        }
        if line == "*** End Patch" {
            finish_codex_file(&mut files, &mut current, &mut current_hunk);
            ended = true;
            continue;
        }

        let header = line
            .strip_prefix("*** Add File: ")
            .map(|path| (path, true, false))
            .or_else(|| {
                line.strip_prefix("*** Update File: ")
                    .map(|path| (path, false, false))
            })
            .or_else(|| {
                line.strip_prefix("*** Delete File: ")
                    .map(|path| (path, false, true))
            });
        if let Some((path, is_new_file, is_deleted)) = header {
            finish_codex_file(&mut files, &mut current, &mut current_hunk);
            current = Some(FilePatch {
                path: parse_diff_path(path),
                hunks: Vec::new(),
                is_new_file,
                is_deleted,
            });
            if is_new_file {
                current_hunk = Some(Hunk::default());
            }
            continue;
        }

        // 认识的指令都在上面。剩下的 `*** …` 一律报错，**不跳过**：真 Codex
        // 有 `*** Move to:`，这里没实现；静默跳过的结果是模型以为文件挪了，
        // 而磁盘上没挪（审查 P04）。
        if let Some(directive) = line.strip_prefix("*** ") {
            let name = directive.split(':').next().unwrap_or(directive).trim();
            return Err(patch_failed(
                "unsupported_directive",
                format!(
                    "*** {name}: is not something this patch format supports here; supported directives are *** Add File:, *** Update File:, *** Delete File:"
                ),
            ));
        }

        if let Some(header) = line.strip_prefix("@@") {
            if let Some(hunk) = current_hunk.take() {
                if let Some(ref mut file) = current {
                    file.hunks.push(hunk);
                }
            }
            let anchor = header.trim();
            current_hunk = Some(Hunk {
                anchor: (!anchor.is_empty()).then(|| anchor.to_string()),
                ..Hunk::default()
            });
            continue;
        }

        let Some(file) = current.as_ref() else {
            continue;
        };
        if file.is_deleted {
            continue;
        }
        let hunk = current_hunk.get_or_insert_with(Hunk::default);
        if let Some(rest) = line.strip_prefix('+') {
            hunk.lines.push(HunkLine::Add(rest.to_string()));
        } else if let Some(rest) = line.strip_prefix('-') {
            hunk.lines.push(HunkLine::Remove(rest.to_string()));
        } else if let Some(rest) = line.strip_prefix(' ') {
            hunk.lines.push(HunkLine::Context(rest.to_string()));
        } else if line.is_empty() {
            hunk.lines.push(HunkLine::Context(String::new()));
        }
    }

    finish_codex_file(&mut files, &mut current, &mut current_hunk);
    // 补丁被截断（网络断了、上下文满了）和补丁写完了长得一模一样，而前者
    // 应用一半就是把文件改坏。只有信封闭合了才算数。
    if !ended {
        return Err(patch_failed(
            "envelope_not_closed",
            "the patch stops before *** End Patch, so it may have been cut short; nothing was applied. Send the whole envelope",
        ));
    }
    Ok(files)
}

fn finish_codex_file(
    files: &mut Vec<FilePatch>,
    current: &mut Option<FilePatch>,
    current_hunk: &mut Option<Hunk>,
) {
    if let Some(hunk) = current_hunk.take() {
        if let Some(file) = current.as_mut() {
            file.hunks.push(hunk);
        }
    }
    if let Some(file) = current.take() {
        files.push(file);
    }
}

fn parse_diff_path(raw: &str) -> String {
    let trimmed = raw.trim();
    let path = trimmed
        .strip_prefix("a/")
        .or_else(|| trimmed.strip_prefix("b/"))
        .unwrap_or(trimmed);
    if path == "/dev/null" {
        return String::new();
    }
    path.replace('\\', "/")
}

/// 改后文件里的 0-based 行号，换算回**原文件**的 1-based 行号。
///
/// 前面几段 hunk 已经把下面的行推上推下了（`shift` 就是净增减），而模型
/// 拿到行号是要去 `read_file` 磁盘上那份文件的，不换算就对不上。
fn to_original_line(index: usize, shift: isize) -> usize {
    ((index as isize - shift).max(0) as usize) + 1
}

/// 失败时装箱：`HunkMiss` 带着候选位置和"差在哪"，不装箱的话每次成功返回也得
/// 按它的大小留位置（clippy 的 result_large_err）。
pub(super) fn apply_hunks(original: &str, hunks: &[Hunk]) -> Result<String, Box<HunkMiss>> {
    let line_ending = if original.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let had_trailing_newline = original.ends_with('\n');
    let mut lines: Vec<String> = if original.is_empty() {
        Vec::new()
    } else {
        original
            .split_terminator('\n')
            .map(|line| line.trim_end_matches('\r').to_string())
            .collect()
    };
    // Hunks apply in order: each is looked for after the previous one ended,
    // so a later hunk cannot land on a repeat of the text above it.
    let mut search_from = 0usize;
    // Lines added minus lines removed so far. Header line numbers refer to
    // the original file, and earlier hunks have moved everything below them.
    let mut shift: isize = 0;

    for (hunk_index, hunk) in hunks.iter().enumerate() {
        if let Some(anchor) = &hunk.anchor {
            let Some(at) = lines[search_from..]
                .iter()
                .position(|line| line.trim() == anchor)
            else {
                return Err(Box::new(HunkMiss {
                    code: "PATCH_FAILED",
                    reason_code: "anchor_not_found",
                    message: format!("Hunk header did not match any line: @@ {anchor}"),
                    hunk_index,
                    expected_range: None,
                    candidate_ranges: Vec::new(),
                    center_line: to_original_line(search_from, shift),
                    near_miss: None,
                }));
            };
            search_from += at + 1;
        }
        let hunk_old: Vec<String> = hunk
            .lines
            .iter()
            .filter_map(|l| match l {
                HunkLine::Context(s) | HunkLine::Remove(s) => Some(s.clone()),
                HunkLine::Add(_) => None,
            })
            .collect();

        let expected = hunk
            .old_start
            .map(|start| start.saturating_add_signed(shift));
        // 会删或改行的 hunk，既没有行号也没有锚点，而上下文在剩下的文件里
        // 匹配到不止一处：**不猜**。挑错一处就是改错地方，而且结果看起来
        // 是成功的。纯插入不算——插哪一处都不动原有内容，而且现有补丁大量
        // 这么写（审查 P04）。
        //
        // 错误码和 `PATCH_FAILED` 分开，因为下一步不一样：那个是"对不上，
        // 重读文件再来"，这个是"对得上太多处，把话说清楚再来"。
        if expected.is_none()
            && hunk.anchor.is_none()
            && hunk.lines.iter().any(|l| matches!(l, HunkLine::Remove(_)))
        {
            let candidates = match_positions(&lines, &hunk_old, search_from, MAX_CANDIDATES);
            if candidates.len() > 1 {
                let places = candidates
                    .iter()
                    .map(|line| to_original_line(*line, shift).to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(Box::new(HunkMiss {
                    code: "PATCH_AMBIGUOUS",
                    reason_code: "context_ambiguous",
                    message: format!(
                        "this hunk's context matches more than one place (lines {places}); add surrounding lines or a @@ -line,count @@ header so it is clear which one to change"
                    ),
                    hunk_index,
                    expected_range: None,
                    candidate_ranges: ranges_at(&candidates, hunk_old.len(), shift),
                    center_line: to_original_line(candidates[0], shift),
                    near_miss: None,
                }));
            }
        }
        let Some(pos) = find_hunk_position(&lines, &hunk_old, search_from, expected) else {
            return Err(Box::new(context_miss(
                &lines,
                &hunk_old,
                search_from,
                expected,
                shift,
                hunk_index,
            )));
        };

        let mut idx = pos;
        for hl in &hunk.lines {
            match hl {
                HunkLine::Context(_) => idx += 1,
                HunkLine::Remove(_) => {
                    if idx < lines.len() {
                        lines.remove(idx);
                    }
                }
                HunkLine::Add(s) => {
                    lines.insert(idx, s.clone());
                    idx += 1;
                }
            }
        }
        search_from = idx;
        let added = hunk
            .lines
            .iter()
            .filter(|l| matches!(l, HunkLine::Add(_)))
            .count();
        let removed = hunk
            .lines
            .iter()
            .filter(|l| matches!(l, HunkLine::Remove(_)))
            .count();
        shift += added as isize - removed as isize;
    }
    let mut output = lines.join(line_ending);
    if !output.is_empty() && (had_trailing_newline || original.is_empty()) {
        output.push_str(line_ending);
    }
    Ok(output)
}

/// 一组命中位置（改后 0-based）变成原文件的 1-based 闭区间。
fn ranges_at(positions: &[usize], length: usize, shift: isize) -> Vec<(usize, usize)> {
    positions
        .iter()
        .map(|position| {
            let start = to_original_line(*position, shift);
            (start, start + length.saturating_sub(1))
        })
        .collect()
}

/// 上下文没对上时，把"它可能在哪儿"找出来。
///
/// 顺着找三层，越往后越松：
/// 1. 整段上下文在文件里别的地方——多半是前面的 hunk 顺序写反了；
/// 2. 上下文的第一行还在，只是漂走了——文件被人改过，行号过期；
/// 3. 什么都找不到——这段补丁和这个文件对不上，得整段重写。
///
/// 三种情况下模型该做的事不一样，所以 reason_code 要分开：原来一律
/// `Hunk context did not match file content.`，连是哪个文件都没有（审查 E03）。
fn context_miss(
    lines: &[String],
    hunk_old: &[String],
    search_from: usize,
    expected: Option<usize>,
    shift: isize,
    hunk_index: usize,
) -> HunkMiss {
    let expected_range = expected.map(|start| {
        let start = to_original_line(start, shift);
        (start, start + hunk_old.len().saturating_sub(1))
    });

    let elsewhere = match_positions(lines, hunk_old, 0, MAX_CANDIDATES);
    if !elsewhere.is_empty() {
        let ranges = ranges_at(&elsewhere, hunk_old.len(), shift);
        let places = ranges
            .iter()
            .map(|(start, _)| start.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        return HunkMiss {
            code: "PATCH_FAILED",
            reason_code: "context_out_of_order",
            message: format!(
                "this hunk's context is in the file (around line {places}) but not after the previous hunk; hunks must be in file order"
            ),
            hunk_index,
            expected_range,
            center_line: ranges[0].0,
            candidate_ranges: ranges,
            near_miss: None,
        };
    }

    // 下面两种是整段在文件里精确找不到：算一下差在哪，日志靠它分清是比对太严
    // 还是补丁写错了。
    let near_miss = NearMiss::find(lines, hunk_old, |index| to_original_line(index, shift));

    // 退一步：整段对不上，那第一行呢。找得到就说明文件被改过、行号过期，
    // 模型重读那一段就能修好；找不到才是真的对不上。
    let first_line = hunk_old.iter().find(|line| !line.trim().is_empty());
    let drifted = first_line
        .map(|needle| {
            lines
                .iter()
                .enumerate()
                .filter(|(_, line)| line.trim() == needle.trim())
                .map(|(index, _)| index)
                .take(MAX_CANDIDATES)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if !drifted.is_empty() {
        let ranges = ranges_at(&drifted, hunk_old.len(), shift);
        let places = ranges
            .iter()
            .map(|(start, _)| start.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        return HunkMiss {
            code: "PATCH_FAILED",
            reason_code: "context_drifted",
            message: format!(
                "this hunk's context did not match; its first line is still there (line {places}), so the rest has changed since the patch was written. Re-read that range and rebuild this hunk"
            ),
            hunk_index,
            expected_range,
            center_line: ranges[0].0,
            candidate_ranges: ranges,
            near_miss,
        };
    }

    HunkMiss {
        code: "PATCH_FAILED",
        reason_code: "context_not_found",
        message: "this hunk's context is nowhere in the file; re-read the file and rebuild this hunk from what is actually there".into(),
        hunk_index,
        expected_range,
        candidate_ranges: Vec::new(),
        center_line: expected_range
            .map(|(start, _)| start)
            .unwrap_or_else(|| to_original_line(search_from, shift)),
        near_miss,
    }
}

/// 上下文在 `start` 之后命中的位置，最多找 `limit` 个（报歧义只需要证明
/// "不止一个"，不必把整份文件找完）。
fn match_positions(lines: &[String], pattern: &[String], start: usize, limit: usize) -> Vec<usize> {
    if pattern.is_empty() || start > lines.len() || pattern.len() > lines.len() - start {
        return Vec::new();
    }
    (start..=lines.len() - pattern.len())
        .filter(|&i| lines[i..i + pattern.len()] == *pattern)
        .take(limit)
        .collect()
}

/// Where the old lines of a hunk are, at or after `start`. With a line
/// number from the header, the match nearest to it wins (the file may have
/// drifted a little since the diff was made); without one, the first.
fn find_hunk_position(
    lines: &[String],
    pattern: &[String],
    start: usize,
    expected: Option<usize>,
) -> Option<usize> {
    if pattern.is_empty() {
        return Some(
            expected
                .unwrap_or(start)
                .clamp(start, lines.len().max(start)),
        );
    }
    if start > lines.len() || pattern.len() > lines.len().saturating_sub(start) {
        return None;
    }
    let mut matches =
        (start..=lines.len() - pattern.len()).filter(|&i| lines[i..i + pattern.len()] == *pattern);
    match expected {
        Some(expected) => matches.min_by_key(|&i| i.abs_diff(expected)),
        None => matches.next(),
    }
}

/// `@@ -a,b +c,d @@` 里的 `b`：这个 hunk 声称动了多少行旧内容。
///
/// 裸 `@@` 和写不出数字的头都是 `None`——那种写法本来就没声称什么，不该因此被拒。
fn unified_old_count(header: &str) -> Option<usize> {
    let old = header.strip_prefix("@@ -")?.split_whitespace().next()?;
    match old.split_once(',') {
        Some((_, count)) => count.parse::<usize>().ok(),
        None => Some(1),
    }
}

/// The 0-based start of a unified hunk's old lines, from `@@ -a,b +c,d @@`.
/// `None` for a bare `@@`, which some tools and models write.
fn unified_old_start(header: &str) -> Option<usize> {
    let old = header.strip_prefix("@@ -")?.split_whitespace().next()?;
    let (start, count) = match old.split_once(',') {
        Some((start, count)) => (start.parse::<usize>().ok()?, count.parse::<usize>().ok()?),
        None => (old.parse::<usize>().ok()?, 1),
    };
    Some(if count == 0 {
        start
    } else {
        start.saturating_sub(1)
    })
}
