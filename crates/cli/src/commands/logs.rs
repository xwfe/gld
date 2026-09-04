//! 日志查看：一次性显示尾部，或 `-f` 跟随。
//!
//! 跟随模式不经过守护进程：日志文件就在本机，直接按偏移量轮询读增量。

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::time::Duration;

use gld_core::app::LogChunk;
use gld_core::runtime::ServiceKind;
use gld_daemon::Request;

use super::Ctx;
use crate::cli::{LogService, LogsArgs};
use crate::error::CliResult;
use crate::output::Output;

/// 一次拉取的最大字节数；足够覆盖几十行请求日志。
const FETCH_BYTES: usize = 256 * 1024;
const POLL_INTERVAL: Duration = Duration::from_millis(500);

pub async fn workspace_logs(ctx: &mut Ctx, args: LogsArgs) -> CliResult {
    let kind = match args.service {
        LogService::Mcp => ServiceKind::Mcp,
        LogService::Actions => ServiceKind::Actions,
    };
    let chunks: Vec<LogChunk> = ctx
        .backend
        .call_typed(Request::Logs {
            target: ctx.target.clone(),
            kind,
            max_bytes: FETCH_BYTES,
        })
        .await?;
    if ctx.out.json_or(&chunks) {
        return Ok(());
    }
    if chunks.is_empty() {
        ctx.out
            .line("还没有日志。服务启动过一次后这里会有 stdout / stderr / 请求日志。");
        return Ok(());
    }
    let files: Vec<(String, PathBuf)> = chunks
        .iter()
        .map(|chunk| (chunk.name.clone(), chunk.path.clone()))
        .collect();
    tail_files(ctx.out, &files, args.lines, args.follow).await
}

/// 打印每个文件的最后 `lines` 行；`follow` 时继续轮询新内容。
pub async fn tail_files(
    out: Output,
    files: &[(String, PathBuf)],
    lines: usize,
    follow: bool,
) -> CliResult {
    let multiple = files.len() > 1;
    let mut offsets: HashMap<PathBuf, u64> = HashMap::new();
    for (name, path) in files {
        let (text, len) = read_tail(path, FETCH_BYTES)?;
        offsets.insert(path.clone(), len);
        if multiple {
            out.line(out.bold(&format!("==> {name} <==")));
        }
        let all: Vec<&str> = text.lines().collect();
        let start = all.len().saturating_sub(lines);
        for line in &all[start..] {
            out.line(line);
        }
        if multiple {
            out.line("");
        }
    }
    if !follow {
        return Ok(());
    }
    out.note("跟随中，Ctrl-C 退出…");
    loop {
        tokio::time::sleep(POLL_INTERVAL).await;
        for (name, path) in files {
            let previous = offsets.get(path).copied().unwrap_or(0);
            let Ok(mut file) = std::fs::File::open(path) else {
                continue;
            };
            let len = file.metadata().map(|m| m.len()).unwrap_or(0);
            if len < previous {
                // 文件被截断 / 轮转了，从头开始。
                offsets.insert(path.clone(), 0);
                continue;
            }
            if len == previous {
                continue;
            }
            file.seek(SeekFrom::Start(previous))?;
            let mut buffer = Vec::new();
            file.read_to_end(&mut buffer)?;
            offsets.insert(path.clone(), len);
            let text = String::from_utf8_lossy(&buffer);
            for line in text.lines() {
                if multiple {
                    out.line(format!("{} {line}", out.dim(&format!("[{name}]"))));
                } else {
                    out.line(line);
                }
            }
        }
    }
}

fn read_tail(path: &PathBuf, max_bytes: usize) -> CliResult<(String, u64)> {
    let Ok(mut file) = std::fs::File::open(path) else {
        return Ok((String::new(), 0));
    };
    let len = file.metadata()?.len();
    let start = len.saturating_sub(max_bytes as u64);
    file.seek(SeekFrom::Start(start))?;
    let mut buffer = Vec::new();
    file.read_to_end(&mut buffer)?;
    let mut text = String::from_utf8_lossy(&buffer).into_owned();
    if start > 0 {
        if let Some(newline) = text.find('\n') {
            text.drain(..=newline);
        }
    }
    Ok((text, len))
}
