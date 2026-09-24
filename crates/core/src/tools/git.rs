use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use regex::Regex;
use serde_json::{json, Value};

use crate::tools::workspace::{tool_ok, Workspace, WorkspaceError};

pub fn git_status(ws: &Workspace, args: &Value) -> Result<Value, WorkspaceError> {
    let path = args.get("path").and_then(Value::as_str).unwrap_or(".");
    let resolved = ws.resolve_existing(path)?;
    let max_entries = crate::tools::args::bounded(args, "git_status", "max_entries") as usize;
    let include_untracked = args
        .get("include_untracked")
        .and_then(Value::as_bool)
        .unwrap_or(true);

    let root_check = run_git(
        &resolved.path,
        &["rev-parse", "--show-toplevel"],
        Duration::from_secs(10),
    )?;
    if !root_check.success {
        return Ok(tool_ok(json!({
            "is_repo": false,
            "clean": true,
            "entries": [],
            "warnings": [root_check.stderr.trim()]
        })));
    }

    let mut status_args = vec!["status", "--porcelain=v1", "-b"];
    if !include_untracked {
        status_args.push("--untracked-files=no");
    }
    let scope = scope_to_workspace(ws.root(), &[])?;
    push_pathspec(&mut status_args, &scope);
    let completed = run_git(&resolved.path, &status_args, Duration::from_secs(10))?;
    if !completed.success && completed.exit_code != 0 {
        return Err(git_error(&completed.stderr));
    }

    let mut branch = String::new();
    let mut upstream = String::new();
    let mut ahead = 0i64;
    let mut behind = 0i64;
    let mut entries = Vec::new();
    let lines: Vec<_> = completed.stdout.lines().collect();
    let total_lines = lines.len();

    for line in lines {
        if let Some(rest) = line.strip_prefix("## ") {
            (branch, upstream, ahead, behind) = parse_branch_line(rest);
            continue;
        }
        if line.len() < 4 {
            continue;
        }
        let index_status = line.chars().next().unwrap_or(' ').to_string();
        let worktree_status = line.chars().nth(1).unwrap_or(' ').to_string();
        let mut path_text = line[3..].to_string();
        let original = if let Some((orig, new)) = path_text.split_once(" -> ") {
            let orig = orig.to_string();
            path_text = new.to_string();
            Some(orig)
        } else {
            None
        };
        let mut entry = json!({
            "path": path_text,
            "index_status": index_status,
            "worktree_status": worktree_status
        });
        if let Some(orig) = original {
            entry["original_path"] = json!(orig);
        }
        entries.push(entry);
        if entries.len() >= max_entries {
            break;
        }
    }

    let head = git_rev_parse(&resolved.path, "HEAD").unwrap_or_default();
    Ok(tool_ok(json!({
        "is_repo": true,
        "branch": branch,
        "head": head,
        "upstream": upstream,
        "ahead": ahead,
        "behind": behind,
        "clean": entries.is_empty(),
        "entries": entries,
        "truncated": entries.len() >= max_entries && total_lines > max_entries + 1,
        "warnings": []
    })))
}

pub fn git_diff(ws: &Workspace, args: &Value) -> Result<Value, WorkspaceError> {
    let staged = args.get("staged").and_then(Value::as_bool).unwrap_or(false);
    let unstaged = args
        .get("unstaged")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let context = crate::tools::args::bounded(args, "git_diff", "context_lines");
    let max_bytes = crate::tools::args::bounded(args, "git_diff", "max_bytes") as usize;

    let mut path_filters: Vec<String> = Vec::new();
    if let Some(p) = args.get("path").and_then(Value::as_str) {
        path_filters.push(p.to_string());
    }
    if let Some(paths) = args.get("paths").and_then(Value::as_array) {
        for p in paths {
            if let Some(s) = p.as_str() {
                path_filters.push(s.to_string());
            }
        }
    }
    for p in &path_filters {
        check_pathspec(ws, p)?;
    }

    if !is_git_repo(ws.root()) {
        return Ok(tool_ok(json!({
            "diff": "",
            "files": [],
            "truncated": false,
            "warnings": ["not a git repository"]
        })));
    }

    let path_filters = scope_to_workspace(ws.root(), &path_filters)?;
    let mut chunks = Vec::new();
    if unstaged {
        chunks.push(run_git_diff(ws.root(), context, &path_filters, false)?);
    }
    if staged {
        chunks.push(run_git_diff(ws.root(), context, &path_filters, true)?);
    }
    let mut combined = chunks.join("\n");
    if !combined.is_empty() && !combined.ends_with('\n') {
        combined.push('\n');
    }
    let truncated = combined.len() > max_bytes;
    let diff_text = if truncated {
        String::from_utf8_lossy(&combined.as_bytes()[..max_bytes]).into_owned()
    } else {
        combined
    };
    let mut files = Vec::new();
    if unstaged {
        files.extend(diff_files(
            ws.root(),
            &["diff"],
            &path_filters,
            Some(false),
        )?);
    }
    if staged {
        files.extend(diff_files(
            ws.root(),
            &["diff", "--cached"],
            &path_filters,
            Some(true),
        )?);
    }
    Ok(tool_ok(json!({
        "diff": diff_text,
        "files": files,
        "truncated": truncated,
        "warnings": if truncated { vec!["diff truncated"] } else { vec![] }
    })))
}

pub fn git_log(ws: &Workspace, args: &Value) -> Result<Value, WorkspaceError> {
    let path = args.get("path").and_then(Value::as_str).unwrap_or(".");
    let resolved = ws.resolve_existing(path)?;
    let ref_name = validate_git_ref(args.get("ref").and_then(Value::as_str).unwrap_or("HEAD"))?;
    let max_count = crate::tools::args::bounded(args, "git_log", "max_count") as usize;
    let skip = crate::tools::args::bounded(args, "git_log", "skip") as usize;

    if !is_git_repo(ws.root()) {
        return Ok(tool_ok(json!({
            "is_repo": false,
            "commits": [],
            "truncated": false,
            "warnings": []
        })));
    }

    let max_count_arg = format!("--max-count={}", max_count + 1);
    let skip_arg = format!("--skip={skip}");
    let pretty = "--pretty=format:%H%x1f%h%x1f%an%x1f%ae%x1f%ad%x1f%s%x1e";
    // 工作区根就是 `"."`，规范化在 `relative_display` 里做了，这里不再判一次。
    let path_filter = resolved.display.clone();
    let mut cmd_args = vec![
        "log",
        max_count_arg.as_str(),
        skip_arg.as_str(),
        "--date=iso-strict",
        pretty,
        ref_name,
    ];
    // 项目是仓库子目录时 "." 也要带上，不然列的是整个仓库的提交。项目就是仓库根时不带：
    // `-- .` 会触发历史简化，把合并提交藏掉。
    let scope = scope_to_workspace(ws.root(), &[])?;
    if path_filter != "." {
        cmd_args.push("--");
        cmd_args.push(path_filter.as_str());
    } else {
        push_pathspec(&mut cmd_args, &scope);
    }

    let completed = run_git(ws.root(), &cmd_args, Duration::from_secs(10))?;
    if !completed.success {
        return Err(git_error(&completed.stderr));
    }

    let mut commits = Vec::new();
    for record in completed.stdout.split('\u{1e}') {
        let fields: Vec<String> = record
            .trim()
            .split('\u{1f}')
            .map(str::trim)
            .map(str::to_string)
            .collect();
        if fields.len() < 6 || fields[0].is_empty() {
            continue;
        }
        commits.push(json!({
            "hash": fields[0],
            "short_hash": fields[1],
            "author_name": fields[2],
            "author_email": fields[3],
            "author_date": fields[4],
            "subject": fields[5],
        }));
    }
    let truncated = commits.len() > max_count;
    Ok(tool_ok(json!({
        "is_repo": true,
        "ref": ref_name,
        "path": path_filter,
        "commits": commits.into_iter().take(max_count).collect::<Vec<_>>(),
        "truncated": truncated,
        "warnings": if truncated { vec!["commit limit reached"] } else { Vec::<&str>::new() }
    })))
}

pub fn git_show(ws: &Workspace, args: &Value) -> Result<Value, WorkspaceError> {
    if !is_git_repo(ws.root()) {
        return Ok(tool_ok(json!({
            "is_repo": false,
            "content": "",
            "files": [],
            "truncated": false,
            "warnings": []
        })));
    }

    let rev = validate_git_ref(args.get("rev").and_then(Value::as_str).unwrap_or("HEAD"))?;
    let context = crate::tools::args::bounded(args, "git_show", "context_lines");
    let max_bytes = crate::tools::args::bounded(args, "git_show", "max_bytes") as usize;
    let include_diff = args
        .get("include_diff")
        .and_then(Value::as_bool)
        .unwrap_or(true);

    let mut path_filters: Vec<String> = Vec::new();
    if let Some(p) = args.get("path").and_then(Value::as_str) {
        path_filters.push(p.to_string());
    }
    if let Some(paths) = args.get("paths").and_then(Value::as_array) {
        for p in paths {
            if let Some(s) = p.as_str() {
                path_filters.push(s.to_string());
            }
        }
    }
    for p in &path_filters {
        check_pathspec(ws, p)?;
    }
    check_object_path(rev, &repo_prefix(ws.root()))?;
    let path_filters = scope_to_workspace(ws.root(), &path_filters)?;

    let unified = format!("--unified={context}");
    let mut cmd_args = vec!["show", "--no-ext-diff", "--format=fuller", unified.as_str()];
    if !include_diff {
        cmd_args.push("--no-patch");
    }
    cmd_args.push(rev);
    if !path_filters.is_empty() {
        cmd_args.push("--");
        for p in &path_filters {
            cmd_args.push(p.as_str());
        }
    }

    let completed = run_git(ws.root(), &cmd_args, Duration::from_secs(10))?;
    if !completed.success {
        return Err(git_error(&completed.stderr));
    }

    let truncated = completed.stdout.len() > max_bytes;
    let content = if truncated {
        String::from_utf8_lossy(&completed.stdout.as_bytes()[..max_bytes]).into_owned()
    } else {
        completed.stdout.clone()
    };
    let files = diff_files(ws.root(), &["show", "--format=", rev], &path_filters, None)?;
    Ok(tool_ok(json!({
        "is_repo": true,
        "rev": rev,
        "content": content,
        "files": files,
        "truncated": truncated,
        "output_bytes": content.len(),
        "warnings": if truncated { vec!["output truncated"] } else { Vec::<&str>::new() }
    })))
}

pub fn git_blame(ws: &Workspace, args: &Value) -> Result<Value, WorkspaceError> {
    let path = args
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| WorkspaceError::invalid_argument("path is required"))?;
    let resolved = ws.resolve_existing(path)?;
    if resolved.path.is_dir() {
        return Err(WorkspaceError::Tool {
            code: "IS_DIRECTORY",
            message: "Path is a directory.".into(),
            category: "validation",
            retryable: false,
        });
    }
    if !is_git_repo(ws.root()) {
        return Ok(tool_ok(json!({
            "is_repo": false,
            "path": resolved.display,
            "lines": [],
            "truncated": false,
            "warnings": []
        })));
    }

    let ref_arg = args.get("rev").and_then(Value::as_str);
    let git_ref = ref_arg.map(validate_git_ref).transpose()?;
    let start_line = args
        .get("start_line")
        .and_then(Value::as_u64)
        .unwrap_or(1)
        .max(1) as usize;
    let end_line_arg = args
        .get("end_line")
        .and_then(Value::as_u64)
        .map(|v| v as usize);
    let max_lines = crate::tools::args::bounded(args, "git_blame", "max_lines") as usize;

    let final_line = match end_line_arg {
        None => start_line + max_lines - 1,
        Some(end) if end < start_line => {
            return Err(WorkspaceError::invalid_argument(
                "end_line must be >= start_line.",
            ));
        }
        Some(end) => end,
    };
    let requested_lines = final_line - start_line + 1;
    let mut truncated = requested_lines > max_lines;
    let final_line = final_line.min(start_line + max_lines - 1);

    let line_range = format!("{start_line},{final_line}");
    let mut cmd_args = vec!["blame", "--line-porcelain", "-L", line_range.as_str()];
    if let Some(r) = git_ref {
        cmd_args.push(r);
    }
    cmd_args.push("--");
    cmd_args.push(resolved.display.as_str());

    let completed = run_git(ws.root(), &cmd_args, Duration::from_secs(10))?;
    if !completed.success {
        return Err(git_error(&completed.stderr));
    }

    let mut lines = parse_git_blame_porcelain(&completed.stdout);
    if lines.len() > max_lines {
        lines.truncate(max_lines);
        truncated = true;
    }

    Ok(tool_ok(json!({
        "is_repo": true,
        "path": resolved.display,
        "rev": ref_arg,
        "start_line": start_line,
        "end_line": final_line,
        "lines": lines,
        "truncated": truncated,
        "warnings": if truncated { vec!["line limit reached"] } else { Vec::<&str>::new() }
    })))
}

/// 这个项目在它所属仓库里的子目录前缀（`git rev-parse --show-prefix`，形如 `api/`）；
/// 项目就是仓库根时是空串。
fn repo_prefix(root: &std::path::Path) -> String {
    run_git(
        root,
        &["rev-parse", "--show-prefix"],
        Duration::from_secs(5),
    )
    .ok()
    .filter(|output| output.success)
    .map(|output| output.stdout.trim().to_string())
    .unwrap_or_default()
}

/// 给 git 的路径过滤。项目是仓库的子目录、调用方又没给过滤时补一个 `.`。
///
/// 不补的话 git 看的是整个仓库：同一个仓库里的兄弟目录（monorepo 里的另一个项目）
/// 的提交、未提交改动都读得到，越过了"读 Git 历史：项目目录内"这条边界——开给 api 的
/// 只读 grant 就能读 web（审查 D07）。项目就是仓库根时不补：整个仓库本来就是它的。
fn scope_to_workspace(
    root: &std::path::Path,
    filters: &[String],
) -> Result<Vec<String>, WorkspaceError> {
    if !filters.is_empty() || repo_prefix(root).is_empty() {
        return Ok(filters.to_vec());
    }
    Ok(vec![".".to_string()])
}

fn push_pathspec<'a>(args: &mut Vec<&'a str>, pathspec: &'a [String]) {
    if !pathspec.is_empty() {
        args.push("--");
        args.extend(pathspec.iter().map(String::as_str));
    }
}

/// 路径过滤只能是项目里的普通相对路径。`:` 开头是 git 的 pathspec 魔法，`:/x`、
/// `:(top)x` 都从仓库根算，能指到项目外面去。
fn check_pathspec(ws: &Workspace, pathspec: &str) -> Result<(), WorkspaceError> {
    ws.reject_unsafe_text(pathspec)?;
    if pathspec.starts_with(':') {
        return Err(WorkspaceError::path_outside_workspace());
    }
    Ok(())
}

/// `git_show` 的 `rev` 可以是 `提交:路径`（看某个版本的文件）。路径默认从仓库根算，
/// 项目是子目录时 `HEAD:web/.env` 就读到了兄弟项目，所以要落在项目前缀里。`:/文字`
/// （按提交说明搜）一样不收：它按整个仓库搜。项目就是仓库根时不查。
fn check_object_path(rev: &str, prefix: &str) -> Result<(), WorkspaceError> {
    let Some((_, path)) = rev.split_once(':') else {
        return Ok(());
    };
    if prefix.is_empty() {
        return Ok(());
    }
    let joined = if path.starts_with("./") || path.starts_with("../") || path == "." {
        format!("{prefix}{path}")
    } else if path.starts_with('/') {
        return Err(WorkspaceError::path_outside_workspace());
    } else {
        path.to_string()
    };
    // 按字面归一化 `.` / `..`，再看落没落在前缀里。
    let mut parts: Vec<&str> = Vec::new();
    for part in joined.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() {
                    return Err(WorkspaceError::path_outside_workspace());
                }
            }
            other => parts.push(other),
        }
    }
    let normalized = format!("{}/", parts.join("/"));
    if normalized.starts_with(prefix) {
        Ok(())
    } else {
        Err(WorkspaceError::path_outside_workspace())
    }
}

fn validate_git_ref(ref_name: &str) -> Result<&str, WorkspaceError> {
    if ref_name.is_empty()
        || ref_name.starts_with('-')
        || ref_name.contains('\0')
        || ref_name.contains('\n')
        || ref_name.contains('\r')
    {
        return Err(WorkspaceError::invalid_argument("Invalid git revision."));
    }
    Ok(ref_name)
}

fn parse_git_blame_porcelain(output: &str) -> Vec<Value> {
    let commit_re = Regex::new(r"^[0-9a-fA-F^]{40}").expect("valid regex");
    let mut rows = Vec::new();
    let mut current: serde_json::Map<String, Value> = serde_json::Map::new();

    for raw in output.lines() {
        let parts: Vec<&str> = raw.split_whitespace().collect();
        if parts.len() >= 3 && commit_re.is_match(parts[0]) {
            current = serde_json::Map::new();
            current.insert("commit".into(), json!(parts[0].trim_start_matches('^')));
            if parts[1].chars().all(|c| c.is_ascii_digit()) {
                current.insert("original_line".into(), json!(parts[1].parse::<i64>().ok()));
            }
            if parts[2].chars().all(|c| c.is_ascii_digit()) {
                current.insert("line".into(), json!(parts[2].parse::<i64>().ok()));
            }
            continue;
        }
        if let Some(author) = raw.strip_prefix("author ") {
            current.insert("author".into(), json!(author));
            continue;
        }
        if let Some(mail) = raw.strip_prefix("author-mail ") {
            current.insert(
                "author_mail".into(),
                json!(mail.trim_matches(|c| c == '<' || c == '>')),
            );
            continue;
        }
        if let Some(time) = raw.strip_prefix("author-time ") {
            let value = if time.chars().all(|c| c.is_ascii_digit()) {
                json!(time.parse::<i64>().ok())
            } else {
                json!(time)
            };
            current.insert("author_time".into(), value);
            continue;
        }
        if let Some(summary) = raw.strip_prefix("summary ") {
            current.insert("summary".into(), json!(summary));
            continue;
        }
        if let Some(content) = raw.strip_prefix('\t') {
            let mut row = current.clone();
            row.insert("content".into(), json!(content));
            rows.push(Value::Object(row));
        }
    }
    rows
}

struct GitOutput {
    success: bool,
    exit_code: i32,
    stdout: String,
    stderr: String,
}

fn run_git(
    cwd: &std::path::Path,
    args: &[&str],
    limit: Duration,
) -> Result<GitOutput, WorkspaceError> {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(cwd)
        .args(args)
        // 改用 spawn 后 stdin 默认会继承守护进程的；原来 output() 给的是空输入，保持不变。
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // 自成进程组，超时时把 git 拉起的 sh、hook、helper 一起杀掉。
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x00000200;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| git_error(&format!("git not available: {e}")))?;
    let stdout = read_pipe_in_background(child.stdout.take());
    let stderr = read_pipe_in_background(child.stderr.take());
    let deadline = Instant::now() + limit;

    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                kill_git(&mut child);
                return Err(git_timeout(args, limit));
            }
            Err(e) => {
                kill_git(&mut child);
                return Err(git_error(&format!("waiting for git failed: {e}")));
            }
        }
    };
    // git 已退出，但它留下的后台进程可能还占着管道；读端也只等到期限为止。
    // 这时 git 已被回收，进程组号可能被复用，所以不再发信号。
    let collect = |pipe: mpsc::Receiver<Vec<u8>>| {
        pipe.recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(|_| git_timeout(args, limit))
    };
    let stdout = collect(stdout)?;
    let stderr = collect(stderr)?;
    Ok(GitOutput {
        success: status.success(),
        exit_code: status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    })
}

fn read_pipe_in_background<R: Read + Send + 'static>(pipe: Option<R>) -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut buf);
        }
        let _ = tx.send(buf);
    });
    rx
}

fn kill_git(child: &mut Child) {
    #[cfg(unix)]
    if let Ok(pgid) = libc::pid_t::try_from(child.id()) {
        // SAFETY: 只发信号，负号表示整个进程组；组号是刚 spawn 且尚未回收的 git。
        unsafe {
            libc::kill(-pgid, libc::SIGKILL);
        }
    }
    // Windows 上 kill 只结束 git 本身；子孙若占着管道，读线程会晚些退出，但调用已经按时返回。
    let _ = child.kill();
    let _ = child.wait();
}

fn git_timeout(args: &[&str], limit: Duration) -> WorkspaceError {
    WorkspaceError::Tool {
        code: "TIMEOUT",
        message: format!(
            "git {} did not finish within {} ms",
            args.join(" "),
            limit.as_millis()
        ),
        category: "runtime",
        retryable: true,
    }
}

fn run_git_diff(
    root: &std::path::Path,
    context: u64,
    path_filters: &[String],
    cached: bool,
) -> Result<String, WorkspaceError> {
    let unified = format!("--unified={context}");
    // 和 git_show 一样不走外部 diff 程序：那是仓库配置里写的任意命令。
    let mut args = vec!["diff", "--no-ext-diff", unified.as_str()];
    if cached {
        args.push("--cached");
    }
    if !path_filters.is_empty() {
        args.push("--");
        for p in path_filters {
            args.push(p.as_str());
        }
    }
    let completed = run_git(root, &args, Duration::from_secs(10))?;
    if completed.exit_code != 0 && completed.exit_code != 1 {
        return Err(git_error(&completed.stderr));
    }
    Ok(completed.stdout)
}

fn is_git_repo(root: &std::path::Path) -> bool {
    run_git(root, &["rev-parse", "--git-dir"], Duration::from_secs(5))
        .map(|o| o.success)
        .unwrap_or(false)
}

fn git_rev_parse(cwd: &std::path::Path, rev: &str) -> Option<String> {
    run_git(cwd, &["rev-parse", rev], Duration::from_secs(5))
        .ok()
        .filter(|o| o.success)
        .map(|o| o.stdout.trim().to_string())
}

/// 解析 `git status -b --porcelain` 的 `## ` 行。
///
/// 四种形态都要认：
///
/// ```text
/// ## main...origin/main [ahead 1, behind 2]
/// ## main
/// ## No commits yet on main      ← 刚 git init，还没有提交
/// ## HEAD (no branch)            ← detached HEAD
/// ```
///
/// 不处理第三种的话，分支名会被解析成 "No"，AI 拿到的分支信息是错的。
fn parse_branch_line(line: &str) -> (String, String, i64, i64) {
    let line = line.strip_prefix("No commits yet on ").unwrap_or(line);
    let (branch_part, tracking) = line
        .split_once("...")
        .map(|(b, t)| (b.to_string(), t.to_string()))
        .unwrap_or((line.to_string(), String::new()));
    let branch = branch_part
        .split_once(' ')
        .map(|(b, _)| b.to_string())
        .unwrap_or(branch_part);
    let mut ahead = 0i64;
    let mut behind = 0i64;
    let mut upstream = tracking.clone();
    if let Some(idx) = tracking.find(' ') {
        upstream = tracking[..idx].to_string();
        // git 写成 `[ahead 1, behind 2]`；不剥掉方括号的话 "[ahead 1" 匹配不上前缀，
        // ahead / behind 会永远是 0。
        let meta = tracking[idx + 1..]
            .trim()
            .trim_start_matches('[')
            .trim_end_matches(']');
        for token in meta.split(',') {
            let token = token.trim();
            if let Some(n) = token.strip_prefix("ahead ") {
                ahead = n.trim().parse().unwrap_or(0);
            } else if let Some(n) = token.strip_prefix("behind ") {
                behind = n.trim().parse().unwrap_or(0);
            }
        }
    }
    (branch, upstream, ahead, behind)
}

/// 改了哪些文件：直接问 git，不从补丁文本里认。
///
/// 以前是从 `--- a/` / `+++ b/` 两行里抠路径：同一个文件两行各记一次（6 个文件报
/// 12 条），状态一律写 modified，二进制一律 false（审查 D14）。从文本里认本身也靠不住：
/// 路径带空格时那两行有歧义，文本被 `max_bytes` 截断后，后面的文件整个不在清单里。
/// 这里用 `-z`：NUL 分隔、路径原样不加引号；清单总是完整的，和文本截没截断无关。
///
/// `staged`：`git_diff` 两边都要时用来区分（同一个文件可以两边都有改动）；`git_show`
/// 不分，传 `None`。合并提交 `git show` 默认不列文件，这里也是空的。
fn diff_files(
    root: &std::path::Path,
    base: &[&str],
    path_filters: &[String],
    staged: Option<bool>,
) -> Result<Vec<Value>, WorkspaceError> {
    let run = |listing: &str| -> Result<String, WorkspaceError> {
        let mut args: Vec<&str> = base.to_vec();
        // -M：改名识别不受个人 git 配置（diff.renames=false）影响，结果在谁机器上都一样。
        args.extend([listing, "-z", "-M", "--no-ext-diff"]);
        if !path_filters.is_empty() {
            args.push("--");
            args.extend(path_filters.iter().map(String::as_str));
        }
        let completed = run_git(root, &args, Duration::from_secs(10))?;
        if completed.exit_code != 0 && completed.exit_code != 1 {
            return Err(git_error(&completed.stderr));
        }
        Ok(completed.stdout)
    };
    let binary = binary_paths(&run("--numstat")?);
    Ok(name_status(&run("--name-status")?)
        .into_iter()
        .map(|(status, path, old_path)| {
            let mut file = json!({
                "path": path,
                "status": status,
                "binary": binary.contains(&path),
            });
            if let Some(old_path) = old_path {
                file["old_path"] = json!(old_path);
            }
            if let Some(staged) = staged {
                file["staged"] = json!(staged);
            }
            file
        })
        .collect())
}

/// `--name-status -z`：`状态\0路径\0`，改名和复制是 `R100\0旧\0新\0`。
fn name_status(raw: &str) -> Vec<(&'static str, String, Option<String>)> {
    let mut tokens = raw.split('\0').filter(|token| !token.is_empty());
    let mut files = Vec::new();
    while let Some(code) = tokens.next() {
        let status = match code.chars().next() {
            Some('A') => "added",
            Some('D') => "deleted",
            Some('M') => "modified",
            Some('R') => "renamed",
            Some('C') => "copied",
            Some('T') => "type_changed",
            Some('U') => "unmerged",
            _ => "unknown",
        };
        let two_paths = matches!(status, "renamed" | "copied");
        let first = tokens.next().map(str::to_string);
        let (path, old_path) = if two_paths {
            (tokens.next().map(str::to_string), first)
        } else {
            (first, None)
        };
        if let Some(path) = path {
            files.push((status, path, old_path));
        }
    }
    files
}

/// `--numstat -z` 里增删行数是 `-\t-` 的那些：git 认定的二进制文件。
/// 格式是 `增\t删\t路径\0`，改名时路径那格为空，后面跟 `旧\0新\0`。
fn binary_paths(raw: &str) -> std::collections::HashSet<String> {
    let mut tokens = raw.split('\0');
    let mut binary = std::collections::HashSet::new();
    while let Some(token) = tokens.next() {
        let mut fields = token.splitn(3, '\t');
        let (Some(added), Some(deleted), Some(path)) =
            (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let path = if path.is_empty() {
            tokens.next();
            tokens.next().unwrap_or_default().to_string()
        } else {
            path.to_string()
        };
        if added == "-" && deleted == "-" {
            binary.insert(path);
        }
    }
    binary
}

fn git_error(message: &str) -> WorkspaceError {
    WorkspaceError::Tool {
        code: "GIT_ERROR",
        message: message.to_string(),
        category: "runtime",
        retryable: false,
    }
}

#[cfg(test)]
mod tests {
    use super::parse_branch_line;

    // `!` 别名让 git 经 sh 再拉起 sleep，sleep 继承 stdout/stderr 管道。
    // 只杀 git 本身时 sleep 还活着占着管道——所以既要按时返回，也要确认 sleep 死了。
    #[cfg(unix)]
    #[test]
    fn run_git_kills_a_hung_git_and_what_it_spawned_at_the_limit() {
        use super::run_git;
        use std::time::{Duration, Instant};

        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("sleep.pid");
        let alias = format!(
            "alias.hang=!echo $$ > '{}'; exec sleep 20",
            pid_file.display()
        );

        // 预算不能太小。这里要赛跑的是 fork/exec 三层（git → sh → echo）能不能在
        // 超时之前把 pid 落盘：超时一到整个进程组就被杀，没写成的话文件永远不会
        // 出现，下面读 pid 那步就报"别名没跑起来"——看着像别名写错了，其实是机器
        // 忙。原来给 300 毫秒，在 CI 的 macOS runner 上偶发失败。2 秒对一个 echo
        // 绰绰有余，而别名里是 sleep 20，所以照样一定超时。
        let result = {
            let started = Instant::now();
            let result = run_git(dir.path(), &["-c", &alias, "hang"], Duration::from_secs(2));
            let elapsed = started.elapsed();
            assert!(elapsed < Duration::from_secs(5), "等了 {elapsed:?} 才返回");
            result
        };
        let error = result.err().expect("超时应当报错");
        assert!(
            error.to_error_value()["code"] == "TIMEOUT",
            "{:?}",
            error.to_error_value()
        );

        let pid: libc::pid_t = std::fs::read_to_string(&pid_file)
            .expect("别名没跑起来")
            .trim()
            .parse()
            .unwrap();
        // 被杀的 sleep 成了孤儿，要等 init/launchd 收尸，给 2 秒。
        let gone_by = Instant::now() + Duration::from_secs(2);
        while unsafe { libc::kill(pid, 0) } == 0 && Instant::now() < gone_by {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_ne!(
            unsafe { libc::kill(pid, 0) },
            0,
            "git 拉起的 sleep {pid} 还活着"
        );
    }

    #[test]
    fn parses_every_form_of_the_status_branch_line() {
        assert_eq!(
            parse_branch_line("main...origin/main [ahead 1, behind 2]"),
            ("main".into(), "origin/main".into(), 1, 2)
        );
        assert_eq!(
            parse_branch_line("main"),
            ("main".into(), String::new(), 0, 0)
        );
        // 刚 git init：分支名以前会被解析成 "No"。
        assert_eq!(
            parse_branch_line("No commits yet on master"),
            ("master".into(), String::new(), 0, 0)
        );
        assert_eq!(
            parse_branch_line("HEAD (no branch)"),
            ("HEAD".into(), String::new(), 0, 0)
        );
    }

    fn git(dir: &std::path::Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.email=t@example.com", "-c", "user.name=t"])
            .args(args)
            .status()
            .expect("git");
        assert!(status.success(), "git {args:?}");
    }

    /// 审查 D14：6 个文件报 12 条、状态一律 modified、二进制一律 false。
    /// 这里每种改动各来一个，外加带空格的路径和被截断的文本。
    #[test]
    fn the_file_list_names_each_file_once_with_its_real_status() {
        use super::{git_diff, git_show};
        use crate::tools::workspace::Workspace;
        use serde_json::json;

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        git(root, &["init", "-q"]);
        std::fs::write(root.join("edit.txt"), "a\n".repeat(2000)).unwrap();
        std::fs::write(root.join("old.txt"), "moved content\n".repeat(20)).unwrap();
        std::fs::write(root.join("gone.txt"), "x\n").unwrap();
        std::fs::write(root.join("with space.txt"), "x\n").unwrap();
        git(root, &["add", "."]);
        git(root, &["commit", "-qm", "init"]);

        std::fs::write(root.join("edit.txt"), "b\n".repeat(2000)).unwrap();
        std::fs::write(root.join("with space.txt"), "y\n").unwrap();
        git(root, &["mv", "old.txt", "new.txt"]);
        git(root, &["rm", "-q", "gone.txt"]);
        std::fs::write(root.join("blob.bin"), [0u8, 1, 2, 0, 255]).unwrap();
        std::fs::write(root.join("added.txt"), "new\n").unwrap();
        git(root, &["add", "blob.bin", "added.txt"]);

        let ws = Workspace::new(root.to_path_buf()).expect("workspace");
        // 文本只要 1 KiB：edit.txt 一个文件的补丁就超了，后面的文件都不在文本里。
        let out = git_diff(&ws, &json!({"staged": true, "max_bytes": 1024})).expect("diff");
        assert_eq!(out["truncated"], true, "{out}");
        let mut files: Vec<(String, String, bool, bool, String)> = out["files"]
            .as_array()
            .expect("files")
            .iter()
            .map(|f| {
                (
                    f["path"].as_str().unwrap().to_string(),
                    f["status"].as_str().unwrap().to_string(),
                    f["binary"].as_bool().unwrap(),
                    f["staged"].as_bool().unwrap(),
                    f["old_path"].as_str().unwrap_or_default().to_string(),
                )
            })
            .collect();
        files.sort();
        let expected: Vec<(String, String, bool, bool, String)> = [
            ("added.txt", "added", false, true, ""),
            ("blob.bin", "added", true, true, ""),
            ("edit.txt", "modified", false, false, ""),
            ("gone.txt", "deleted", false, true, ""),
            ("new.txt", "renamed", false, true, "old.txt"),
            ("with space.txt", "modified", false, false, ""),
        ]
        .into_iter()
        .map(|(p, s, b, st, o)| (p.into(), s.into(), b, st, o.into()))
        .collect();
        assert_eq!(files, expected);

        git(root, &["commit", "-qam", "second"]);
        let shown = git_show(&ws, &json!({"max_bytes": 1})).expect("show");
        let mut paths: Vec<&str> = shown["files"]
            .as_array()
            .expect("files")
            .iter()
            .map(|f| f["path"].as_str().unwrap())
            .collect();
        paths.sort();
        assert_eq!(
            paths,
            [
                "added.txt",
                "blob.bin",
                "edit.txt",
                "gone.txt",
                "new.txt",
                "with space.txt"
            ],
            "{shown}"
        );
    }

    /// 项目是仓库的子目录（monorepo 里的 api/）：Git 工具看不到同一个仓库里的 web/。
    /// 以前 `git_show HEAD:web/.env` 直接读出内容，`git_diff` / `git_status` / `git_log`
    /// 列的是整个仓库（审查 D07：开给 api 的只读 grant 能读 web）。
    #[test]
    fn a_workspace_inside_a_repo_sees_only_its_own_subtree() {
        use super::{git_diff, git_log, git_show, git_status};
        use crate::tools::workspace::Workspace;
        use serde_json::json;

        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().canonicalize().unwrap();
        git(&repo, &["init", "-q"]);
        std::fs::create_dir_all(repo.join("api")).unwrap();
        std::fs::create_dir_all(repo.join("web")).unwrap();
        std::fs::write(repo.join("api/main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(repo.join("web/.env"), "WEB_SECRET=1\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-qm", "both"]);
        std::fs::write(repo.join("web/.env"), "WEB_SECRET=2\n").unwrap();
        git(&repo, &["commit", "-qam", "only web touched"]);
        std::fs::write(repo.join("web/.env"), "WEB_SECRET=3\n").unwrap();
        std::fs::write(repo.join("api/main.rs"), "fn main() { api() }\n").unwrap();

        let ws = Workspace::new(repo.join("api")).expect("workspace");
        let leaks = |value: &serde_json::Value| {
            value.to_string().contains("WEB_SECRET") || value.to_string().contains("web/")
        };

        for rev in [
            "HEAD:web/.env",
            "HEAD:./../web/.env",
            "HEAD:api/../web/.env",
            ":/only web",
        ] {
            let refused = git_show(&ws, &json!({ "rev": rev }));
            assert!(refused.is_err(), "{rev} 读到了项目外：{refused:?}");
        }
        let own = git_show(&ws, &json!({ "rev": "HEAD:./main.rs" })).expect("show own file");
        assert!(
            own["content"].as_str().unwrap().contains("fn main"),
            "{own}"
        );
        let own = git_show(&ws, &json!({ "rev": "HEAD~1:api/main.rs" })).expect("show own file");
        assert!(
            own["content"].as_str().unwrap().contains("fn main"),
            "{own}"
        );

        for magic in [":/web", ":(top)web/.env"] {
            assert!(git_diff(&ws, &json!({ "path": magic })).is_err(), "{magic}");
            assert!(git_show(&ws, &json!({ "path": magic })).is_err(), "{magic}");
        }

        let diff = git_diff(&ws, &json!({})).expect("diff");
        assert!(diff["diff"].as_str().unwrap().contains("api()"), "{diff}");
        assert!(!leaks(&diff), "{diff}");
        let status = git_status(&ws, &json!({})).expect("status");
        assert_eq!(status["entries"].as_array().unwrap().len(), 1, "{status}");
        assert!(!leaks(&status), "{status}");
        let log = git_log(&ws, &json!({})).expect("log");
        let subjects: Vec<&str> = log["commits"]
            .as_array()
            .unwrap()
            .iter()
            .map(|commit| commit["subject"].as_str().unwrap())
            .collect();
        assert_eq!(subjects, ["both"], "只动了 web 的提交不该出现：{log}");
        let first = git_show(&ws, &json!({ "rev": "HEAD~1" })).expect("show commit");
        assert!(!leaks(&first), "提交里 web 的改动漏出来了：{first}");

        // 项目就是仓库根时一切照旧：整个仓库本来就是它的。
        let whole = Workspace::new(repo.clone()).expect("workspace");
        let shown = git_show(&whole, &json!({ "rev": "HEAD:web/.env" })).expect("root show");
        assert!(
            shown["content"].as_str().unwrap().contains("WEB_SECRET=2"),
            "{shown}"
        );
        let whole_log = git_log(&whole, &json!({})).expect("root log");
        assert_eq!(whole_log["commits"].as_array().map(Vec::len), Some(2));
    }
}
