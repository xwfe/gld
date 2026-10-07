//! package.json 这一半：scripts、该用哪个包管理器、依赖装没装。

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use super::{body_watches, manifest_dir, read_manifest, role_for, script_preview, Command, Report};
use crate::tools::workspace::{relative_display, Workspace};

/// 包管理器靠哪个锁文件认出来。顺序就是几个锁文件同时在时的取舍顺序，
/// 那种情况会写进 `ambiguities`，不是悄悄选一个。
const LOCKFILES: &[(&str, &str)] = &[
    ("pnpm-lock.yaml", "pnpm"),
    ("yarn.lock", "yarn"),
    ("bun.lock", "bun"),
    ("bun.lockb", "bun"),
    ("package-lock.json", "npm"),
    ("npm-shrinkwrap.json", "npm"),
];

pub(super) struct Package {
    dir: PathBuf,
    rel_dir: String,
    file: String,
    name: Option<String>,
    scripts: Vec<(String, String)>,
    has_dependencies: bool,
    node_version: Option<String>,
}

impl Package {
    pub(super) fn new(ws: &Workspace, path: &Path, value: &Value) -> Self {
        let (dir, rel_dir) = manifest_dir(ws, path);
        let scripts = value
            .get("scripts")
            .and_then(Value::as_object)
            .map(|scripts| {
                scripts
                    .iter()
                    .filter_map(|(name, body)| Some((name.clone(), body.as_str()?.to_string())))
                    .collect()
            })
            .unwrap_or_default();
        let has_dependencies = ["dependencies", "devDependencies", "optionalDependencies"]
            .iter()
            .any(|key| {
                value
                    .get(*key)
                    .and_then(Value::as_object)
                    .is_some_and(|deps| !deps.is_empty())
            });
        let node_version = value
            .pointer("/engines/node")
            .and_then(Value::as_str)
            .map(|version| format!("{version} (engines.node)"))
            .or_else(|| {
                value
                    .pointer("/volta/node")
                    .and_then(Value::as_str)
                    .map(|version| format!("{version} (volta.node)"))
            })
            .or_else(|| version_file(&dir));
        Self {
            file: relative_display(ws.root(), path),
            dir,
            rel_dir,
            name: value
                .get("name")
                .and_then(Value::as_str)
                .map(str::to_string),
            scripts,
            has_dependencies,
            node_version,
        }
    }
}

fn version_file(dir: &Path) -> Option<String> {
    for name in [".nvmrc", ".node-version"] {
        if let Ok(text) = read_manifest(&dir.join(name)) {
            let version = text.lines().next().unwrap_or("").trim();
            if !version.is_empty() {
                return Some(format!("{version} ({name})"));
            }
        }
    }
    None
}

/// 一个包该用哪个包管理器、它的根目录（锁文件和 `node_modules` 通常在那儿）在哪。
struct PackageManager {
    name: String,
    source: String,
    root: PathBuf,
}

/// 从包所在目录往上找到工作区根：先遇到写了 `packageManager` 字段或有锁文件的那一层
/// 就是根。monorepo 的成员包自己没有锁文件，靠的就是往上找。
fn package_manager_for(
    ws: &Workspace,
    dir: &Path,
    ambiguities: &mut Vec<String>,
) -> PackageManager {
    let mut cursor = Some(dir);
    while let Some(current) = cursor {
        if !current.starts_with(ws.root()) {
            break;
        }
        let declared = declared_package_manager(&current.join("package.json"));
        let locks: Vec<(&str, &str)> = LOCKFILES
            .iter()
            .copied()
            .filter(|(file, _)| current.join(file).is_file())
            .collect();
        if declared.is_some() || !locks.is_empty() {
            let where_ = relative_display(ws.root(), current);
            return decide_package_manager(declared, &locks, current, &where_, ambiguities);
        }
        cursor = current.parent();
    }
    ambiguities.push(format!(
        "{}: no lockfile and no packageManager field up to the workspace root; assumed npm",
        relative_display(ws.root(), dir)
    ));
    PackageManager {
        name: "npm".into(),
        source: "assumed (no lockfile)".into(),
        root: dir.to_path_buf(),
    }
}

fn decide_package_manager(
    declared: Option<String>,
    locks: &[(&str, &str)],
    root: &Path,
    where_: &str,
    ambiguities: &mut Vec<String>,
) -> PackageManager {
    let lock_names: Vec<&str> = locks.iter().map(|(file, _)| *file).collect();
    if let Some(declared) = declared {
        // "pnpm@9.12.0+sha512..." → pnpm
        let name = declared.split('@').next().unwrap_or("").to_string();
        if matches!(name.as_str(), "npm" | "pnpm" | "yarn" | "bun") {
            if !locks.is_empty() && !locks.iter().any(|(_, manager)| *manager == name) {
                ambiguities.push(format!(
                    "{where_}: packageManager says {name} but the lockfile is {}; used {name}",
                    lock_names.join(", ")
                ));
            }
            return PackageManager {
                name,
                source: format!("packageManager field ({declared})"),
                root: root.to_path_buf(),
            };
        }
        ambiguities.push(format!(
            "{where_}: packageManager \"{declared}\" is not one of npm, pnpm, yarn, bun; went by the lockfile"
        ));
    }
    let Some((file, manager)) = locks.first() else {
        return PackageManager {
            name: "npm".into(),
            source: "assumed (unrecognized packageManager, no lockfile)".into(),
            root: root.to_path_buf(),
        };
    };
    let managers: BTreeSet<&str> = locks.iter().map(|(_, manager)| *manager).collect();
    if managers.len() > 1 {
        ambiguities.push(format!(
            "{where_}: several lockfiles ({}); used {manager}. Check the README or CI for the one the project really uses",
            lock_names.join(", ")
        ));
    }
    PackageManager {
        name: manager.to_string(),
        source: file.to_string(),
        root: root.to_path_buf(),
    }
}

fn declared_package_manager(package_json: &Path) -> Option<String> {
    let text = read_manifest(package_json).ok()?;
    let value: Value = serde_json::from_str(&text).ok()?;
    value
        .get("packageManager")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// `node_modules` 在包目录或往上到包管理器根目录之间的任何一层都算装了：Node 找依赖就是
/// 这么往上找的。只看在不在，不保证装全、装对版本。
fn dependencies_installed(dir: &Path, root: &Path) -> bool {
    let mut cursor = Some(dir);
    while let Some(current) = cursor {
        if current.join("node_modules").is_dir() {
            return true;
        }
        if current == root || !current.starts_with(root) {
            break;
        }
        cursor = current.parent();
    }
    false
}

pub(super) fn add_projects(ws: &Workspace, packages: &[Package], report: &mut Report) {
    // 同一个根目录只提示装一次依赖。
    let mut install_offered: BTreeSet<PathBuf> = BTreeSet::new();
    for package in packages {
        let manager = package_manager_for(ws, &package.dir, &mut report.ambiguities);
        let installed = package
            .has_dependencies
            .then(|| dependencies_installed(&package.dir, &manager.root));
        let mut project = json!({
            "kind": "node",
            "manifest": package.file,
            "workdir": package.rel_dir,
            "name": package.name,
            "package_manager": manager.name,
            "package_manager_source": manager.source,
            "package_manager_root": relative_display(ws.root(), &manager.root),
            "dependencies_installed": installed,
        });
        if let Some(version) = &package.node_version {
            project["node_version"] = json!(version);
        }
        report.projects.push(project);

        if installed == Some(false) && install_offered.insert(manager.root.clone()) {
            let root = relative_display(ws.root(), &manager.root);
            report.commands.push(Command::new(
                &[manager.name.as_str(), "install"],
                &root,
                "install",
                format!("{} ({})", manager.name, manager.source),
            ));
        }

        let names: BTreeSet<&str> = package
            .scripts
            .iter()
            .map(|(name, _)| name.as_str())
            .collect();
        for (name, body) in &package.scripts {
            // `pretest` / `posttest` 跟着 `test` 自动跑（npm 一定跑，pnpm 看配置），
            // 单独调没意义。
            let paired_hook = ["pre", "post"].iter().any(|prefix| {
                name.strip_prefix(prefix)
                    .is_some_and(|rest| names.contains(rest))
            });
            let role = if paired_hook {
                "hook"
            } else {
                role_for(name, body)
            };
            let mut command = Command::new(
                &[manager.name.as_str(), "run", name.as_str()],
                &package.rel_dir,
                role,
                format!("{} scripts.{name}", package.file),
            );
            command.declared = true;
            command.long_running = role == "dev_server" || body_watches(body);
            command.script = Some(script_preview(body));
            report.commands.push(command);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 字段和锁文件不一致时听字段的，但必须说出来：AI 照着跑了另一个包管理器，
    /// 会生成第二份锁文件。
    #[test]
    fn package_manager_field_wins_and_a_conflict_is_reported() {
        let dir = tempfile::tempdir().expect("dir");
        let mut ambiguities = Vec::new();
        let chosen = decide_package_manager(
            Some("pnpm@9.12.0".into()),
            &[("package-lock.json", "npm")],
            dir.path(),
            ".",
            &mut ambiguities,
        );
        assert_eq!(chosen.name, "pnpm");
        assert_eq!(ambiguities.len(), 1, "{ambiguities:?}");
        assert!(ambiguities[0].contains("package-lock.json"));
    }

    #[test]
    fn several_lockfiles_pick_one_and_say_so() {
        let dir = tempfile::tempdir().expect("dir");
        let mut ambiguities = Vec::new();
        let chosen = decide_package_manager(
            None,
            &[("pnpm-lock.yaml", "pnpm"), ("package-lock.json", "npm")],
            dir.path(),
            ".",
            &mut ambiguities,
        );
        assert_eq!(chosen.name, "pnpm");
        assert_eq!(chosen.source, "pnpm-lock.yaml");
        assert_eq!(ambiguities.len(), 1);
    }
}
