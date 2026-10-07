//! CI 这一半：`.github/workflows/*.yml` 里各 job 的 `run:` 步骤，和清单声明的命令分开列。
//!
//! 清单说的是"项目声明了哪些入口"，CI 说的是"项目实际怎么验"。ChatGPT 实测（审查 §14）里最可靠的
//! 链路就是 发现 → 读 CI → 读脚本实现 → 执行，它也照着 CI 先跑了 `pnpm install`、没问用户——
//! 那条安装步骤要是在这里标成 install，就会带上"先问用户"的提醒。只摆出来，不跑。

use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use yaml_rust2::parser::Parser;
use yaml_rust2::{Event, Yaml, YamlLoader};

use super::{read_manifest, script_preview, segment_role, touches_remote, Report};
use crate::tools::workspace::{relative_display, Workspace};

const MAX_CI_STEPS: usize = 60;
/// 工作流文件正常不过十来层。再深的不解析：解析出来的树释放时是递归的，深到几十万层会把栈撑爆。
const MAX_YAML_DEPTH: usize = 64;

pub(super) struct Step {
    pub(super) run: String,
    /// 单行、不带 `${{ }}` 的才有：只有这种能原样交给 exec_command。
    pub(super) cmd: Option<String>,
    pub(super) workdir: String,
    pub(super) role: &'static str,
    pub(super) source: String,
    name: Option<String>,
    env: Vec<String>,
    pub(super) exec: Value,
}

impl Step {
    pub(super) fn to_json(&self) -> Value {
        let mut value = json!({
            "run": self.run,
            "workdir": self.workdir,
            "role": self.role,
            "source": self.source,
            "exec": self.exec,
        });
        if let Some(name) = &self.name {
            value["name"] = json!(name);
        }
        if !self.env.is_empty() {
            value["env"] = json!(self.env);
        }
        value
    }
}

pub(super) fn add_steps(ws: &Workspace, files: &[PathBuf], report: &mut Report) {
    for path in files {
        let rel = relative_display(ws.root(), path);
        let text = match read_manifest(path) {
            Ok(text) => text,
            Err(problem) => {
                report.problems.push(format!("{rel}: {problem}"));
                continue;
            }
        };
        if let Err(problem) = safe_to_load(&text) {
            report.problems.push(format!("{rel}: {problem}"));
            continue;
        }
        let docs = match YamlLoader::load_from_str(&text) {
            Ok(docs) => docs,
            Err(error) => {
                report
                    .problems
                    .push(format!("{rel}: not valid YAML ({error})"));
                continue;
            }
        };
        let Some(doc) = docs.first() else {
            continue;
        };
        if !add_workflow(&rel, doc, report) {
            report.truncated = true;
            return;
        }
    }
}

/// 加一个工作流里的步骤；到了上限返回 false。
fn add_workflow(rel: &str, doc: &Yaml, report: &mut Report) -> bool {
    let Some(jobs) = doc["jobs"].as_hash() else {
        return true;
    };
    let workflow_dir = default_workdir(doc);
    let workflow_env = env_names(&doc["env"]);
    for (job_id, job) in jobs {
        let job_id = job_id.as_str().unwrap_or("?");
        // 优先级和 GitHub 一样：步骤自己的 > job 的 defaults > 工作流的 defaults。
        let job_dir = default_workdir(job).or_else(|| workflow_dir.clone());
        let job_env = env_names(&job["env"]);
        let Some(steps) = job["steps"].as_vec() else {
            continue;
        };
        for (index, step) in steps.iter().enumerate() {
            let Some(run) = step["run"].as_str() else {
                continue;
            };
            if report.ci_steps.len() >= MAX_CI_STEPS {
                return false;
            }
            let workdir = step["working-directory"]
                .as_str()
                .map(str::to_string)
                .or_else(|| job_dir.clone())
                .map(|dir| normalize_dir(&dir))
                .unwrap_or_else(|| ".".into());
            let single_line = run.trim();
            let runnable = !single_line.contains('\n')
                && !single_line.contains("${{")
                && !workdir.contains("${{");
            let mut env: Vec<String> = workflow_env
                .iter()
                .chain(&job_env)
                .chain(&env_names(&step["env"]))
                .cloned()
                .collect();
            env.sort();
            env.dedup();
            report.ci_steps.push(Step {
                run: script_preview(single_line),
                cmd: runnable.then(|| single_line.to_string()),
                workdir,
                role: ci_role(run),
                source: format!("{rel} jobs.{job_id}.steps[{index}]"),
                name: step["name"].as_str().map(str::to_string),
                env,
                exec: Value::Null,
            });
        }
    }
    true
}

/// 不可信的 YAML 先过一遍事件流再建树。yaml-rust2 建树时把别名指向的节点整个复制一份，
/// 十几层互相引用的锚点就能用几 KB 的文件撑出几十 GB 内存（"十亿笑"）；GitHub 的工作流
/// 基本用不到别名，有就不解析，让 AI 自己读。
fn safe_to_load(text: &str) -> Result<(), String> {
    let mut parser = Parser::new_from_str(text);
    let mut depth = 0usize;
    loop {
        let (event, _) = parser
            .next_token()
            .map_err(|error| format!("not valid YAML ({error})"))?;
        match event {
            Event::StreamEnd => return Ok(()),
            Event::Alias(_) => {
                return Err("uses YAML aliases (*name); not expanded, read the file instead".into())
            }
            Event::SequenceStart(..) | Event::MappingStart(..) => {
                depth += 1;
                if depth > MAX_YAML_DEPTH {
                    return Err(format!(
                        "nested deeper than {MAX_YAML_DEPTH} levels; not parsed"
                    ));
                }
            }
            Event::SequenceEnd | Event::MappingEnd => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
}

fn default_workdir(node: &Yaml) -> Option<String> {
    node["defaults"]["run"]["working-directory"]
        .as_str()
        .map(str::to_string)
}

/// 只要名字：值常是 `${{ secrets.X }}`，也可能直接写着口令。
fn env_names(node: &Yaml) -> Vec<String> {
    node.as_hash()
        .map(|env| {
            env.keys()
                .filter_map(|key| key.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// `./web/` → `web`，空和 `./` → `.`：和清单那边的 workdir 写法一致，能原样交给 exec_command。
fn normalize_dir(raw: &str) -> String {
    let trimmed = raw.trim().trim_end_matches('/');
    let trimmed = trimmed.strip_prefix("./").unwrap_or(trimmed);
    if trimmed.is_empty() || trimmed == "." {
        ".".into()
    } else {
        trimmed.to_string()
    }
}

/// 按命令内容猜：先认装依赖、再认会动远端的，再找第一个认得出的子命令
/// （`cargo clippy …` → lint，`pnpm run test` → test）。
fn ci_role(run: &str) -> &'static str {
    if installs(run) {
        return "install";
    }
    if touches_remote(run) {
        return "deploy";
    }
    // 和脚本名一样按 `:`、`-` 拆开认：`pnpm test:unit`、`pnpm exec vue-tsc`。
    run.split_whitespace()
        .filter(|token| !token.starts_with('-'))
        .flat_map(|token| token.split([':', '-', '_', '.', '/']))
        .map(|segment| segment_role(&segment.to_ascii_lowercase()))
        .find(|role| *role != "other")
        .unwrap_or("other")
}

/// 会下载依赖的步骤：`pnpm install`、`npm ci`、`pip install`、`uv sync`、`playwright install` 这类。
fn installs(run: &str) -> bool {
    run.lines().any(|line| {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        tokens.iter().enumerate().any(|(index, token)| {
            let previous = index.checked_sub(1).map(|at| tokens[at]);
            *token == "install"
                || (matches!(*token, "ci" | "i" | "add")
                    && matches!(previous, Some("npm" | "pnpm" | "yarn" | "bun")))
                || (*token == "sync" && previous == Some("uv"))
                || (*token == "download" && previous == Some("mod"))
        })
    })
}

/// 工作流文件：起点和工作区根两处的 `.github/workflows/*.yml|yaml`，各取按名字排的前 10 个。
pub(super) fn workflow_files(ws: &Workspace, start: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![start.to_path_buf()];
    if start != ws.root() {
        dirs.push(ws.root().to_path_buf());
    }
    let mut found = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir.join(".github").join("workflows")) else {
            continue;
        };
        let mut files: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.is_file()
                    && matches!(
                        path.extension().and_then(|ext| ext.to_str()),
                        Some("yml" | "yaml")
                    )
                    && ws.is_safe_existing_path(path)
            })
            .collect();
        files.sort();
        found.extend(files.into_iter().take(10));
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_come_from_the_command() {
        assert_eq!(ci_role("pnpm install --frozen-lockfile"), "install");
        assert_eq!(ci_role("npm ci"), "install");
        assert_eq!(ci_role("npx playwright install --with-deps"), "install");
        assert_eq!(ci_role("cargo test --locked"), "test");
        assert_eq!(ci_role("cargo clippy --workspace -- -D warnings"), "lint");
        assert_eq!(ci_role("cargo fmt --all -- --check"), "format");
        assert_eq!(ci_role("pnpm run build"), "build");
        assert_eq!(ci_role("npx wrangler deploy"), "deploy");
        assert_eq!(ci_role("node scripts/check-version.mjs"), "other");
        assert_eq!(ci_role("pnpm test:unit"), "test");
        assert_eq!(ci_role("pnpm exec vue-tsc --noEmit"), "typecheck");
        assert_eq!(
            ci_role("cargo test --manifest-path src-tauri/Cargo.toml"),
            "test"
        );
    }

    #[test]
    fn workdirs_are_written_the_way_exec_command_takes_them() {
        assert_eq!(normalize_dir("./web/"), "web");
        assert_eq!(normalize_dir("./"), ".");
        assert_eq!(normalize_dir(""), ".");
        assert_eq!(normalize_dir("apps/site"), "apps/site");
    }

    /// 几 KB 的文件靠别名就能展开成几十 GB；有别名就不建树。
    #[test]
    fn aliases_and_deep_nesting_are_refused_before_building_the_tree() {
        let laughs = "a: &a [x, x]\nb: &b [*a, *a]\nc: &c [*b, *b]\n";
        assert!(safe_to_load(laughs).unwrap_err().contains("aliases"));
        let deep = format!("{}x{}", "[".repeat(100), "]".repeat(100));
        assert!(safe_to_load(&deep).unwrap_err().contains("nested"));
        assert!(safe_to_load("jobs:\n  test:\n    steps:\n      - run: cargo test\n").is_ok());
    }
}
