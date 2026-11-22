use std::collections::VecDeque;
use std::fs::{self, File};
use std::io::{BufReader, Read};
use std::path::Path;

use gld_protocol::tool_args::bounded;
use regex::Regex;
use serde_json::{json, Value};
use toexec_text::{next_line, LineLimits};
use walkdir::WalkDir;

use crate::workspace::{relative_display, tool_ok, Workspace, WorkspaceError};

use super::{glob_match, keep_walking_into, string_list_arg, HiddenSkips};

const BINARY_PEEK_BYTES: usize = 8192;

/// `search_text` 一行最多保留多少字节。
///
/// 以前用 `BufRead::lines()`，一行有多长就往内存里放多长。`max_file_bytes`
/// 兜着（默认 2 MiB，最多 64 MiB），所以不至于把机器吃光，但一个 64 MiB 的
/// 单行文件（压缩过的 JS、一行导出的 JSON）确实会整行进内存，`context_lines`
/// 还会把它克隆好几十份。
///
/// 代价说清楚：**超过这个长度的行，只搜前 1 MiB**，后面匹配不到。正常源码
/// 没有这么长的行；真有的话它也是一行压缩产物，`max_preview_bytes` 最多给
/// 4096 字节的预览，搜到了也看不出什么。
const SEARCH_LINE_KEEP: usize = 1024 * 1024;

pub fn search_text(ws: &Workspace, args: &Value) -> Result<Value, WorkspaceError> {
    let query = args
        .get("query")
        .and_then(Value::as_str)
        .ok_or_else(|| WorkspaceError::invalid_argument("query is required"))?;
    let path = args.get("path").and_then(Value::as_str).unwrap_or(".");
    let resolved = ws.resolve_read_path(path)?;
    let use_regex = args.get("regex").and_then(Value::as_bool).unwrap_or(false);
    let case_sensitive = args
        .get("case_sensitive")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let max_results = bounded(args, "search_text", "max_results") as usize;
    let max_preview = bounded(args, "search_text", "max_preview_bytes") as usize;
    let max_file_bytes = bounded(args, "search_text", "max_file_bytes");

    let (include_globs, exclude_globs) = search_globs(args);
    let context_lines = bounded(args, "search_text", "context_lines") as usize;
    let multiline = args
        .get("multiline")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    // 隐藏文件默认还是不搜。`.github/workflows` 要改的时候，以前连"看一眼
    // 现在写的是什么"都做不到（审查 F03）。
    let include_hidden = args
        .get("include_hidden")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let output_mode = OutputMode::from_args(args)?;
    let file_type = match args.get("type").and_then(Value::as_str) {
        Some(name) if !name.trim().is_empty() => {
            Some(crate::file_types::lookup(name).ok_or_else(|| {
                WorkspaceError::invalid_argument(format!(
                    "Unknown type: {name}. Supported: {}",
                    crate::file_types::known_names().join(", ")
                ))
            })?)
        }
        _ => None,
    };
    let matcher = build_matcher(query, use_regex, case_sensitive, multiline)?;

    let mut matches = Vec::new();
    // `files_with_matches` 和 `count` 各自的结果。三种模式共用一次遍历，
    // 只是每个文件停在哪儿不一样。
    let mut files: Vec<String> = Vec::new();
    let mut counts: Vec<Value> = Vec::new();
    let mut warnings = Vec::new();
    let mut skipped_large = 0usize;
    let mut skipped_binary = 0usize;
    // 打不开的文件（权限不够、正被换掉、坏软链）。以前它和"搜过了没匹配"
    // 一样是静悄悄的（验收 A13 要求"权限跳过"能区分出来）。
    let mut skipped_unreadable = 0usize;
    let mut truncated = false;
    // 零结果有好几种，它们的下一步完全不同：没有文件通过 glob（八成是 glob
    // 写错了基准）、文件都被跳过了（太大 / 二进制）、真的搜了但没匹配。以前
    // 三种都只回一个空数组，模型无从分辨（审查 F02、复现 E07）。
    let mut scanned = 0usize;
    // 一行超过 SEARCH_LINE_KEEP（1 MiB）时，只有前 1 MiB 参与匹配，尾部既没搜
    // 也不会出现在预览里。不说的话它和"这一行里没有"长得一模一样（方案 D：
    // 超过保留长度的行不能默默表现为完整扫描）。
    let mut long_lines = 0usize;
    let mut rejected_by_glob = 0usize;
    let mut rejected_by_type = 0usize;

    let mut consider_file = |p: &Path| {
        // `max_results` 在三种模式下限的东西不同：content 限匹配行数，另外
        // 两种限文件数。名字没改，语义在工具说明里写清楚。
        let collected = match output_mode {
            OutputMode::Content => matches.len(),
            OutputMode::FilesWithMatches => files.len(),
            OutputMode::Count => counts.len(),
        };
        if collected >= max_results {
            truncated = true;
            return false;
        }
        if !ws.is_safe_read_path(p) {
            return true;
        }
        if ws.is_ignored_path(p, include_hidden, false) {
            return true;
        }
        let rel = relative_display(ws.root(), p);
        if !passes_glob_filters(&rel, &include_globs, &exclude_globs) {
            rejected_by_glob += 1;
            return true;
        }
        if file_type.is_some_and(|file_type| !file_type.matches(&rel)) {
            rejected_by_type += 1;
            return true;
        }
        let meta = match p.metadata() {
            Ok(m) if m.is_file() => m,
            _ => return true,
        };
        if meta.len() > max_file_bytes {
            skipped_large += 1;
            return true;
        }
        match file_text_eligibility(p) {
            FileEligibility::Binary => {
                skipped_binary += 1;
                return true;
            }
            FileEligibility::Unreadable => {
                skipped_unreadable += 1;
                return true;
            }
            FileEligibility::Text => {}
        }
        scanned += 1;
        match output_mode {
            OutputMode::Content => {
                let stop = if matcher.is_multiline() {
                    search_file_multiline(
                        p,
                        &rel,
                        &matcher,
                        context_lines,
                        max_preview,
                        max_results,
                        &mut matches,
                    )
                } else {
                    search_file_streaming(
                        p,
                        &rel,
                        &matcher,
                        context_lines,
                        max_preview,
                        max_results,
                        &mut matches,
                        &mut long_lines,
                    )
                };
                if stop {
                    truncated = true;
                    return false;
                }
            }
            // 命中一次就够，不用把这个文件读完。
            OutputMode::FilesWithMatches => {
                if count_file_matches(p, &matcher, Some(1), &mut long_lines) > 0 {
                    files.push(rel);
                    if files.len() >= max_results {
                        truncated = true;
                        return false;
                    }
                }
            }
            OutputMode::Count => {
                let count = count_file_matches(p, &matcher, None, &mut long_lines);
                if count > 0 {
                    counts.push(json!({ "path": rel, "count": count }));
                    if counts.len() >= max_results {
                        truncated = true;
                        return false;
                    }
                }
            }
        }
        true
    };

    let mut hidden_skips = HiddenSkips::default();
    if resolved.path.is_file() {
        let _ = consider_file(&resolved.path);
    } else {
        for entry in WalkDir::new(&resolved.path)
            .follow_links(false)
            // 目录项按文件名排序再走。结果被 max_results 截断时，同一个查询
            // 两次跑必须给出同一批——否则"缩小范围再搜一次"这句建议没法照做
            // （readdir 的顺序是文件系统说了算的，macOS 和 Linux 就不一样）。
            .sort_by_file_name()
            .into_iter()
            .filter_entry(|entry| {
                let keep = keep_walking_into(ws, &resolved.path, entry, include_hidden);
                if !keep && !include_hidden {
                    hidden_skips.note(ws, entry);
                }
                keep
            })
            .filter_map(Result::ok)
        {
            if !entry.file_type().is_file() {
                continue;
            }
            if !consider_file(entry.path()) {
                break;
            }
        }
    }

    if truncated {
        warnings.push(format!(
            "result limit reached ({max_results}); scan stopped early — narrow the query, give a deeper path, or raise max_results. Counts below describe what was scanned before stopping, not the whole project."
        ));
    }
    if skipped_large > 0 {
        warnings.push(format!(
            "skipped {skipped_large} file(s) larger than max_file_bytes ({max_file_bytes})"
        ));
    }
    if skipped_binary > 0 {
        warnings.push(format!(
            "skipped {skipped_binary} binary or non-utf8 file(s)"
        ));
    }
    if skipped_unreadable > 0 {
        warnings.push(format!(
            "skipped {skipped_unreadable} file(s) that could not be opened (permissions, or they changed while scanning)"
        ));
    }
    if long_lines > 0 {
        warnings.push(format!(
            "{long_lines} line(s) are longer than {SEARCH_LINE_KEEP} bytes; only their first {SEARCH_LINE_KEEP} bytes were searched, the rest was not read"
        ));
    }

    // 这次返回了几条。三种模式下它数的东西不一样（匹配行 / 文件 / 文件），
    // 跟着 `max_results` 走。
    let total = match output_mode {
        OutputMode::Content => matches.len(),
        OutputMode::FilesWithMatches => files.len(),
        OutputMode::Count => counts.len(),
    };
    // 一个都没搜到的时候，说清是哪一种"没有"。
    if total == 0 && !truncated {
        if scanned == 0 && rejected_by_glob > 0 {
            warnings.push(format!(
                "no file matched the globs, so nothing was searched: {rejected_by_glob} file(s) were rejected by include/exclude_globs. Globs match the path relative to the WORKSPACE ROOT, not to `path` — searching path=\"crates\" for glob \"exec.rs\" finds nothing; write \"**/exec.rs\"."
            ));
        } else if scanned == 0 && rejected_by_type > 0 {
            warnings.push(format!(
                "no file matched type={}, so nothing was searched ({rejected_by_type} file(s) rejected)",
                args.get("type").and_then(Value::as_str).unwrap_or("")
            ));
        } else if scanned == 0 {
            warnings
                .push("no readable text file under this path, so nothing was searched".to_string());
        } else {
            warnings.push(format!(
                "searched {scanned} file(s), none contained the query"
            ));
        }
        // 上面那句说的是"搜过的那些里没有"。搜没搜到整棵 `.github` 是另一回事，
        // 两句话要同时在。
        if let Some(note) = hidden_skips.warning("searched") {
            warnings.push(note);
        }
    }
    Ok(tool_ok(json!({
        "query": query,
        "output_mode": output_mode.as_str(),
        "multiline": multiline,
        "include_hidden": include_hidden,
        "type": args.get("type").and_then(Value::as_str),
        "matches": matches,
        "files": files,
        "counts": counts,
        // 这一页返回了几条，和"整个项目一共有几条"是两码事。截断之后后者
        // **不知道**，所以给 null——以前这里回的是这一页的条数，读起来就像
        // "全项目就这么多"（方案 D：全局计数必须和提前终止的预览分开）。
        "returned_matches": total,
        "total_matches": if truncated { Value::Null } else { Value::from(total) },
        "truncated": truncated,
        "max_file_bytes": max_file_bytes,
        "skipped_large_files": skipped_large,
        "skipped_binary_files": skipped_binary,
        "skipped_unreadable_files": skipped_unreadable,
        // 这次到底搜了什么：搜索根、glob 的基准、有多少文件真被读过、多少
        // 在过滤器那一步就没进来。零结果因此可以被解释，而不是只回一个空数组
        //（审查 F02 / F03，验收 A13）。
        "search_root": resolved.display,
        "glob_base": "workspace",
        "scanned_files": scanned,
        "lines_truncated_for_search": long_lines,
        "rejected_by_glob": rejected_by_glob,
        "rejected_by_type": rejected_by_type,
        "skipped_hidden_dirs": hidden_skips.count,
        "warnings": warnings
    })))
}

/// 搜索结果返回什么。名字和语义照 ripgrep / Claude Code 的 Grep：
/// 模型是照那套习惯写参数的。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutputMode {
    /// 匹配行 + 上下文（默认，也是一直以来的行为）。
    Content,
    /// 只要有匹配的文件路径。"这个符号在哪几个文件里"这种问题，回内容是浪费。
    FilesWithMatches,
    /// 每个文件匹配了几行。
    Count,
}

impl OutputMode {
    fn from_args(args: &Value) -> Result<Self, WorkspaceError> {
        match args.get("output_mode").and_then(Value::as_str) {
            None | Some("content") => Ok(Self::Content),
            Some("files_with_matches") => Ok(Self::FilesWithMatches),
            Some("count") => Ok(Self::Count),
            Some(other) => Err(WorkspaceError::invalid_argument(format!(
                "Unknown output_mode: {other}. Use content, files_with_matches or count"
            ))),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Content => "content",
            Self::FilesWithMatches => "files_with_matches",
            Self::Count => "count",
        }
    }
}

enum FileEligibility {
    Text,
    Binary,
    Unreadable,
}

fn file_text_eligibility(path: &Path) -> FileEligibility {
    let mut file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return FileEligibility::Unreadable,
    };
    let mut buf = [0u8; BINARY_PEEK_BYTES];
    let n = match file.read(&mut buf) {
        Ok(n) => n,
        Err(_) => return FileEligibility::Unreadable,
    };
    if buf[..n].contains(&0) {
        return FileEligibility::Binary;
    }
    FileEligibility::Text
}

/// Stream a file line-by-line. Returns true when `max_results` is reached.
#[allow(clippy::too_many_arguments)]
fn search_file_streaming(
    path: &Path,
    rel: &str,
    matcher: &Matcher,
    context_lines: usize,
    max_preview: usize,
    max_results: usize,
    matches: &mut Vec<Value>,
    long_lines: &mut usize,
) -> bool {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return false,
    };
    let mut reader = BufReader::new(file);
    let mut recent: VecDeque<String> = VecDeque::with_capacity(context_lines.max(1));
    let mut pending: Vec<PendingMatch> = Vec::new();
    let mut line_no = 0usize;
    // 共用 ccnm 的有界读行（toexec-text）。`scan_limit` 是 None：这里跟以前一样
    // 要读完整个文件，行长才有上限。
    let limits = LineLimits {
        keep: SEARCH_LINE_KEEP,
        scan_limit: None,
    };
    let mut raw = Vec::new();
    let mut scanned = 0u64;

    loop {
        raw.clear();
        match next_line(&mut reader, &mut raw, limits, &mut scanned) {
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(_) => {
                // 读不动了：跟以前一样丢掉没写完的 context，这个文件到此为止。
                flush_pending(&mut pending, matches, max_results);
                return matches.len() >= max_results;
            }
        }
        if raw.len() >= SEARCH_LINE_KEEP {
            *long_lines += 1;
        }
        let line = match std::str::from_utf8(&raw) {
            Ok(text) => text.to_string(),
            Err(_) => {
                // Invalid UTF-8 mid-file: drop unfinished context and stop this file.
                flush_pending(&mut pending, matches, max_results);
                return matches.len() >= max_results;
            }
        };
        line_no += 1;

        // 要留下来当 context 的那一份先截到 `max_preview_bytes`，跟 preview 同一把尺子。
        // 不截断的话一行会被克隆很多份：`recent` 存最近 context_lines 行，同时最多有
        // context_lines + 1 个待定匹配，每个又各存一份 before 和一份 after。实测 41 行
        // 每行 1 MiB（`SEARCH_LINE_KEEP` 的上限）配 context_lines=20，结果里的字符串
        // 就有 1.19 GiB，进程峰值 RSS 2.5 GB。
        // 匹配用的还是整行，截断只影响留存，搜得到什么没变。
        let context = (context_lines > 0).then(|| preview_line(&line, max_preview));

        // Feed "after" context for earlier hits.
        if let Some(context) = &context {
            for pend in &mut pending {
                if pend.after.len() < context_lines {
                    pend.after.push(context.clone());
                }
            }
            while pending
                .first()
                .is_some_and(|front| front.after.len() >= context_lines)
            {
                let done = pending.remove(0);
                matches.push(done.into_value());
                if matches.len() >= max_results {
                    return true;
                }
            }
        }

        if matcher.is_match(&line) {
            if let Some(context) = &context {
                pending.push(PendingMatch {
                    path: rel.to_string(),
                    line: line_no,
                    // preview 跟 context 是同一把尺子截出来的，复用这一份。
                    preview: context.clone(),
                    before: recent.iter().cloned().collect(),
                    after: Vec::new(),
                });
            } else {
                matches.push(json!({
                    "path": rel,
                    "line": line_no,
                    "column": 1,
                    "preview": preview_line(&line, max_preview)
                }));
                if matches.len() >= max_results {
                    return true;
                }
            }
        }

        if let Some(context) = context {
            recent.push_back(context);
            while recent.len() > context_lines {
                recent.pop_front();
            }
        }
    }

    // EOF: emit remaining pending with partial after context.
    for pend in pending {
        matches.push(pend.into_value());
        if matches.len() >= max_results {
            return true;
        }
    }
    false
}

struct PendingMatch {
    path: String,
    line: usize,
    preview: String,
    before: Vec<String>,
    after: Vec<String>,
}

impl PendingMatch {
    fn into_value(self) -> Value {
        json!({
            "path": self.path,
            "line": self.line,
            "column": 1,
            "preview": self.preview,
            "before": self.before,
            "after": self.after
        })
    }
}

/// 一行截到 `max_preview` 字节，切在字符边界上，截过的以 `...` 收尾。
/// preview 和 before/after 的 context 行都走这里。
fn preview_line(line: &str, max_preview: usize) -> String {
    if line.len() <= max_preview {
        return line.to_string();
    }
    let mut end = max_preview;
    while end > 0 && !line.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &line[..end])
}

fn flush_pending(pending: &mut Vec<PendingMatch>, matches: &mut Vec<Value>, max_results: usize) {
    for pend in pending.drain(..) {
        if matches.len() >= max_results {
            break;
        }
        matches.push(pend.into_value());
    }
}

fn build_matcher(
    query: &str,
    use_regex: bool,
    case_sensitive: bool,
    multiline: bool,
) -> Result<Matcher, WorkspaceError> {
    if multiline {
        // 跨行匹配统一走正则，字面量先转义。这样"查一段带换行的固定文本"
        // 也能用，而且不必为字面量再写一套跨行的大小写处理——那条路上
        // 小写化会改变字节偏移，报出来的行号就是错的。
        //
        // `s` 让 `.` 也匹配换行：不打开的话 `fn a\(.*\) \{` 这种写法跨不了行，
        // 而模型写 multiline 就是为了这个。
        let body = if use_regex {
            query.to_string()
        } else {
            regex::escape(query)
        };
        let flags = if case_sensitive { "s" } else { "si" };
        let pattern = Regex::new(&format!("(?{flags}:{body})"))
            .map_err(|e| WorkspaceError::invalid_argument(format!("Invalid regex: {e}")))?;
        return Ok(Matcher::Multiline(pattern));
    }
    if use_regex {
        let pattern = if case_sensitive {
            Regex::new(query)
        } else {
            Regex::new(&format!("(?i:{query})"))
        }
        .map_err(|e| WorkspaceError::invalid_argument(format!("Invalid regex: {e}")))?;
        Ok(Matcher::Regex(pattern))
    } else if case_sensitive {
        Ok(Matcher::Literal(query.to_string()))
    } else {
        Ok(Matcher::Literal(query.to_lowercase()))
    }
}

enum Matcher {
    Regex(Regex),
    Literal(String),
    /// 跨行：整个文件当一个字符串匹配，而不是一行一行。
    Multiline(Regex),
}

impl Matcher {
    fn is_match(&self, line: &str) -> bool {
        match self {
            Matcher::Regex(re) | Matcher::Multiline(re) => re.is_match(line),
            Matcher::Literal(lit) => {
                if lit.chars().any(|c| c.is_uppercase()) {
                    line.contains(lit.as_str())
                } else {
                    line.to_lowercase().contains(lit)
                }
            }
        }
    }

    fn is_multiline(&self) -> bool {
        matches!(self, Matcher::Multiline(_))
    }
}

/// 一个文件里有几处匹配。`limit` 给 `Some(1)` 时命中一次就返回——
/// `files_with_matches` 只要知道"有没有"，没必要把 10 MB 的文件读完。
///
/// 数的单位和 content 模式一致：**匹配的行数**（跨行模式下是匹配的段数）。
fn count_file_matches(
    path: &Path,
    matcher: &Matcher,
    limit: Option<usize>,
    long_lines: &mut usize,
) -> usize {
    if let Matcher::Multiline(re) = matcher {
        let Ok(text) = fs::read_to_string(path) else {
            return 0;
        };
        return match limit {
            Some(limit) => re.find_iter(&text).take(limit).count(),
            None => re.find_iter(&text).count(),
        };
    }

    let Ok(file) = File::open(path) else {
        return 0;
    };
    let mut reader = BufReader::new(file);
    let limits = LineLimits {
        keep: SEARCH_LINE_KEEP,
        scan_limit: None,
    };
    let mut raw = Vec::new();
    let mut scanned = 0u64;
    let mut count = 0usize;
    loop {
        raw.clear();
        match next_line(&mut reader, &mut raw, limits, &mut scanned) {
            Ok(Some(_)) => {}
            Ok(None) => break,
            // 读不动、或者中间冒出非 UTF-8：到此为止，已经数到的算数。
            Err(_) => break,
        }
        if raw.len() >= SEARCH_LINE_KEEP {
            *long_lines += 1;
        }
        let Ok(line) = std::str::from_utf8(&raw) else {
            break;
        };
        if matcher.is_match(line) {
            count += 1;
            if limit.is_some_and(|limit| count >= limit) {
                break;
            }
        }
    }
    count
}

/// 跨行搜索一个文件。
///
/// 和逐行那条路不同，这里必须把整个文件读进内存——跨行匹配本来就要看见换行
/// 两边。文件大小已经被 `max_file_bytes` 挡过一道（默认 2 MiB）。
///
/// 报的行号是**匹配起点所在的行**，预览是那一行；一处匹配跨了 5 行也只报一条，
/// 和 ripgrep 的 `-U` 一致。
fn search_file_multiline(
    path: &Path,
    rel: &str,
    matcher: &Matcher,
    context_lines: usize,
    max_preview: usize,
    max_results: usize,
    matches: &mut Vec<Value>,
) -> bool {
    let Matcher::Multiline(re) = matcher else {
        return false;
    };
    let Ok(text) = fs::read_to_string(path) else {
        return false;
    };
    let lines: Vec<&str> = text.split('\n').map(|l| l.trim_end_matches('\r')).collect();
    // 每一行起点的字节偏移，用来把匹配位置换算成行号。
    let mut line_starts = Vec::with_capacity(lines.len());
    let mut at = 0usize;
    for line in &lines {
        line_starts.push(at);
        at += line.len() + 1;
    }

    for found in re.find_iter(&text) {
        let index = line_starts
            .partition_point(|start| *start <= found.start())
            .saturating_sub(1);
        let line_no = index + 1;
        let before = if context_lines == 0 {
            Vec::new()
        } else {
            lines[index.saturating_sub(context_lines)..index]
                .iter()
                .map(|line| preview_line(line, max_preview))
                .collect()
        };
        let after_end = (index + 1 + context_lines).min(lines.len());
        let after = if context_lines == 0 || index + 1 >= lines.len() {
            Vec::new()
        } else {
            lines[index + 1..after_end]
                .iter()
                .map(|line| preview_line(line, max_preview))
                .collect()
        };
        matches.push(json!({
            "path": rel,
            "line": line_no,
            "column": 1,
            "preview": preview_line(lines[index], max_preview),
            "before": before,
            "after": after
        }));
        if matches.len() >= max_results {
            return true;
        }
    }
    false
}

fn search_globs(args: &Value) -> (Vec<String>, Vec<String>) {
    let mut include = string_list_arg(args, "include_globs");
    if let Some(glob) = args.get("glob").and_then(Value::as_str) {
        if !glob.is_empty() {
            include.push(glob.to_string());
        }
    }
    (include, string_list_arg(args, "exclude_globs"))
}

fn passes_glob_filters(rel: &str, include: &[String], exclude: &[String]) -> bool {
    if !include.is_empty() && !include.iter().any(|pat| glob_match(pat, rel)) {
        return false;
    }
    !exclude.iter().any(|pat| glob_match(pat, rel))
}

#[cfg(test)]
mod tests;
