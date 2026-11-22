use std::fs::File;
use std::io::Read;
use std::path::Path;

use gld_protocol::tool_args::bounded;
use serde_json::{json, Value};
use walkdir::WalkDir;

use crate::workspace::{relative_display, tool_err, tool_ok, Workspace, WorkspaceError};

use super::keep_walking_into;

/// `read_file` 每次从磁盘取多少字节。内存峰值约等于这个缓冲加 `max_bytes`，跟文件多大无关。
///
/// 以前是整份读进来、转 UTF-8 时再复制一份、再给全文建行索引：读一个 511 MB 的文件
/// 哪怕只要 200 字节，峰值也有 1114 MB（改成流式后 6 MB，耗时持平）。AI 连着读几个
/// 大日志，守护进程就把机器内存吃光了。
const READ_CHUNK_BYTES: usize = 64 * 1024;
/// 开头这么多字节里出现 0 字节，就当二进制文件拒绝。
const BINARY_SNIFF_BYTES: u64 = 4096;

/// 一次最多读几段。
const MAX_RANGES: usize = 20;
/// 一次调用里所有段加起来最多读多少字节。
const RANGES_TOTAL_BYTES: usize = 256 * 1024;

/// `read_file` 带 `ranges`：一次读几个文件或几段行。每段按一次单独的 `read_file` 读
/// （同样的 `max_bytes`），按顺序放进 `results`；一段读不了（不存在、越界）只在那一段里报，
/// 不影响别的段。读满 256 KiB 后剩下的段标 `skipped`。
///
/// 以前一次只能读一个文件，ChatGPT 就改用 `python3 -c` 一次读十几段：读操作被记成命令、
/// 算进任务的写操作，还绕开了读工具的路径限制（审查 §28）。
fn read_file_ranges(
    ws: &Workspace,
    args: &Value,
    max_bytes: usize,
) -> Result<Value, WorkspaceError> {
    for key in ["path", "start_line", "end_line", "start_byte"] {
        if args.get(key).is_some() {
            return Err(WorkspaceError::invalid_argument(format!(
                "{key} 和 ranges 不能一起给：读一段用 path，读几段把 path / start_line / end_line 写进 ranges 的每一项"
            )));
        }
    }
    let ranges = args
        .get("ranges")
        .and_then(Value::as_array)
        .ok_or_else(|| WorkspaceError::invalid_argument("ranges must be an array"))?;
    if ranges.is_empty() || ranges.len() > MAX_RANGES {
        return Err(WorkspaceError::invalid_argument(format!(
            "ranges 要有 1 到 {MAX_RANGES} 项，给了 {}",
            ranges.len()
        )));
    }
    let mut used = 0usize;
    let mut results = Vec::with_capacity(ranges.len());
    for (index, item) in ranges.iter().enumerate() {
        let Some(object) = item.as_object() else {
            return Err(WorkspaceError::invalid_argument(format!(
                "ranges[{index}] must be an object with path, and optionally start_line / end_line"
            )));
        };
        if let Some(key) = object
            .keys()
            .find(|key| !matches!(key.as_str(), "path" | "start_line" | "end_line"))
        {
            return Err(WorkspaceError::invalid_argument(format!(
                "ranges[{index}] 只收 path、start_line、end_line，多了 {key}"
            )));
        }
        let path = object.get("path").and_then(Value::as_str).unwrap_or("");
        if used >= RANGES_TOTAL_BYTES {
            results.push(json!({
                "ok": false,
                "path": path,
                "skipped": true,
                "reason": format!("这次调用已经读了 {RANGES_TOTAL_BYTES} 字节，剩下的分开再读")
            }));
            continue;
        }
        let one = json!({
            "path": path,
            "start_line": object.get("start_line"),
            "end_line": object.get("end_line"),
        });
        match read_one(ws, &one, max_bytes.min(RANGES_TOTAL_BYTES - used)) {
            Ok(value) => {
                used += value["bytes_read"].as_u64().unwrap_or(0) as usize;
                results.push(value);
            }
            Err(error) => {
                let mut value = tool_err(error);
                value["path"] = json!(path);
                results.push(value);
            }
        }
    }
    Ok(tool_ok(json!({
        "results": results,
        "bytes_read": used,
    })))
}

pub fn read_file(ws: &Workspace, args: &Value) -> Result<Value, WorkspaceError> {
    let max_bytes = bounded(args, "read_file", "max_bytes") as usize;
    if args.get("ranges").is_some() {
        return read_file_ranges(ws, args, max_bytes);
    }
    read_one(ws, args, max_bytes)
}

fn read_one(ws: &Workspace, args: &Value, max_bytes: usize) -> Result<Value, WorkspaceError> {
    let path = args
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| WorkspaceError::invalid_argument("path is required"))?;
    let resolved = ws
        .resolve_read_path(path)
        .map_err(|error| not_found_with_namesakes(ws, path, error))?;
    if resolved.path.is_dir() {
        return Err(WorkspaceError::Tool {
            code: "IS_DIRECTORY",
            message: "Path is a directory.".into(),
            category: "validation",
            retryable: false,
        });
    }
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
    // 版本标记：改这个文件时原样交给 apply_patch 的 expected_versions，
    // 文件在这之后被别人写过就会被拒，而不是把人家的改动覆盖掉（审查 C3）。
    let version = file
        .metadata()
        .ok()
        .map(|meta| crate::workspace::file_version(&meta));
    if let Some(start_byte) = args.get("start_byte").and_then(Value::as_u64) {
        return read_byte_window(file, &resolved.display, version, start_byte, max_bytes);
    }
    let TextSelection {
        content,
        truncated,
        total_lines,
        total_bytes,
        content_start_byte,
        cut_line_end,
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
    //
    // "这一页连一整行都装不下"就是**按行翻页会丢字节**的唯一情况。那时同时给出
    // `next_start_byte`：照它再调一次就能无损接上（方案 D：需要无损读取时支持
    // 有界字节续读，不把"翻页结束"当成"全文已读"）。
    let mut next_start_byte = None;
    let mut skipped_bytes = 0u64;
    let next_start_line = if !truncated {
        None
    } else if content.ends_with('\n') {
        Some(actual_end + 1)
    } else if content.contains('\n') {
        Some(actual_end)
    } else {
        let resume = content_start_byte + content.len() as u64;
        skipped_bytes = cut_line_end.unwrap_or(resume).saturating_sub(resume);
        next_start_byte = Some(resume);
        warnings.push(format!(
            "line {start_line} is longer than max_bytes ({max_bytes}): next_start_line skips the remaining {skipped_bytes} byte(s) of it. Read them with start_byte={resume}, or raise max_bytes."
        ));
        Some(start_line + 1)
    };
    Ok(tool_ok(json!({
        "path": resolved.display,
        "version": version,
        "content": content,
        "encoding": "utf-8",
        "read_mode": "lines",
        "start_line": start_line,
        "end_line": actual_end,
        "next_start_line": next_start_line,
        // 按行翻页会跳过的字节数，以及从哪个字节接着读才不丢。两个都只在
        // "一行比 max_bytes 还长"时才有值，其余情况按行翻页本来就是无损的。
        "skipped_bytes": skipped_bytes,
        "next_start_byte": next_start_byte,
        "total_lines": total_lines,
        "total_bytes": total_bytes,
        "bytes_read": content.len(),
        "truncated": truncated,
        "truncated_by": truncated_by,
        "warnings": warnings
    })))
}

/// 按**绝对字节偏移**读一段，不数行、不扫全文。
///
/// 只有一个用途：某一行比 `max_bytes` 还长时，把按行翻页跳过的那一段捞回来。
/// 所以它不回 `total_lines`——要知道第几行就得从文件头再数一遍，而这个入口存在
/// 的理由正是"不要为了读中间一段去扫整个文件"。
fn read_byte_window(
    mut file: File,
    display: &str,
    version: Option<String>,
    start_byte: u64,
    max_bytes: usize,
) -> Result<Value, WorkspaceError> {
    use std::io::{Seek, SeekFrom};

    let total_bytes = file.metadata().map(|meta| meta.len()).unwrap_or(0);
    if start_byte > total_bytes {
        return Err(WorkspaceError::invalid_argument(format!(
            "start_byte {start_byte} is past the end of the file ({total_bytes} bytes)"
        )));
    }
    file.seek(SeekFrom::Start(start_byte))
        .map_err(|_| WorkspaceError::not_found("File not found"))?;
    let mut raw = vec![0u8; max_bytes];
    let mut filled = 0usize;
    while filled < max_bytes {
        match file.read(&mut raw[filled..]) {
            Ok(0) => break,
            Ok(read) => filled += read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return Err(WorkspaceError::not_found("File not found")),
        }
    }
    raw.truncate(filled);
    if raw.contains(&0) {
        return Err(WorkspaceError::Tool {
            code: "BINARY_FILE",
            message: "Binary file read blocked for text tool.".into(),
            category: "validation",
            retryable: false,
        });
    }
    // 0b10xxxxxx 是 UTF-8 的续字节：落在这儿说明偏移切在一个字符中间。
    // 拿上一次返回的 next_start_byte 就不会这样，自己算的偏移会。
    if raw.first().is_some_and(|byte| byte & 0xC0 == 0x80) {
        return Err(WorkspaceError::invalid_argument(format!(
            "start_byte {start_byte} falls inside a utf-8 character; use the next_start_byte a previous read returned"
        )));
    }
    let valid_up_to = match std::str::from_utf8(&raw) {
        Ok(_) => raw.len(),
        Err(error) if error.error_len().is_none() => error.valid_up_to(),
        Err(_) => {
            return Err(WorkspaceError::Tool {
                code: "UNSUPPORTED_ENCODING",
                message: "File is not valid utf-8.".into(),
                category: "validation",
                retryable: false,
            })
        }
    };
    raw.truncate(valid_up_to);
    let end_byte = start_byte + raw.len() as u64;
    let truncated = end_byte < total_bytes;
    let content = String::from_utf8(raw).map_err(|_| WorkspaceError::Tool {
        code: "UNSUPPORTED_ENCODING",
        message: "File is not valid utf-8.".into(),
        category: "validation",
        retryable: false,
    })?;
    Ok(tool_ok(json!({
        "path": display,
        "version": version,
        "content": content,
        "encoding": "utf-8",
        "read_mode": "bytes",
        "start_byte": start_byte,
        "end_byte": end_byte,
        "next_start_byte": truncated.then_some(end_byte),
        // 这一路不数行，所以行号一概是 null——不是"第 0 行"，是"没算"。
        "start_line": Value::Null,
        "end_line": Value::Null,
        "next_start_line": Value::Null,
        "total_lines": Value::Null,
        "skipped_bytes": 0,
        "total_bytes": total_bytes,
        "bytes_read": content.len(),
        "truncated": truncated,
        "truncated_by": truncated.then_some("bytes"),
        "warnings": if truncated {
            vec!["content truncated".to_string()]
        } else {
            Vec::new()
        }
    })))
}

/// 遍历找同名文件最多看这么多项：大仓库里没找到就算了，别让一次报错拖成全盘扫描。
const NAMESAKE_WALK_LIMIT: usize = 20_000;
const NAMESAKES_SHOWN: usize = 5;

/// `read_file` 找不到时，顺手说出项目里同名的文件。
///
/// D10 浏览器那轮（2026-10-08）ChatGPT 照浏览器 console 里的 `/app.js` 去读 `app.js`，只拿到一句
/// `Path not found`，又列了一次目录才找到 `public/app.js`。按名字找、和 `list_files` 同一套忽略规则，
/// 只给路径，读不读还是它自己定。别的错误原样返回。
fn not_found_with_namesakes(
    ws: &Workspace,
    requested: &str,
    error: WorkspaceError,
) -> WorkspaceError {
    let WorkspaceError::Tool {
        code: "NOT_FOUND",
        ref message,
        category,
        retryable,
    } = error
    else {
        return error;
    };
    let Some(name) = Path::new(requested).file_name() else {
        return error;
    };
    let root = ws.root().to_path_buf();
    let mut found = Vec::new();
    for entry in WalkDir::new(&root)
        .follow_links(false)
        .sort_by_file_name()
        .into_iter()
        .filter_entry(|entry| keep_walking_into(ws, &root, entry, false))
        .filter_map(Result::ok)
        .take(NAMESAKE_WALK_LIMIT)
    {
        let p = entry.path();
        if entry.file_type().is_file()
            && p.file_name() == Some(name)
            && ws.is_safe_read_path(p)
            && !ws.is_ignored_path(p, false, false)
        {
            found.push(relative_display(&root, p));
            if found.len() == NAMESAKES_SHOWN {
                break;
            }
        }
    }
    if found.is_empty() {
        return error;
    }
    WorkspaceError::ToolDetails {
        code: "NOT_FOUND",
        message: format!(
            "{message}. Files with that name in the workspace: {}",
            found.join(", ")
        ),
        category,
        retryable,
        details: json!({ "same_name_elsewhere": found }),
    }
}

struct TextSelection {
    content: String,
    truncated: bool,
    total_lines: usize,
    total_bytes: u64,
    /// `content` 第一个字节在文件里的绝对偏移。按行翻页丢字节时，
    /// 加上 `content.len()` 就是"接着读"的起点。
    content_start_byte: u64,
    /// 截断落在一行中间时，那一行结束（含换行符）的绝对偏移。
    /// 用来算出按行翻页会跳过多少字节。
    cut_line_end: Option<u64>,
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
    // 绝对字节偏移：`content` 从哪儿开始、被截断的那一行到哪儿结束。
    // 只为了在"一行比 max_bytes 还长"时能说出接着从哪个字节读。
    let mut content_start_byte = 0u64;
    let mut content_started = false;
    let mut cut_line_end: Option<u64> = None;
    // 截断发生在块尾那半行上，那一行的结尾要到后面的块里去找。
    let mut seeking_line_end = false;

    loop {
        let chunk_start = total_bytes;
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
            if seeking_line_end {
                if let Some(at) = chunk.iter().position(|byte| *byte == b'\n') {
                    cut_line_end = Some(chunk_start + at as u64 + 1);
                    seeking_line_end = false;
                }
            }
            total_lines += chunk.iter().filter(|byte| **byte == b'\n').count();
            line_open = chunk.last() != Some(&b'\n');
            continue;
        }
        // 合法 UTF-8 的多字节字符里不会出现 b'\n'，所以按字节切行不会把字符切坏。
        let mut piece_start = chunk_start;
        for piece in chunk.split_inclusive(|byte| *byte == b'\n') {
            let line = total_lines + 1;
            if line >= start_line && end_line.is_none_or(|end| line <= end) {
                let room = max_bytes.saturating_sub(kept.len());
                if !content_started {
                    content_start_byte = piece_start;
                    content_started = true;
                }
                if piece.len() > room {
                    kept.extend_from_slice(&piece[..room]);
                    truncated = true;
                    // 截在这一行中间：它到哪儿结束决定了按行翻页会跳过多少。
                    // 这一块的最后一段没有换行符时，行还没完，往后面的块里找。
                    if room > 0 {
                        if piece.ends_with(b"\n") {
                            cut_line_end = Some(piece_start + piece.len() as u64);
                        } else {
                            seeking_line_end = true;
                        }
                    }
                } else {
                    kept.extend_from_slice(piece);
                }
            }
            piece_start += piece.len() as u64;
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
    // 文件末尾没有换行符时，被截的那一行就结束在文件末尾。
    if seeking_line_end {
        cut_line_end = Some(total_bytes);
    }
    Ok(TextSelection {
        content: String::from_utf8(kept).map_err(|_| unsupported_encoding())?,
        truncated,
        total_lines,
        total_bytes,
        content_start_byte,
        cut_line_end,
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

#[cfg(test)]
mod tests;
