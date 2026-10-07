//! 项目自己声明了哪些构建、测试、运行命令（D10 任务发现）。
//!
//! D10 第三轮里 AI 要先翻 README、CI、`docs/development.md` 才知道怎么跑测试；前两轮的
//! 命令都是提示词里给的。这个工具把 `Cargo.toml` / `package.json` 里**已经写好的**入口
//! 列出来，每条给出能原样交给 `exec_command` 的 `argv` + `workdir`。
//!
//! **只读**：不起进程、不装依赖，也不跑 `cargo metadata`（它会去拉索引、可能联网）。
//! 能不能跑走 [`crate::tools::exec::check_command`] 同一条判定，自己不另写一套——
//! 两边各判各的，迟早出现"发现说能跑、真跑被拒"。
//!
//! 角色（test / build / dev_server…）是按名字猜的，回包里写明是猜的；
//! 测没测全、还要哪些环境变量和参数，仍以项目文档和 CI 为准（`other_sources`）。

mod cargo;
mod node;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use walkdir::WalkDir;

use crate::tools::context::ToolContext;
use crate::tools::workspace::{relative_display, tool_ok, Workspace, WorkspaceError};

/// 从扫描起点往下最多几层。monorepo 常见的 `apps/web/package.json` 是 3 层，
/// 再深的多半是 fixture 或示例，要的话用 `path` 指到更深处。
const MAX_DEPTH: usize = 6;
/// 清单文件最多读几个，浅的优先。超过就截断并说明，不是悄悄少列。
/// xdo 那样的 monorepo（几十个 Cargo 成员加一堆前端包）有 62 个。
const MAX_MANIFESTS: usize = 100;
/// 遍历最多看这么多个目录项，挡住没被剪枝的巨型目录（比如没叫 node_modules 的依赖缓存）。
const MAX_WALK_ENTRIES: usize = 50_000;
const MAX_COMMANDS: usize = 150;
/// 比这还大的 package.json / Cargo.toml 不是正常的清单，不读。
const MAX_MANIFEST_BYTES: u64 = 1 << 20;
/// 脚本正文只回这么多字符：让 AI 认出它在干什么就够了，全文用 read_file 看。
const MAX_SCRIPT_CHARS: usize = 200;
const MAX_LISTED_MEMBERS: usize = 20;
const MAX_RUN_TARGETS: usize = 10;

/// 常写着"怎么构建、怎么测"但这一版不解析的文件。只报在不在，让 AI 自己去读。
const OTHER_SOURCES: &[&str] = &[
    "AGENTS.md",
    "CLAUDE.md",
    "CONTRIBUTING.md",
    "DEVELOPMENT.md",
    "docs/development.md",
    "docs/CONTRIBUTING.md",
    "Makefile",
    "GNUmakefile",
    "justfile",
    "Justfile",
    "Taskfile.yml",
    "Taskfile.yaml",
    "pyproject.toml",
    "go.mod",
    ".gitlab-ci.yml",
];

pub fn list_project_commands(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    let ws = &ctx.workspace;
    let raw = args.get("path").and_then(Value::as_str).unwrap_or(".");
    // resolve_existing 不认绝对路径和 `..`：发现出来的 workdir 要能原样交给 exec_command，
    // 而 exec_command 只在工作区里跑。
    let start = ws.resolve_existing(raw)?;
    if !start.path.is_dir() {
        return Err(WorkspaceError::not_a_directory("path is not a directory"));
    }

    let mut report = Report::default();
    let manifests = find_manifests(ws, &start.path, &mut report);

    let mut cargo = Vec::new();
    let mut node = Vec::new();
    for path in manifests {
        let rel = relative_display(ws.root(), &path);
        let text = match read_manifest(&path) {
            Ok(text) => text,
            Err(problem) => {
                report.problems.push(format!("{rel}: {problem}"));
                continue;
            }
        };
        let name = path.file_name().and_then(|name| name.to_str());
        if name == Some("Cargo.toml") {
            match text.parse::<toml::Table>() {
                Ok(table) => cargo.push(cargo::Manifest::new(ws, &path, &table)),
                Err(error) => report.problems.push(format!(
                    "{rel}: not valid TOML ({})",
                    first_line(&error.to_string())
                )),
            }
        } else {
            match serde_json::from_str::<Value>(&text) {
                Ok(value) if value.is_object() => node.push(node::Package::new(ws, &path, &value)),
                Ok(_) => report.problems.push(format!("{rel}: not a JSON object")),
                Err(error) => report
                    .problems
                    .push(format!("{rel}: not valid JSON ({error})")),
            }
        }
    }

    cargo::add_projects(ws, &cargo, &mut report);
    node::add_projects(ws, &node, &mut report);

    if report.commands.len() > MAX_COMMANDS {
        report.commands.truncate(MAX_COMMANDS);
        report.truncated = true;
    }
    for command in &mut report.commands {
        command.exec = exec_decision(ctx, &command.argv, &command.workdir);
    }

    let other_sources = other_sources(ws, &start.path);
    Ok(tool_ok(report.into_json(start.display, other_sources)))
}

#[derive(Default)]
struct Report {
    projects: Vec<Value>,
    commands: Vec<Command>,
    ambiguities: Vec<String>,
    problems: Vec<String>,
    truncated: bool,
}

struct Command {
    argv: Vec<String>,
    workdir: String,
    role: &'static str,
    source: String,
    /// 项目自己写的（scripts、cargo alias）还是 Cargo / 包管理器本来就有的。
    declared: bool,
    script: Option<String>,
    long_running: bool,
    exec: Value,
}

impl Command {
    fn new(argv: &[&str], workdir: &str, role: &'static str, source: String) -> Self {
        Self {
            argv: argv.iter().map(|part| part.to_string()).collect(),
            workdir: workdir.to_string(),
            role,
            source,
            declared: false,
            script: None,
            long_running: role == "dev_server",
            exec: Value::Null,
        }
    }

    fn to_json(&self) -> Value {
        let mut value = json!({
            "argv": self.argv,
            "workdir": self.workdir,
            "role": self.role,
            "source": self.source,
            "declared": self.declared,
            "long_running": self.long_running,
            "exec": self.exec,
        });
        if let Some(script) = &self.script {
            value["script"] = json!(script);
        }
        value
    }
}

impl Report {
    fn into_json(mut self, path: String, other_sources: Vec<String>) -> Value {
        // monorepo 里同一个根目录下的每个包都会把根上的锁文件冲突报一遍。
        let mut seen = BTreeSet::new();
        self.ambiguities.retain(|line| seen.insert(line.clone()));
        let mut notes = vec![
            "Nothing was run or installed. exec is the same check exec_command makes (see check_command); it says whether the command would be accepted, not whether it will pass.".to_string(),
            "role is guessed from the script or alias name. These are the entry points the manifests declare, not necessarily the project's full check: CI workflows and contributor docs (other_sources) often add flags, environment variables or extra steps.".to_string(),
        ];
        if self.commands.iter().any(|command| command.declared) {
            notes.push("A script runs through the package manager or cargo, so the allowlist only checks the outer program; read script to see what it actually does.".into());
        }
        if self.commands.iter().any(|command| command.long_running) {
            notes.push("long_running commands keep going until stopped, and exec_command stops them at timeout_ms (at most 10 minutes) even in the background. For browser tests, let the test runner start and stop the server (for example Playwright webServer).".into());
        }
        if self
            .commands
            .iter()
            .any(|command| command.role == "install")
        {
            notes.push("install downloads packages from the network and runs their install scripts. Ask the user before running it.".into());
        }
        if self.commands.iter().any(|command| command.role == "deploy") {
            notes.push("deploy-like scripts may publish or change things outside this machine. Run one only after the user explicitly asked for it.".into());
        }
        if self.truncated {
            notes.push(format!(
                "Results were cut off (at most {MAX_MANIFESTS} manifests, shallowest first, {MAX_COMMANDS} commands, {MAX_DEPTH} directory levels). Pass a deeper path to see one project in full."
            ));
        }
        json!({
            "path": path,
            "projects": self.projects,
            "commands": self.commands.iter().map(Command::to_json).collect::<Vec<_>>(),
            "ambiguities": self.ambiguities,
            "problems": self.problems,
            "other_sources": other_sources,
            "truncated": self.truncated,
            "side_effects": "none",
            "notes": notes,
        })
    }
}

/// 找 `Cargo.toml` 和 `package.json`。剪枝规则和 list_files 一样：点开头的目录、
/// `node_modules`、`target` 这些整棵不进（[`Workspace::is_ignored_path`]）。
fn find_manifests(ws: &Workspace, start: &Path, report: &mut Report) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut visited = 0usize;
    let walker = WalkDir::new(start)
        .max_depth(MAX_DEPTH)
        // 不跟软链：指到工作区外面的目录不该被当成这个项目的一部分。
        .follow_links(false)
        // 截断时留下的那一批要每次一样。
        .sort_by_file_name()
        .into_iter()
        .filter_entry(|entry| {
            entry.path() == start
                || !entry.file_type().is_dir()
                || !ws.is_ignored_path(entry.path(), false, false)
        });
    for entry in walker.filter_map(Result::ok) {
        visited += 1;
        if visited > MAX_WALK_ENTRIES {
            report.truncated = true;
            break;
        }
        let name = entry.file_name();
        if entry.file_type().is_file() && (name == "Cargo.toml" || name == "package.json") {
            found.push(entry.into_path());
        }
    }
    // 浅的在前，截断也从深处截：先收齐再排，不是走到第几个就停——按名字深度优先走，
    // 停下时没走到的往往是排在后面的整棵 `packages/`。判断 Cargo 成员、包管理器根目录
    // 也都要先见到上层。
    found.sort_by_key(|path| (path.components().count(), path.clone()));
    if found.len() > MAX_MANIFESTS {
        found.truncate(MAX_MANIFESTS);
        report.truncated = true;
    }
    found
}

fn read_manifest(path: &Path) -> Result<String, String> {
    let meta = std::fs::metadata(path).map_err(|error| error.to_string())?;
    if meta.len() > MAX_MANIFEST_BYTES {
        return Err(format!(
            "skipped, {} bytes is larger than the {MAX_MANIFEST_BYTES}-byte limit",
            meta.len()
        ));
    }
    std::fs::read_to_string(path).map_err(|error| error.to_string())
}

fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or(text).trim()
}

fn manifest_dir(ws: &Workspace, manifest: &Path) -> (PathBuf, String) {
    let dir = manifest.parent().unwrap_or(ws.root()).to_path_buf();
    let rel = relative_display(ws.root(), &dir);
    (dir, rel)
}

/// 交给 exec_command 前它会怎么判。和 check_command 是同一个函数、同一份参数。
fn exec_decision(ctx: &ToolContext, argv: &[String], workdir: &str) -> Value {
    let checked =
        crate::tools::exec::check_command(ctx, &json!({ "argv": argv, "workdir": workdir }));
    let Ok(result) = checked else {
        return json!({ "decision": "unknown" });
    };
    let mut decision = json!({
        "decision": result["decision"],
        "rule": result["rule"],
        "program_found": result["program"]["found"],
    });
    if result["decision"] != "allow" {
        decision["suggestion"] = result["suggestion"].clone();
    }
    decision
}

fn other_sources(ws: &Workspace, start: &Path) -> Vec<String> {
    let mut dirs = vec![start.to_path_buf()];
    if start != ws.root() {
        dirs.push(ws.root().to_path_buf());
    }
    let mut found = BTreeSet::new();
    for dir in dirs {
        for name in OTHER_SOURCES {
            let path = dir.join(name);
            if path.is_file() && ws.is_safe_existing_path(&path) {
                found.insert(relative_display(ws.root(), &path));
            }
        }
        // CI 配置往往是"项目到底怎么测"最准的一份：flag、环境变量、测试隔离都在里面。
        let workflows = dir.join(".github").join("workflows");
        if let Ok(entries) = std::fs::read_dir(&workflows) {
            let mut files: Vec<PathBuf> = entries
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|path| {
                    path.is_file()
                        && matches!(
                            path.extension().and_then(|ext| ext.to_str()),
                            Some("yml" | "yaml")
                        )
                })
                .collect();
            files.sort();
            for path in files.into_iter().take(10) {
                if ws.is_safe_existing_path(&path) {
                    found.insert(relative_display(ws.root(), &path));
                }
            }
        }
    }
    found.into_iter().collect()
}

/// 按名字猜一条脚本是干什么的。只用来排序和提醒（长时间运行、要联网、会发布），
/// 不拿它做任何放行判断。
fn role_for(name: &str, body: &str) -> &'static str {
    let lower = name.to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "preinstall"
            | "install"
            | "postinstall"
            | "prepare"
            | "prepublish"
            | "prepublishonly"
            | "prepack"
            | "postpack"
            | "postpublish"
            | "preversion"
            | "version"
            | "postversion"
    ) {
        return "hook";
    }
    if matches!(
        lower.as_str(),
        "typecheck" | "type-check" | "type:check" | "types" | "tsc"
    ) {
        return "typecheck";
    }
    // 正文会动这台机器以外的东西就按 deploy 提醒，不管名字叫什么：`db:migrate` 的正文
    // 是 `wrangler d1 execute ... --remote`，改的是线上数据库。宁可多提醒一次。
    if touches_remote(body) {
        return "deploy";
    }
    // 第一段认不出来再看后面几段：`tauri:build`、`devtools:bundle`。
    let role = lower
        .split([':', '-', '_', '.'])
        .map(segment_role)
        .find(|role| *role != "other")
        .unwrap_or("other");
    if role == "other" && body_watches(body) {
        return "dev_server";
    }
    role
}

fn segment_role(segment: &str) -> &'static str {
    match segment {
        "test" | "tests" | "e2e" | "spec" | "smoke" | "vitest" | "jest" | "playwright"
        | "cypress" => "test",
        "build" | "compile" | "bundle" => "build",
        "dev" | "start" | "serve" | "server" | "preview" | "watch" | "storybook" => "dev_server",
        "lint" | "eslint" | "clippy" => "lint",
        "format" | "fmt" | "prettier" => "format",
        "typecheck" => "typecheck",
        "clean" => "clean",
        "deploy" | "release" | "publish" | "ship" => "deploy",
        _ => "other",
    }
}

/// 正文里有会发布、会改远端的词（`wrangler deploy`、`npm publish`、`--remote`、`vercel --prod`）。
/// 按整词比，`--release`、`preflight:production` 这种不算。
fn touches_remote(body: &str) -> bool {
    body.split_whitespace().any(|token| {
        matches!(
            token,
            "deploy" | "publish" | "release" | "--remote" | "--prod" | "--production"
        )
    })
}

/// 脚本正文里明说了会一直跑（`--watch`、`-w`、`watch` 子命令）。
fn body_watches(body: &str) -> bool {
    body.split_whitespace()
        .any(|token| matches!(token, "--watch" | "-w" | "watch"))
}

/// 回包里的脚本正文：先过一遍 list_runs 也用的脱敏，再截短。
fn script_preview(body: &str) -> String {
    let mut text = body.to_string();
    crate::tools::history::redact_text(&mut text);
    truncate_chars(&text, MAX_SCRIPT_CHARS)
}

fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut cut: String = text.chars().take(max).collect();
    cut.push('…');
    cut
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_are_guessed_from_the_name_first() {
        assert_eq!(role_for("test", "vitest run"), "test");
        assert_eq!(role_for("test:e2e", "playwright test"), "test");
        assert_eq!(role_for("e2e", "playwright test"), "test");
        assert_eq!(role_for("build", "vite build"), "build");
        assert_eq!(role_for("dev", "vite"), "dev_server");
        assert_eq!(role_for("start", "node server.js"), "dev_server");
        assert_eq!(role_for("type-check", "tsc --noEmit"), "typecheck");
        assert_eq!(role_for("lint:fix", "eslint . --fix"), "lint");
        assert_eq!(role_for("postinstall", "patch-package"), "hook");
        assert_eq!(role_for("release", "changeset publish"), "deploy");
        assert_eq!(
            role_for("tailwind", "tailwindcss -i in.css --watch"),
            "dev_server"
        );
        assert_eq!(role_for("gen", "node scripts/gen.js"), "other");
        assert_eq!(role_for("tauri:build", "tauri build"), "build");
        assert_eq!(role_for("smoke:dev", "node scripts/smoke-dev.mjs"), "test");
    }

    /// 名字看不出来、正文会改线上的，也要提醒：xwshare 的 `db:migrate` 就是这样。
    #[test]
    fn a_body_that_touches_remote_is_deploy_whatever_the_name() {
        assert_eq!(
            role_for("db:migrate", "wrangler d1 execute db --remote --file=m.sql"),
            "deploy"
        );
        assert_eq!(
            role_for(
                "db:migrate:local",
                "wrangler d1 execute db --local --file=m.sql"
            ),
            "other"
        );
        assert_eq!(role_for("ship-it", "vercel --prod"), "deploy");
        assert_eq!(role_for("build", "cargo build --release"), "build");
        assert_eq!(
            role_for(
                "build:production",
                "pnpm run preflight:production && pnpm build"
            ),
            "build"
        );
    }

    #[test]
    fn script_previews_are_redacted() {
        let preview = script_preview("curl -H 'Authorization: Bearer abcdef123456' x");
        assert!(!preview.contains("abcdef123456"), "{preview}");
    }

    #[test]
    fn long_script_bodies_are_cut_with_a_marker() {
        let body = "x".repeat(MAX_SCRIPT_CHARS + 5);
        let cut = truncate_chars(&body, MAX_SCRIPT_CHARS);
        assert_eq!(cut.chars().count(), MAX_SCRIPT_CHARS + 1);
        assert!(cut.ends_with('…'));
    }
}
