//! `gld prune`：一次清掉两类东西——登记了、目录却已经不在的项目，和已删项目留在数据目录里的记录。
//!
//! `gld rm` 故意留着任务记录和运行记录（同一个目录再加回来还接得上，见 concepts.md「停和删是两回事」），
//! 日志也不删。项目搬了目录、或者测试项目删过一批之后，这些就只剩占地方，以前只能照文档算哈希手工删。
//!
//! 数据目录里按项目分的几样，各自用什么当目录名：
//!
//! | 位置 | 名字 |
//! | --- | --- |
//! | `logs/<名字>/` | 项目 id |
//! | `data/oauth-clients/<名字>.json` | 项目 id，或 `<id>-actions` |
//! | `harness/workspaces/<名字>/`、`runs/<名字>/`、`agents/<名字>/` | 项目绝对路径的 SHA-256 前 32 位 |
//!
//! 对不上任何一个留下的项目，就算残留。`write-locks/` 不管：每个目录一个 0 字节的锁文件。

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::App;
use crate::error::AppResult;
use crate::workspace::WorkspaceProfile;

/// `gld prune` 看到的（只列清单时）或者做了的（真删时）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PruneReport {
    /// 登记了、目录却已经不在的项目。
    pub missing_projects: Vec<MissingProject>,
    /// 对不上任何留下的项目的数据：已删项目的，和上面那些项目的。
    pub leftovers: Vec<Leftover>,
    /// 也对不上，但里面还有命令或 Agent 在跑，这次不动。
    #[serde(default)]
    pub busy: Vec<Leftover>,
    /// 真删了是 true；只列清单是 false。
    #[serde(default)]
    pub applied: bool,
    /// 删的时候出了错的。
    #[serde(default)]
    pub failures: Vec<PruneFailure>,
}

impl PruneReport {
    pub fn is_empty(&self) -> bool {
        self.missing_projects.is_empty()
            && self.leftovers.is_empty()
            && self.busy.is_empty()
            && self.failures.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MissingProject {
    pub id: String,
    pub name: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Leftover {
    /// logs | oauth | harness | runs | agents
    pub kind: String,
    pub path: String,
    pub bytes: u64,
    /// 属于上面哪个目录不在了的项目（它的 id）；已经删掉的项目留下的是空串。
    #[serde(default)]
    pub project: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PruneFailure {
    /// 项目 id 或路径。
    pub target: String,
    pub error: String,
}

impl App {
    /// `confirmed` 是 `None` 时只列清单，什么都不动。
    ///
    /// 给了就删，但只删**这次重新扫出来、并且在 `confirmed` 里**的：用户看清单、确认、再发回来，
    /// 中间可能有目录被加了回来、有命令刚起——那些这次扫不出来，自然不删；用户没看到过的新残留也不删。
    pub async fn prune(&self, confirmed: Option<PruneReport>) -> AppResult<PruneReport> {
        let home = crate::home::data_home()?;
        let profiles = self.list_workspaces()?;
        let scan_home = home.clone();
        let mut plan = tokio::task::spawn_blocking(move || scan(&scan_home, &profiles))
            .await
            .map_err(|error| crate::error::AppError::Message(error.to_string()))?;
        if !plan.failures.is_empty() {
            return Ok(plan);
        }
        let Some(confirmed) = confirmed else {
            return Ok(plan);
        };
        retain_confirmed(&mut plan, confirmed);

        let mut failures = Vec::new();
        let mut kept_projects = HashSet::new();
        for project in &plan.missing_projects {
            if let Err(error) = self.delete_workspace(&project.id).await {
                kept_projects.insert(project.id.clone());
                failures.push(PruneFailure {
                    target: project.id.clone(),
                    error: error.to_string(),
                });
            }
        }
        // 项目没删掉的话它还登记着，它的记录也不能动。
        plan.missing_projects
            .retain(|project| !kept_projects.contains(&project.id));
        plan.leftovers
            .retain(|leftover| !kept_projects.contains(&leftover.project));

        let leftovers = std::mem::take(&mut plan.leftovers);
        let (removed, mut errors) =
            tokio::task::spawn_blocking(move || remove_leftovers(&home, leftovers))
                .await
                .map_err(|error| crate::error::AppError::Message(error.to_string()))?;
        failures.append(&mut errors);
        plan.leftovers = removed;
        plan.applied = true;
        plan.failures = failures;
        Ok(plan)
    }
}

/// 这次扫出来的里面，只留用户确认过的。
fn retain_confirmed(plan: &mut PruneReport, confirmed: PruneReport) {
    let projects: HashSet<String> = confirmed
        .missing_projects
        .into_iter()
        .map(|project| project.id)
        .collect();
    let paths: HashSet<String> = confirmed
        .leftovers
        .into_iter()
        .map(|leftover| leftover.path)
        .collect();
    plan.missing_projects
        .retain(|project| projects.contains(&project.id));
    // 属于某个目录不在了的项目的记录，只在这个项目也一起删时才删：项目还登记着，它的记录就还有主。
    let removed_projects: HashSet<String> = plan
        .missing_projects
        .iter()
        .map(|project| project.id.clone())
        .collect();
    plan.leftovers.retain(|leftover| {
        paths.contains(&leftover.path)
            && (leftover.project.is_empty() || removed_projects.contains(&leftover.project))
    });
}

/// 找出目录不在的项目和对不上的数据，不动任何东西。
fn scan(home: &Path, profiles: &[WorkspaceProfile]) -> PruneReport {
    let managed = match crate::managed_workspaces::Store::new(home.into()).all() {
        Ok(records) => records,
        Err(error) => {
            return PruneReport {
                failures: vec![PruneFailure {
                    target: "managed-workspaces".into(),
                    error: error.message(),
                }],
                ..PruneReport::default()
            }
        }
    };
    let (gone, kept): (Vec<&WorkspaceProfile>, Vec<&WorkspaceProfile>) = profiles
        .iter()
        .partition(|profile| directory_is_gone(Path::new(&profile.path)));
    let kept_ids: HashSet<&str> = kept
        .iter()
        .map(|profile| profile.id.as_str())
        .chain(managed.iter().map(|record| record.id.as_str()))
        .collect();
    let mut kept_hashes: HashSet<String> = kept
        .iter()
        .flat_map(|profile| path_hashes(&profile.path))
        .collect();
    kept_hashes.extend(
        managed
            .iter()
            .flat_map(|record| path_hashes(&record.root.to_string_lossy())),
    );
    // 残留属于哪个目录不在了的项目：按 id 认日志和 OAuth 注册表，按路径哈希认另外三样。
    let owner_of = |name: &str| -> String {
        let id = name.strip_suffix("-actions").unwrap_or(name);
        gone.iter()
            .find(|profile| {
                profile.id == id || path_hashes(&profile.path).iter().any(|h| h == name)
            })
            .map(|profile| profile.id.clone())
            .unwrap_or_default()
    };

    let mut report = PruneReport {
        missing_projects: gone
            .iter()
            .map(|profile| MissingProject {
                id: profile.id.clone(),
                name: profile.name.clone(),
                path: profile.path.clone(),
            })
            .collect(),
        ..PruneReport::default()
    };
    let mut push = |kind: &str, path: PathBuf, name: &str, busy: bool| {
        let leftover = Leftover {
            kind: kind.into(),
            bytes: size_of(&path),
            path: path.to_string_lossy().into_owned(),
            project: owner_of(name),
        };
        if busy {
            report.busy.push(leftover);
        } else {
            report.leftovers.push(leftover);
        }
    };

    for (name, path) in entries(&home.join("logs"), true) {
        if is_hex_id(&name) && !kept_ids.contains(name.as_str()) {
            push("logs", path, &name, false);
        }
    }
    for (name, path) in entries(&home.join("data").join("oauth-clients"), false) {
        let Some(scope) = name.strip_suffix(".json") else {
            continue;
        };
        let id = scope.strip_suffix("-actions").unwrap_or(scope);
        if is_hex_id(id) && !kept_ids.contains(id) {
            push("oauth", path, scope, false);
        }
    }
    for (name, path) in entries(&home.join("harness").join("workspaces"), true) {
        if is_hex_id(&name) && !kept_hashes.contains(&name) {
            push("harness", path, &name, false);
        }
    }
    let runs_root = home.join("runs");
    for (name, path) in entries(&runs_root, true) {
        if is_hex_id(&name) && !kept_hashes.contains(&name) {
            let busy = crate::tools::runs::project_has_running(&runs_root, &path);
            push("runs", path, &name, busy);
        }
    }
    for (name, path) in entries(&crate::agents::store::root(home), true) {
        if is_hex_id(&name) && !kept_hashes.contains(&name) {
            let busy = has_unfinished_agent_task(&path);
            push("agents", path, &name, busy);
        }
    }
    report
}

/// 只有"确实不存在"（或者成了一个文件）才算目录不在了。读不了（权限不够之类）不算：
/// 宁可这次不清，也别把一个只是暂时看不见的项目删掉。外接盘没插上时挂载点不存在，会算进来，
/// 所以命令行删之前列清单、要用户确认。
pub fn directory_is_gone(path: &Path) -> bool {
    match fs::metadata(path) {
        Ok(meta) => !meta.is_dir(),
        Err(error) => error.kind() == std::io::ErrorKind::NotFound,
    }
}

/// 运行记录、任务记录用的是登记时的路径，委派记录先 canonicalize 再算（`agents::start`），
/// 两种都算上，哪种都不会被当成残留。
fn path_hashes(path: &str) -> Vec<String> {
    let raw = Path::new(path);
    let mut hashes = vec![crate::tools::runs::project_id(raw)];
    if let Ok(real) = raw.canonicalize() {
        let hash = crate::tools::runs::project_id(&real);
        if !hashes.contains(&hash) {
            hashes.push(hash);
        }
    }
    hashes
}

/// 项目 id 和路径哈希都是 32 位小写十六进制。别的名字（`hub`、`owners`、`daemon.log`……）不碰。
fn is_hex_id(name: &str) -> bool {
    name.len() == 32
        && name
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
}

/// 一个目录下的条目：`dirs` 为真只要子目录，否则只要文件。目录不在就是空的。
fn entries(dir: &Path, dirs: bool) -> Vec<(String, PathBuf)> {
    let Ok(read) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<(String, PathBuf)> = read
        .flatten()
        .filter(|entry| {
            entry
                .file_type()
                .is_ok_and(|kind| if dirs { kind.is_dir() } else { kind.is_file() })
        })
        .filter_map(|entry| Some((entry.file_name().to_str()?.to_string(), entry.path())))
        .collect();
    found.sort();
    found
}

fn has_unfinished_agent_task(project_dir: &Path) -> bool {
    entries(project_dir, true).iter().any(|(_, task_dir)| {
        crate::agents::store::read(task_dir).is_some_and(|task| !task.state.is_finished())
    })
}

/// 文件或目录一共多大。符号链接不跟进去，只算它自己。
fn size_of(path: &Path) -> u64 {
    walkdir::WalkDir::new(path)
        .follow_links(false)
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.metadata().ok())
        .filter(|meta| meta.is_file())
        .map(|meta| meta.len())
        .sum()
}

/// 删掉这些残留，返回删成了的和出错的。路径是这次扫出来的；再核一遍在数据目录里，
/// 哪天拼路径出了错也删不到外面去。
fn remove_leftovers(home: &Path, leftovers: Vec<Leftover>) -> (Vec<Leftover>, Vec<PruneFailure>) {
    let mut removed = Vec::new();
    let mut failures = Vec::new();
    // 确认清单之后可能已经建了通用空间；删除前再认一次登记。损坏时整体不删。
    let managed = match crate::managed_workspaces::Store::new(home.into()).all() {
        Ok(records) => records,
        Err(error) => {
            return (
                removed,
                vec![PruneFailure {
                    target: "managed-workspaces".into(),
                    error: error.message(),
                }],
            )
        }
    };
    let kept: HashSet<String> = managed
        .iter()
        .flat_map(|record| {
            let mut keys = path_hashes(&record.root.to_string_lossy());
            keys.push(record.id.clone());
            keys
        })
        .collect();
    for leftover in leftovers {
        let path = PathBuf::from(&leftover.path);
        if path
            .file_name()
            .and_then(|s| s.to_str())
            .is_some_and(|name| {
                kept.contains(name)
                    || kept.contains(name.trim_end_matches(".json").trim_end_matches("-actions"))
            })
        {
            continue;
        }
        if !path.starts_with(home) {
            failures.push(PruneFailure {
                target: leftover.path.clone(),
                error: "不在数据目录里，没删".into(),
            });
            continue;
        }
        let result = if path.is_dir() {
            fs::remove_dir_all(&path)
        } else {
            fs::remove_file(&path)
        };
        match result {
            Ok(()) => removed.push(leftover),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => removed.push(leftover),
            Err(error) => failures.push(PruneFailure {
                target: leftover.path.clone(),
                error: error.to_string(),
            }),
        }
    }
    (removed, failures)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(id: &str, path: &Path) -> WorkspaceProfile {
        let mut profile = WorkspaceProfile::new(path.display().to_string(), Some(id.into()));
        profile.id = format!("{id:0>32}");
        profile
    }

    fn touch(path: &Path) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"x").unwrap();
    }

    #[test]
    fn managed_records_are_kept_even_without_projects_or_a_present_work_directory() {
        let home = tempfile::tempdir().unwrap();
        let store = crate::managed_workspaces::Store::new(home.path().into());
        let (record, _) = store.start("owner", "one", "").unwrap();
        let hash = crate::tools::runs::project_id(&record.root);
        let mut pretend_confirmed = Vec::new();
        for (kind, base) in [
            ("harness", "harness/workspaces"),
            ("runs", "runs"),
            ("agents", "agents"),
        ] {
            let path = home.path().join(base).join(&hash);
            touch(&path.join("evidence.txt"));
            pretend_confirmed.push(Leftover {
                kind: kind.into(),
                path: path.to_string_lossy().into_owned(),
                bytes: 1,
                project: String::new(),
            });
        }
        fs::remove_dir(&record.root).unwrap();
        let plan = scan(home.path(), &[]);
        assert!(plan.leftovers.is_empty(), "{plan:?}");
        assert!(plan.failures.is_empty());
        let (removed, failures) = remove_leftovers(home.path(), pretend_confirmed);
        assert!(removed.is_empty());
        assert!(failures.is_empty());
        assert!(home
            .path()
            .join("runs")
            .join(&hash)
            .join("evidence.txt")
            .exists());
    }

    #[test]
    fn invalid_managed_registry_blocks_pruning_instead_of_dropping_ownership() {
        let home = tempfile::tempdir().unwrap();
        touch(&home.path().join("managed-workspaces/broken.json"));
        let orphan = home.path().join("runs").join("b".repeat(32));
        touch(&orphan.join("evidence.txt"));
        let plan = scan(home.path(), &[]);
        assert!(!plan.is_empty());
        assert_eq!(plan.failures.len(), 1);
        assert!(plan.leftovers.is_empty());
        let (removed, failures) = remove_leftovers(
            home.path(),
            vec![Leftover {
                kind: "runs".into(),
                path: orphan.to_string_lossy().into_owned(),
                bytes: 1,
                project: String::new(),
            }],
        );
        assert!(removed.is_empty());
        assert_eq!(failures.len(), 1);
        assert!(orphan.exists());
    }

    /// 留下的项目的东西一样不动；删掉的、目录不在了的项目的都列出来，并且记得属于谁。
    #[test]
    fn only_data_of_projects_that_stay_is_kept() {
        let home = tempfile::tempdir().unwrap();
        let alive_dir = tempfile::tempdir().unwrap();
        let home = home.path();
        let alive = profile("a", alive_dir.path());
        let gone = profile("b", &home.join("no-such-dir"));
        let deleted_id = format!("{:0>32}", "c");
        let alive_hash = crate::tools::runs::project_id(alive_dir.path());
        let gone_hash = crate::tools::runs::project_id(Path::new(&gone.path));
        let deleted_hash = format!("{:0>32}", "d");

        for name in [&alive.id, &gone.id, &deleted_id] {
            touch(&home.join("logs").join(name).join("stderr.log"));
        }
        touch(&home.join("logs").join("hub").join("stderr.log"));
        touch(&home.join("logs").join("daemon.log"));
        touch(&home.join("data/oauth-clients/hub.json"));
        touch(&home.join(format!("data/oauth-clients/{}.json", alive.id)));
        touch(&home.join(format!("data/oauth-clients/{deleted_id}-actions.json")));
        for hash in [&alive_hash, &gone_hash, &deleted_hash] {
            touch(
                &home
                    .join("harness/workspaces")
                    .join(hash)
                    .join("state.json"),
            );
            touch(&home.join("agents").join(hash).join("x").join("task.json"));
        }
        touch(&home.join("runs/owners/x.lock"));
        touch(&home.join("agents/owners/x.lock"));

        let report = scan(home, &[alive.clone(), gone.clone()]);
        assert_eq!(
            report.missing_projects,
            vec![MissingProject {
                id: gone.id.clone(),
                name: gone.name.clone(),
                path: gone.path.clone()
            }]
        );
        let found: HashSet<(String, String)> = report
            .leftovers
            .iter()
            .map(|leftover| {
                let rel = Path::new(&leftover.path)
                    .strip_prefix(home)
                    .unwrap()
                    .display()
                    .to_string();
                (rel, leftover.project.clone())
            })
            .collect();
        let expected: HashSet<(String, String)> = [
            (format!("logs/{}", gone.id), gone.id.clone()),
            (format!("logs/{deleted_id}"), String::new()),
            (
                format!("data/oauth-clients/{deleted_id}-actions.json"),
                String::new(),
            ),
            (format!("harness/workspaces/{gone_hash}"), gone.id.clone()),
            (format!("harness/workspaces/{deleted_hash}"), String::new()),
            (format!("agents/{gone_hash}"), gone.id.clone()),
            (format!("agents/{deleted_hash}"), String::new()),
        ]
        .into_iter()
        .collect();
        assert_eq!(found, expected);
        assert!(report.leftovers.iter().all(|leftover| leftover.bytes == 1));
    }

    #[test]
    fn only_a_directory_that_is_really_not_there_counts_as_gone() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!directory_is_gone(dir.path()));
        assert!(directory_is_gone(&dir.path().join("moved-away")));
        // 是文件不是目录：登记的项目目录被换成了同名文件，也算不在了。
        let file = dir.path().join("file");
        fs::write(&file, b"").unwrap();
        assert!(directory_is_gone(&file));
    }

    /// 父目录没有权限时 metadata 报的是 PermissionDenied，不是 NotFound：这种不能删。
    #[cfg(unix)]
    #[test]
    fn a_project_behind_a_locked_parent_is_not_counted_as_gone() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().join("locked");
        let project = parent.join("project");
        fs::create_dir_all(&project).unwrap();
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o000)).unwrap();
        let readable_anyway = fs::metadata(&project).is_ok();
        let gone = directory_is_gone(&project);
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o755)).unwrap();
        // root 不受权限位限制，读得到就没什么可测的。
        if !readable_anyway {
            assert!(!gone);
        }
    }

    /// 看清单和按回车之间新冒出来的（用户没看到过的），以及已经不该删的（目录加回来了），都不删。
    #[test]
    fn only_what_was_confirmed_and_is_still_a_leftover_gets_deleted() {
        let leftover = |path: &str| Leftover {
            kind: "logs".into(),
            path: path.into(),
            bytes: 1,
            project: String::new(),
        };
        let project = |id: &str| MissingProject {
            id: id.into(),
            name: id.into(),
            path: format!("/gone/{id}"),
        };
        let owned_by = |path: &str, id: &str| Leftover {
            project: id.into(),
            ..leftover(path)
        };
        let confirmed = PruneReport {
            missing_projects: vec![project("p1"), project("p2")],
            leftovers: vec![
                leftover("/h/logs/a"),
                leftover("/h/logs/b"),
                owned_by("/h/logs/p1", "p1"),
                owned_by("/h/logs/p4", "p4"),
            ],
            ..PruneReport::default()
        };
        // 现在扫：p2 的目录回来了、b 被谁删了，又冒出来一个 c；p4 没被确认删，它的日志也不能删。
        let mut now = PruneReport {
            missing_projects: vec![project("p1"), project("p3"), project("p4")],
            leftovers: vec![
                leftover("/h/logs/a"),
                leftover("/h/logs/c"),
                owned_by("/h/logs/p1", "p1"),
                owned_by("/h/logs/p4", "p4"),
            ],
            ..PruneReport::default()
        };
        retain_confirmed(&mut now, confirmed);
        assert_eq!(now.missing_projects, vec![project("p1")]);
        assert_eq!(
            now.leftovers,
            vec![leftover("/h/logs/a"), owned_by("/h/logs/p1", "p1")]
        );
    }

    #[test]
    fn removal_stays_inside_the_data_directory() {
        let home = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let inside = home.path().join("logs").join(format!("{:0>32}", "e"));
        touch(&inside.join("stderr.log"));
        let leftover = |path: &Path| Leftover {
            kind: "logs".into(),
            path: path.display().to_string(),
            bytes: 1,
            project: String::new(),
        };
        let (removed, failures) = remove_leftovers(
            home.path(),
            vec![leftover(&inside), leftover(outside.path())],
        );
        assert_eq!(removed.len(), 1);
        assert!(!inside.exists());
        assert_eq!(failures.len(), 1);
        assert!(outside.path().exists());
    }
}
