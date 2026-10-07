//! Cargo 这一半。Cargo 没有 scripts：`build` / `test` / `run` 是它自带的，
//! 项目自己写的只有 `.cargo/config.toml` 里的 `[alias]`。

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde_json::json;

use super::{
    body_watches, manifest_dir, read_manifest, role_for, script_preview, Command, Report,
    MAX_LISTED_MEMBERS, MAX_RUN_TARGETS,
};
use crate::tools::workspace::{relative_display, Workspace};

pub(super) struct Manifest {
    dir: PathBuf,
    rel_dir: String,
    file: String,
    package: Option<String>,
    is_workspace_root: bool,
    excluded: Vec<PathBuf>,
    bins: Vec<String>,
    rust_version: Option<String>,
}

impl Manifest {
    pub(super) fn new(ws: &Workspace, path: &Path, table: &toml::Table) -> Self {
        let (dir, rel_dir) = manifest_dir(ws, path);
        let package = table.get("package").and_then(toml::Value::as_table);
        let package_name = package
            .and_then(|package| package.get("name"))
            .and_then(toml::Value::as_str)
            .map(str::to_string);
        let workspace = table.get("workspace").and_then(toml::Value::as_table);
        let excluded = workspace
            .and_then(|workspace| workspace.get("exclude"))
            .and_then(toml::Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(toml::Value::as_str)
                    .map(|item| dir.join(item))
                    .collect()
            })
            .unwrap_or_default();
        // `rust-version.workspace = true` 是个表，不是字符串，那种就从 workspace.package 取。
        let rust_version = package
            .and_then(|package| package.get("rust-version"))
            .and_then(toml::Value::as_str)
            .or_else(|| {
                workspace
                    .and_then(|workspace| workspace.get("package"))
                    .and_then(|package| package.get("rust-version"))
                    .and_then(toml::Value::as_str)
            })
            .map(str::to_string);
        let bins = match &package_name {
            Some(name) => cargo_bins(&dir, name, table),
            None => Vec::new(),
        };
        Self {
            file: relative_display(ws.root(), path),
            dir,
            rel_dir,
            package: package_name,
            is_workspace_root: workspace.is_some(),
            excluded,
            bins,
            rust_version,
        }
    }
}

/// 这个包有哪些可执行目标：`[[bin]]` 写明的，加上 Cargo 按目录约定自动认的
/// （`src/main.rs` 用包名，`src/bin/x.rs`、`src/bin/x/main.rs` 用 x）。
fn cargo_bins(dir: &Path, package: &str, table: &toml::Table) -> Vec<String> {
    let mut bins = BTreeSet::new();
    if let Some(declared) = table.get("bin").and_then(toml::Value::as_array) {
        for bin in declared {
            if let Some(name) = bin.get("name").and_then(toml::Value::as_str) {
                bins.insert(name.to_string());
            }
        }
    }
    let autobins = table
        .get("package")
        .and_then(|package| package.get("autobins"))
        .and_then(toml::Value::as_bool)
        .unwrap_or(true);
    if autobins {
        if dir.join("src").join("main.rs").is_file() {
            bins.insert(package.to_string());
        }
        if let Ok(entries) = std::fs::read_dir(dir.join("src").join("bin")) {
            for entry in entries.filter_map(Result::ok) {
                let path = entry.path();
                let single_file =
                    path.is_file() && path.extension().and_then(|ext| ext.to_str()) == Some("rs");
                if !single_file && !path.join("main.rs").is_file() {
                    continue;
                }
                if let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) {
                    bins.insert(stem.to_string());
                }
            }
        }
    }
    bins.into_iter().collect()
}

pub(super) fn add_projects(ws: &Workspace, manifests: &[Manifest], report: &mut Report) {
    // 在某个 workspace 根目录底下的包算它的成员（Cargo 自己也是往上找 workspace 根），
    // 命令在根上跑一遍就覆盖了；被 `exclude` 掉的才单算。
    let is_member_of = |member: &Manifest, root: &Manifest| {
        root.is_workspace_root
            && member.dir != root.dir
            && member.dir.starts_with(&root.dir)
            && !root
                .excluded
                .iter()
                .any(|excluded| member.dir.starts_with(excluded))
    };
    for root in manifests {
        if manifests.iter().any(|other| is_member_of(root, other)) {
            continue;
        }
        let members: Vec<&Manifest> = manifests
            .iter()
            .filter(|member| is_member_of(member, root))
            .collect();
        let workdir = root.rel_dir.as_str();
        let mut project = json!({
            "kind": "cargo",
            "manifest": root.file,
            "workdir": workdir,
            "package": root.package,
            "workspace": root.is_workspace_root,
        });
        if root.is_workspace_root {
            let listed: Vec<&str> = members
                .iter()
                .take(MAX_LISTED_MEMBERS)
                .map(|member| member.rel_dir.as_str())
                .collect();
            project["members"] = json!(listed);
            project["members_found"] = json!(members.len());
        }
        if let Some(toolchain) = rust_toolchain(&root.dir) {
            project["toolchain"] = json!(toolchain);
        }
        if let Some(version) = &root.rust_version {
            project["rust_version"] = json!(version);
        }
        report.projects.push(project);

        let source = format!("cargo built-in ({})", root.file);
        report.commands.push(Command::new(
            &["cargo", "build"],
            workdir,
            "build",
            source.clone(),
        ));
        report.commands.push(Command::new(
            &["cargo", "test"],
            workdir,
            "test",
            source.clone(),
        ));

        // 只有一个可执行目标且就在根包里时 `cargo run` 不用挑；否则每个都写明 --bin，
        // 同名的再加 -p。让 AI 自己去猜该挑哪个，它会先跑一次报错的。
        let mut targets: Vec<(&str, &str)> = Vec::new();
        for manifest in std::iter::once(root).chain(members.iter().copied()) {
            if let Some(package) = &manifest.package {
                for bin in &manifest.bins {
                    targets.push((package.as_str(), bin.as_str()));
                }
            }
        }
        if targets.len() == 1 && root.package.as_deref() == Some(targets[0].0) {
            report.commands.push(Command::new(
                &["cargo", "run"],
                workdir,
                "run",
                source.clone(),
            ));
        } else {
            if targets.len() > MAX_RUN_TARGETS {
                report.ambiguities.push(format!(
                    "{}: {} binary targets, only the first {MAX_RUN_TARGETS} are listed as cargo run commands",
                    root.file,
                    targets.len()
                ));
            }
            for (package, bin) in targets.iter().take(MAX_RUN_TARGETS) {
                let duplicated = targets.iter().filter(|(_, other)| other == bin).count() > 1;
                let argv: Vec<&str> = if duplicated {
                    vec!["cargo", "run", "-p", package, "--bin", bin]
                } else {
                    vec!["cargo", "run", "--bin", bin]
                };
                report
                    .commands
                    .push(Command::new(&argv, workdir, "run", source.clone()));
            }
        }

        for (alias, expansion, file) in cargo_aliases(ws, &root.dir) {
            let mut command = Command::new(
                &["cargo", alias.as_str()],
                workdir,
                role_for(&alias, &expansion),
                format!("{file} alias.{alias}"),
            );
            command.declared = true;
            command.long_running = command.role == "dev_server" || body_watches(&expansion);
            command.script = Some(script_preview(&expansion));
            report.commands.push(command);
        }
    }
}

/// `rust-toolchain.toml`（或老式的 `rust-toolchain`）钉的版本：本机装的不是这个，
/// rustup 会在第一次跑 cargo 时去下载。
fn rust_toolchain(dir: &Path) -> Option<String> {
    let toml_path = dir.join("rust-toolchain.toml");
    if let Ok(text) = read_manifest(&toml_path) {
        let channel = text
            .parse::<toml::Table>()
            .ok()?
            .get("toolchain")?
            .get("channel")?
            .as_str()?
            .to_string();
        return Some(format!("{channel} (rust-toolchain.toml)"));
    }
    let plain = read_manifest(&dir.join("rust-toolchain")).ok()?;
    let channel = plain.lines().next()?.trim();
    (!channel.is_empty()).then(|| format!("{channel} (rust-toolchain)"))
}

fn cargo_aliases(ws: &Workspace, dir: &Path) -> Vec<(String, String, String)> {
    for name in ["config.toml", "config"] {
        let path = dir.join(".cargo").join(name);
        if !path.is_file() || !ws.is_safe_existing_path(&path) {
            continue;
        }
        let Ok(table) = read_manifest(&path).and_then(|text| {
            text.parse::<toml::Table>()
                .map_err(|error| error.to_string())
        }) else {
            return Vec::new();
        };
        let file = relative_display(ws.root(), &path);
        let Some(aliases) = table.get("alias").and_then(toml::Value::as_table) else {
            return Vec::new();
        };
        return aliases
            .iter()
            .filter_map(|(alias, value)| {
                let expansion = match value {
                    toml::Value::String(text) => text.clone(),
                    toml::Value::Array(parts) => parts
                        .iter()
                        .filter_map(toml::Value::as_str)
                        .collect::<Vec<_>>()
                        .join(" "),
                    _ => return None,
                };
                Some((alias.clone(), expansion, file.clone()))
            })
            .collect();
    }
    Vec::new()
}
