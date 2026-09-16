use std::collections::VecDeque;
use std::fs::{self, File};
use std::io::{BufReader, Read};
use std::path::Path;
use std::time::SystemTime;

use regex::Regex;
use serde_json::{json, Value};
use toexec_text::{next_line, LineLimits};
use walkdir::WalkDir;

use crate::tools::workspace::{relative_display, tool_ok, Workspace, WorkspaceError};

const BINARY_PEEK_BYTES: usize = 8192;

/// `read_file` 每次从磁盘取多少字节。内存峰值约等于这个缓冲加 `max_bytes`，跟文件多大无关。
///
/// 以前是整份读进来、转 UTF-8 时再复制一份、再给全文建行索引：读一个 511 MB 的文件
/// 哪怕只要 200 字节，峰值也有 1114 MB（改成流式后 6 MB，耗时持平）。AI 连着读几个
/// 大日志，守护进程就把机器内存吃光了。
const READ_CHUNK_BYTES: usize = 64 * 1024;
/// 开头这么多字节里出现 0 字节，就当二进制文件拒绝。
const BINARY_SNIFF_BYTES: u64 = 4096;

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

pub fn read_file(ws: &Workspace, args: &Value) -> Result<Value, WorkspaceError> {
    let path = args
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| WorkspaceError::invalid_argument("path is required"))?;
    let resolved = ws.resolve_read_path(path)?;
    if resolved.path.is_dir() {
        return Err(WorkspaceError::Tool {
            code: "IS_DIRECTORY",
            message: "Path is a directory.".into(),
            category: "validation",
            retryable: false,
        });
    }
    let max_bytes = crate::tools::args::bounded(args, "read_file", "max_bytes") as usize;
    let start_line = args
        .get("start_line")
        .and_then(Value::as_u64)
        .unwrap_or(1)
        .max(1) as usize;
    let end_line = args
        .get("end_line")
        .and_then(Value::as_u64)
        .map(|v| v as usize);

    let file =
        File::open(&resolved.path).map_err(|_| WorkspaceError::not_found("File not found"))?;
    let TextSelection {
        content,
        truncated,
        total_lines,
        total_bytes,
    } = read_text_selection(file, READ_CHUNK_BYTES, start_line, end_line, max_bytes)?;
    let truncated_by = truncated.then_some("bytes");
    let end = end_line.unwrap_or(total_lines).min(total_lines);
    let actual_end = if truncated && !content.is_empty() {
        start_line + content.lines().count().saturating_sub(1)
    } else {
        end
    };
    let mut warnings = Vec::new();
    if truncated {
        warnings.push("content truncated".to_string());
    }
    // 截断落在一行中间时这一行只给了半截，下一页得从它重新读，否则后半截就被跳过了
    // （以前给的是下一行，AI 照着翻完一遍会以为读全了）。只有这一页连一整行都装不下时
    // 才往下跳，不然翻页会停在原地；跳过的部分用 warning 说清楚。
    let next_start_line = if !truncated {
        None
    } else if content.ends_with('\n') {
        Some(actual_end + 1)
    } else if content.contains('\n') {
        Some(actual_end)
    } else {
        warnings.push(format!(
            "line {start_line} is longer than max_bytes ({max_bytes}); the rest of it is skipped, raise max_bytes to read it whole"
        ));
        Some(start_line + 1)
    };
    Ok(tool_ok(json!({
        "path": resolved.display,
        "content": content,
        "encoding": "utf-8",
        "start_line": start_line,
        "end_line": actual_end,
        "next_start_line": next_start_line,
        "total_lines": total_lines,
        "total_bytes": total_bytes,
        "bytes_read": content.len(),
        "truncated": truncated,
        "truncated_by": truncated_by,
        "warnings": warnings
    })))
}

pub fn list_dir(ws: &Workspace, args: &Value) -> Result<Value, WorkspaceError> {
    let path = args.get("path").and_then(Value::as_str).unwrap_or(".");
    let resolved = ws.resolve_read_path(path)?;
    if !resolved.path.is_dir() {
        return Err(WorkspaceError::not_a_directory("Path is not a directory"));
    }
    let recursive = args
        .get("recursive")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let max_depth = crate::tools::args::bounded(args, "list_dir", "max_depth") as usize;
    let max_entries = crate::tools::args::bounded(args, "list_dir", "max_entries") as usize;
    let include_hidden = args
        .get("include_hidden")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let include_ignored = args
        .get("include_ignored")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let mut entries = Vec::new();
    let mut truncated = false;
    collect_dir_entries(
        ws,
        &resolved.path,
        &resolved.display,
        1,
        max_depth,
        recursive,
        include_hidden,
        include_ignored,
        max_entries,
        &mut entries,
        &mut truncated,
    );
    entries.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    Ok(tool_ok(json!({
        "path": resolved.display,
        "entries": entries,
        "truncated": truncated,
        "warnings": if truncated { vec!["entry limit reached"] } else { vec![] }
    })))
}

pub fn list_files(ws: &Workspace, args: &Value) -> Result<Value, WorkspaceError> {
    let path = args.get("path").and_then(Value::as_str).unwrap_or(".");
    let resolved = ws.resolve_read_path(path)?;
    if !resolved.path.is_dir() {
        return Err(WorkspaceError::not_a_directory("Path is not a directory"));
    }
    let patterns = list_files_patterns(args);
    let exclude_patterns = string_list_arg(args, "exclude_patterns");
    let max_results = crate::tools::args::bounded(args, "list_files", "max_results") as usize;
    let include_hidden = args
        .get("include_hidden")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let include_ignored = args
        .get("include_ignored")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let mut files = Vec::new();
    let mut truncated = false;
    for entry in WalkDir::new(&resolved.path)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
    {
        let p = entry.path();
        if p == resolved.path {
            continue;
        }
        if !ws.is_safe_read_path(p) {
            continue;
        }
        if ws.is_ignored_path(p, include_hidden, include_ignored) {
            if entry.file_type().is_dir() {
                continue;
            }
            continue;
        }
        if !entry.file_type().is_file() && !entry.file_type().is_symlink() {
            continue;
        }
        let rel = relative_display(ws.root(), p);
        if !patterns.iter().any(|pat| glob_match(pat, &rel)) {
            continue;
        }
        if exclude_patterns.iter().any(|pat| glob_match(pat, &rel)) {
            continue;
        }
        let meta = p.symlink_metadata().ok();
        files.push(json!({
            "path": rel,
            "type": if entry.file_type().is_symlink() { "symlink" } else { "file" },
            "size_bytes": meta.as_ref().map(|m| m.len()).unwrap_or(0),
            "modified": meta.and_then(|m| format_mtime(m.modified().ok()))
        }));
        if files.len() >= max_results {
            truncated = true;
            break;
        }
    }
    files.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
    Ok(tool_ok(json!({
        "path": resolved.display,
        "files": files,
        "truncated": truncated,
        "warnings": if truncated { vec!["result limit reached"] } else { vec![] }
    })))
}

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
    let max_results = crate::tools::args::bounded(args, "search_text", "max_results") as usize;
    let max_preview =
        crate::tools::args::bounded(args, "search_text", "max_preview_bytes") as usize;
    let max_file_bytes = crate::tools::args::bounded(args, "search_text", "max_file_bytes");

    let (include_globs, exclude_globs) = search_globs(args);
    let context_lines = crate::tools::args::bounded(args, "search_text", "context_lines") as usize;
    let matcher = build_matcher(query, use_regex, case_sensitive)?;

    let mut matches = Vec::new();
    let mut warnings = Vec::new();
    let mut skipped_large = 0usize;
    let mut skipped_binary = 0usize;
    let mut truncated = false;

    let mut consider_file = |p: &Path| {
        if matches.len() >= max_results {
            truncated = true;
            return false;
        }
        if !ws.is_safe_read_path(p) {
            return true;
        }
        if ws.is_ignored_path(p, false, false) {
            return true;
        }
        let rel = relative_display(ws.root(), p);
        if !passes_glob_filters(&rel, &include_globs, &exclude_globs) {
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
            FileEligibility::Unreadable => return true,
            FileEligibility::Text => {}
        }
        let stop = search_file_streaming(
            p,
            &rel,
            &matcher,
            context_lines,
            max_preview,
            max_results,
            &mut matches,
        );
        if stop {
            truncated = true;
            return false;
        }
        true
    };

    if resolved.path.is_file() {
        let _ = consider_file(&resolved.path);
    } else {
        for entry in WalkDir::new(&resolved.path)
            .follow_links(false)
            .into_iter()
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
        warnings.push("result limit reached; scan stopped early".to_string());
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

    Ok(tool_ok(json!({
        "query": query,
        "matches": matches,
        "total_matches": matches.len(),
        "truncated": truncated,
        "max_file_bytes": max_file_bytes,
        "skipped_large_files": skipped_large,
        "skipped_binary_files": skipped_binary,
        "warnings": warnings
    })))
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
fn search_file_streaming(
    path: &Path,
    rel: &str,
    matcher: &Matcher,
    context_lines: usize,
    max_preview: usize,
    max_results: usize,
    matches: &mut Vec<Value>,
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
) -> Result<Matcher, WorkspaceError> {
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
}

impl Matcher {
    fn is_match(&self, line: &str) -> bool {
        match self {
            Matcher::Regex(re) => re.is_match(line),
            Matcher::Literal(lit) => {
                if lit.chars().any(|c| c.is_uppercase()) {
                    line.contains(lit.as_str())
                } else {
                    line.to_lowercase().contains(lit)
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn collect_dir_entries(
    ws: &Workspace,
    dir: &Path,
    display: &str,
    depth: usize,
    max_depth: usize,
    recursive: bool,
    include_hidden: bool,
    include_ignored: bool,
    max_entries: usize,
    entries: &mut Vec<Value>,
    truncated: &mut bool,
) {
    let read_dir = match fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(_) => return,
    };
    for item in read_dir.flatten() {
        if *truncated {
            return;
        }
        let p = item.path();
        if ws.is_ignored_path(&p, include_hidden, include_ignored) {
            continue;
        }
        let name = item.file_name().to_string_lossy().into_owned();
        let rel = if display == "." {
            name.clone()
        } else {
            format!("{display}/{name}")
        };
        let ft = item.file_type().ok();
        let entry_type = if ft.as_ref().map(|t| t.is_symlink()).unwrap_or(false) {
            "symlink"
        } else if ft.as_ref().map(|t| t.is_dir()).unwrap_or(false) {
            "directory"
        } else if ft.as_ref().map(|t| t.is_file()).unwrap_or(false) {
            "file"
        } else {
            "other"
        };
        let meta = item.metadata().ok();
        entries.push(json!({
            "name": name,
            "path": rel.replace('\\', "/"),
            "type": entry_type,
            "size_bytes": meta.as_ref().map(|m| m.len()).unwrap_or(0),
            "modified": meta.and_then(|m| format_mtime(m.modified().ok())),
            "is_hidden": name.starts_with('.'),
            "is_ignored": false
        }));
        if entries.len() >= max_entries {
            *truncated = true;
            return;
        }
        if recursive && depth < max_depth && entry_type == "directory" && !p.is_symlink() {
            collect_dir_entries(
                ws,
                &p,
                &rel.replace('\\', "/"),
                depth + 1,
                max_depth,
                recursive,
                include_hidden,
                include_ignored,
                max_entries,
                entries,
                truncated,
            );
        }
    }
}

struct TextSelection {
    content: String,
    truncated: bool,
    total_lines: usize,
    total_bytes: u64,
}

/// 流式读一个 UTF-8 文本，只留下第 `start_line..=end_line` 行里的前 `max_bytes` 字节。
///
/// 结果跟"整份读进来按 `split_inclusive('\n')` 切行、再在字符边界上截断"逐字节一致，
/// 测试里拿旧算法对拍。文件仍然要读完：`total_lines` 和"整个文件是不是合法 UTF-8"
/// 都得看到最后一个字节才知道。
///
/// 报错优先级也跟以前一样：开头 4096 字节里有 0 字节就报 `BINARY_FILE`，哪怕更前面
/// 已经有非法 UTF-8——所以编码错误先记下，等嗅探窗口看完再报。
fn read_text_selection(
    mut reader: impl Read,
    chunk_bytes: usize,
    start_line: usize,
    end_line: Option<usize>,
    max_bytes: usize,
) -> Result<TextSelection, WorkspaceError> {
    let mut buf = vec![0u8; chunk_bytes];
    let mut kept: Vec<u8> = Vec::new();
    let mut truncated = false;
    // 已经遇到换行符的行数；最后一行没有换行符时靠 `line_open` 补上。
    let mut total_lines = 0usize;
    let mut line_open = false;
    let mut total_bytes = 0u64;
    // 上一块末尾没读完整的半个字符。
    let mut utf8_tail: Vec<u8> = Vec::new();
    let mut invalid_utf8 = false;

    loop {
        let read = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return Err(WorkspaceError::not_found("File not found")),
        };
        let chunk = &buf[..read];
        if total_bytes < BINARY_SNIFF_BYTES {
            let sniff = ((BINARY_SNIFF_BYTES - total_bytes) as usize).min(read);
            if chunk[..sniff].contains(&0) {
                return Err(WorkspaceError::Tool {
                    code: "BINARY_FILE",
                    message: "Binary file read blocked for text tool.".into(),
                    category: "validation",
                    retryable: false,
                });
            }
        }
        total_bytes += read as u64;
        if !invalid_utf8 && !utf8_chunk_ok(&mut utf8_tail, chunk) {
            invalid_utf8 = true;
        }
        if invalid_utf8 {
            if total_bytes >= BINARY_SNIFF_BYTES {
                break;
            }
            continue;
        }

        // 内容已经截满、或者已经过了 end_line，后面只剩数行。常见的"读大文件开头一段"
        // 几乎全部时间都在这里，逐行切分会慢一倍。
        if truncated || end_line.is_some_and(|end| total_lines >= end) {
            total_lines += chunk.iter().filter(|byte| **byte == b'\n').count();
            line_open = chunk.last() != Some(&b'\n');
            continue;
        }
        // 合法 UTF-8 的多字节字符里不会出现 b'\n'，所以按字节切行不会把字符切坏。
        for piece in chunk.split_inclusive(|byte| *byte == b'\n') {
            let line = total_lines + 1;
            if line >= start_line && end_line.is_none_or(|end| line <= end) {
                let room = max_bytes.saturating_sub(kept.len());
                if piece.len() > room {
                    kept.extend_from_slice(&piece[..room]);
                    truncated = true;
                } else {
                    kept.extend_from_slice(piece);
                }
            }
            line_open = !piece.ends_with(b"\n");
            if !line_open {
                total_lines += 1;
            }
        }
    }

    let unsupported_encoding = || WorkspaceError::Tool {
        code: "UNSUPPORTED_ENCODING",
        message: "File is not valid utf-8.".into(),
        category: "validation",
        retryable: false,
    };
    if invalid_utf8 || !utf8_tail.is_empty() {
        return Err(unsupported_encoding());
    }
    if line_open {
        total_lines += 1;
    }
    // kept 是合法文本的前缀，只可能在末尾截掉了半个字符，退回到字符边界。
    if let Err(error) = std::str::from_utf8(&kept) {
        kept.truncate(error.valid_up_to());
    }
    Ok(TextSelection {
        content: String::from_utf8(kept).map_err(|_| unsupported_encoding())?,
        truncated,
        total_lines,
        total_bytes,
    })
}

/// 增量校验 UTF-8。块尾没读完的半个字符（最多 3 字节）留在 `tail` 里。
///
/// 只拿下一块开头几个字节把它补完整，不把整块拼进来——整块复制会让读大文件慢一倍。
fn utf8_chunk_ok(tail: &mut Vec<u8>, mut chunk: &[u8]) -> bool {
    while let (false, Some((&byte, rest))) = (tail.is_empty(), chunk.split_first()) {
        tail.push(byte);
        chunk = rest;
        match std::str::from_utf8(tail) {
            Ok(_) => tail.clear(),
            Err(error) if error.error_len().is_none() => {}
            Err(_) => return false,
        }
    }
    match std::str::from_utf8(chunk) {
        Ok(_) => true,
        Err(error) if error.error_len().is_none() => {
            tail.extend_from_slice(&chunk[error.valid_up_to()..]);
            true
        }
        Err(_) => false,
    }
}

fn string_list_arg(args: &Value, key: &str) -> Vec<String> {
    args.get(key)
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn list_files_patterns(args: &Value) -> Vec<String> {
    let patterns = string_list_arg(args, "patterns");
    if !patterns.is_empty() {
        return patterns;
    }
    if let Some(glob) = args.get("glob").and_then(Value::as_str) {
        if !glob.is_empty() {
            return vec![glob.to_string()];
        }
    }
    vec!["**/*".to_string()]
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

fn glob_match(pattern: &str, path: &str) -> bool {
    let pat = pattern.replace('\\', "/");
    let p = path.replace('\\', "/");
    if pat == "**/*" || pat == "*" {
        return true;
    }
    if let Some(suffix) = pat.strip_prefix("**/") {
        return simple_glob(suffix, &p) || p.split('/').any(|part| simple_glob(suffix, part));
    }
    simple_glob(&pat, &p)
}

fn simple_glob(pattern: &str, text: &str) -> bool {
    glob::Pattern::new(pattern)
        .map(|p| p.matches(text))
        .unwrap_or(false)
}

fn format_mtime(st: Option<SystemTime>) -> Option<String> {
    st.map(|t| {
        let d = t.duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
        format!("{}.{:03}Z", d.as_secs(), d.subsec_millis())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The schema tells the client "default 100"; the code used to return up
    /// to 1000, ten times what the client planned its context for.
    #[test]
    fn search_text_default_max_results_is_what_the_schema_says() {
        let dir = tempfile::tempdir().expect("dir");
        std::fs::write(dir.path().join("many.txt"), "needle\n".repeat(150)).expect("write");
        let ws = Workspace::new(dir.path().to_path_buf()).expect("workspace");
        let result = search_text(&ws, &json!({ "query": "needle" })).expect("search");
        assert_eq!(
            result["matches"].as_array().expect("matches").len(),
            100,
            "{result}"
        );
        assert_eq!(result["truncated"], true);
    }

    /// Out-of-range values are brought into the declared range rather than
    /// obeyed: a max_results of a billion is a request for "all of it", not
    /// a licence to fill memory.
    #[test]
    fn search_text_max_results_above_the_schema_maximum_is_clamped() {
        let dir = tempfile::tempdir().expect("dir");
        std::fs::write(dir.path().join("many.txt"), "needle\n".repeat(10_050)).expect("write");
        let ws = Workspace::new(dir.path().to_path_buf()).expect("workspace");
        let result = search_text(
            &ws,
            &json!({ "query": "needle", "max_results": 1_000_000_000u64, "max_preview_bytes": 1 }),
        )
        .expect("search");
        assert_eq!(result["matches"].as_array().expect("matches").len(), 10_000);
    }

    /// 改成流式之前的算法，原样留着当标准答案。
    fn whole_file_reference(
        data: &[u8],
        start_line: usize,
        end_line: Option<usize>,
        max_bytes: usize,
    ) -> Result<(String, bool, usize), &'static str> {
        if data.iter().take(4096).any(|b| *b == 0) {
            return Err("BINARY_FILE");
        }
        let text = std::str::from_utf8(data).map_err(|_| "UNSUPPORTED_ENCODING")?;
        let lines: Vec<&str> = text.split_inclusive('\n').collect();
        let total_lines = lines.len();
        let end = end_line.unwrap_or(total_lines).min(total_lines);
        let selected: String = if end < start_line {
            String::new()
        } else {
            lines[(start_line - 1)..end].concat()
        };
        if selected.len() <= max_bytes {
            return Ok((selected, false, total_lines));
        }
        let mut cut = max_bytes;
        while cut > 0 && !selected.is_char_boundary(cut) {
            cut -= 1;
        }
        Ok((selected[..cut].to_string(), true, total_lines))
    }

    fn error_code(error: WorkspaceError) -> &'static str {
        match error {
            WorkspaceError::Tool { code, .. } | WorkspaceError::ToolDetails { code, .. } => code,
        }
    }

    /// 分块大小故意压到 1、2、3 字节，让多字节字符、换行符、嗅探窗口的边界都落在两块之间。
    fn assert_same_as_reference(data: &[u8]) {
        let ranges: &[(usize, Option<usize>)] = &[
            (1, None),
            (1, Some(1)),
            (2, Some(3)),
            (3, None),
            (5, Some(3)),
            (10_000, None),
        ];
        for chunk in [1, 2, 3, 5, 4096, READ_CHUNK_BYTES] {
            for &(start, end) in ranges {
                for max_bytes in [0, 1, 4, 7, 32, usize::MAX / 2] {
                    let expected = whole_file_reference(data, start, end, max_bytes);
                    let actual = read_text_selection(data, chunk, start, end, max_bytes)
                        .map(|s| {
                            assert_eq!(s.total_bytes, data.len() as u64);
                            (s.content, s.truncated, s.total_lines)
                        })
                        .map_err(error_code);
                    assert_eq!(
                        actual,
                        expected,
                        "chunk={chunk} start={start} end={end:?} max_bytes={max_bytes} data={:?}",
                        String::from_utf8_lossy(&data[..data.len().min(80)])
                    );
                }
            }
        }
    }

    #[test]
    fn streaming_read_matches_the_whole_file_algorithm() {
        for text in [
            "",
            "\n",
            "\n\n\n",
            "no newline at all",
            "a\nbb\nccc\n",
            "a\nbb\nccc",
            "crlf\r\nline\r\n",
            "中文\n混合 emoji 🌬 和 ascii\n最后一行没有换行",
            "🌬🌬🌬🌬🌬🌬🌬🌬🌬🌬🌬🌬🌬🌬🌬🌬🌬🌬🌬🌬",
        ] {
            assert_same_as_reference(text.as_bytes());
        }
    }

    #[test]
    fn streaming_read_reports_the_same_errors_as_before() {
        let mut nul_inside_sniff = vec![b'a'; 4095];
        nul_inside_sniff.push(0);
        let mut nul_after_sniff = vec![b'a'; 4096];
        nul_after_sniff.push(0);
        // 非法 UTF-8 在前、0 字节在后（仍在嗅探窗口内）：以前报的是 BINARY_FILE。
        let mut invalid_then_nul = b"ok\n\xff".to_vec();
        invalid_then_nul.extend_from_slice(&[b'a'; 100]);
        invalid_then_nul.push(0);

        for data in [
            nul_inside_sniff,
            nul_after_sniff,
            invalid_then_nul,
            b"lone continuation \x80 byte\n".to_vec(),
            // 文件在多字节字符中间结束。
            "尾巴被截断".as_bytes()[..7].to_vec(),
        ] {
            assert_same_as_reference(&data);
        }
    }

    #[test]
    fn a_giant_single_line_is_cut_without_holding_it_in_memory() {
        let data = "字".repeat(100_000);
        let selection = read_text_selection(data.as_bytes(), 1024, 1, None, 10).expect("read");
        assert_eq!(selection.content, "字字字");
        assert!(selection.truncated);
        assert_eq!(selection.total_lines, 1);
        assert_eq!(selection.total_bytes, data.len() as u64);
    }

    /// 搜索换了读行的实现（`BufRead::lines()` → 共用的 `toexec_text::next_line`）。
    /// `lines()` 会把 `\r\n` 两个字节都去掉，新实现必须一样，否则 CRLF 文件里
    /// 每条 preview 末尾都会多一个 `\r`，行尾锚定的正则也会失配。
    #[test]
    fn a_crlf_file_is_searched_without_the_carriage_return() {
        let dir = tempfile::tempdir().expect("dir");
        std::fs::write(dir.path().join("crlf.txt"), "alpha\r\nneedle\r\nomega\r\n").expect("write");
        let ws = Workspace::new(dir.path().to_path_buf()).expect("workspace");

        let result = search_text(&ws, &json!({ "query": "needle" })).expect("search");
        let matches = result["matches"].as_array().expect("matches");
        assert_eq!(matches.len(), 1, "{result}");
        assert_eq!(matches[0]["preview"], "needle");
        assert_eq!(matches[0]["line"], 2);

        // 行尾锚定：`\r` 要是还留着，这条就搜不到。
        let anchored = search_text(
            &ws,
            &json!({ "query": "needle$", "regex": true, "context_lines": 1 }),
        )
        .expect("search");
        assert_eq!(anchored["matches"].as_array().expect("matches").len(), 1);
        assert_eq!(anchored["matches"][0]["before"][0], "alpha");
        assert_eq!(anchored["matches"][0]["after"][0], "omega");
    }

    /// 新语义，跟以前不一样：一行超过 `SEARCH_LINE_KEEP` 的部分不再参与匹配。
    /// 换来的是一行占的内存有上限——以前有多长就吃多长，`context_lines` 还会
    /// 把它克隆几十份。
    #[test]
    fn a_line_past_the_keep_limit_is_only_searched_up_to_it() {
        let dir = tempfile::tempdir().expect("dir");
        let padding = "x".repeat(SEARCH_LINE_KEEP);
        std::fs::write(
            dir.path().join("huge.txt"),
            format!("needle-early{padding}needle-late\n"),
        )
        .expect("write");
        let ws = Workspace::new(dir.path().to_path_buf()).expect("workspace");
        let big = json!({ "max_file_bytes": 67_108_864u64 });

        let early = search_text(
            &ws,
            &json!({ "query": "needle-early", "max_file_bytes": big["max_file_bytes"] }),
        )
        .expect("search");
        assert_eq!(early["matches"].as_array().expect("matches").len(), 1);

        let late = search_text(
            &ws,
            &json!({ "query": "needle-late", "max_file_bytes": big["max_file_bytes"] }),
        )
        .expect("search");
        assert!(
            late["matches"].as_array().expect("matches").is_empty(),
            "保留上限之后的内容不该被搜到：{late}"
        );
    }

    /// 结果里所有字符串加起来多少字节。context 的克隆全都落在这里，所以这个
    /// 数就是放大程度，不用去量进程 RSS。
    fn held_string_bytes(value: &Value) -> usize {
        match value {
            Value::String(text) => text.len(),
            Value::Array(items) => items.iter().map(held_string_bytes).sum(),
            Value::Object(fields) => fields.values().map(held_string_bytes).sum(),
            _ => 0,
        }
    }

    /// context 行不截断的话，`context_lines: 20` 会把整行克隆几十份（为什么克隆
    /// 这么多份，见 `search_file_streaming` 里的注释）。
    ///
    /// 下面这份 41 行、每行 64 KiB 的文件，修之前结果里的字符串有 79,972,193 字节、
    /// 进程峰值 RSS 212 MB，修之后是 326,933 字节、RSS 11 MB；1 MiB 的阈值卡在中间。
    #[test]
    fn long_context_lines_are_clipped_instead_of_cloned_whole() {
        let dir = tempfile::tempdir().expect("dir");
        let wide = "x".repeat(64 * 1024);
        std::fs::write(
            dir.path().join("wide.txt"),
            format!("needle{wide}\n").repeat(41),
        )
        .expect("write");
        let ws = Workspace::new(dir.path().to_path_buf()).expect("workspace");

        let result = search_text(
            &ws,
            &json!({
                "query": "needle",
                "context_lines": 20,
                "max_file_bytes": 67_108_864u64,
            }),
        )
        .expect("search");

        let matches = result["matches"].as_array().expect("matches");
        assert_eq!(matches.len(), 41);
        let held = held_string_bytes(&result);
        assert!(
            held < 1024 * 1024,
            "context 行还在整行克隆：结果里有 {held} 字节"
        );

        // 行为变了，钉死：超长的 context 行按 max_preview_bytes（默认 256）截断，
        // 跟 preview 一样以 ... 收尾。
        let before = matches[1]["before"][0].as_str().expect("before");
        assert_eq!(before, matches[1]["preview"].as_str().expect("preview"));
        assert_eq!(before.len(), 256 + "...".len());
        assert_eq!(
            matches[1]["after"][0].as_str().expect("after").len(),
            256 + "...".len()
        );
    }
}
