//! `gld prune`：目录不在了的项目和已删项目留在数据目录里的东西，列清单、确认后删；留下的项目一样不动。

mod common;

use std::path::{Path, PathBuf};

use common::env::Env;
use sha2::{Digest, Sha256};

/// 任务记录、运行记录、委派记录的目录名：登记路径的 SHA-256 前 32 位。
fn path_hash(path: &str) -> String {
    format!("{:x}", Sha256::digest(path.as_bytes()))[..32].to_string()
}

fn touch(path: &Path) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, b"leftover").unwrap();
}

/// 服务里某个项目登记的路径和 id。
fn registered(env: &Env, name: &str) -> (String, String) {
    let state = env.json(&["--json", "ls"]);
    let member = state["service"]["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|member| member["name"] == name)
        .unwrap_or_else(|| panic!("{name} 不在 ls 里：{state}"))
        .clone();
    (
        member["path"].as_str().unwrap().to_string(),
        member["id"].as_str().unwrap().to_string(),
    )
}

#[test]
fn prune_lists_then_removes_only_what_no_remaining_project_owns() {
    let env = Env::new();
    let gone_dir = tempfile::tempdir().unwrap();
    env.ok(&["add", ".", "--name", "keep"]);
    env.ok(&[
        "add",
        &gone_dir.path().display().to_string(),
        "--name",
        "gone",
    ]);
    let (keep_path, keep_id) = registered(&env, "keep");
    let (gone_path, gone_id) = registered(&env, "gone");

    let home = env.home.path();
    let deleted_id = "0123456789abcdef0123456789abcdef";
    let leftovers: Vec<PathBuf> = vec![
        home.join("logs").join(&gone_id).join("stderr.log"),
        home.join("logs").join(deleted_id).join("stderr.log"),
        home.join(format!("data/oauth-clients/{deleted_id}-actions.json")),
        home.join("harness/workspaces")
            .join(path_hash(&gone_path))
            .join("state.json"),
    ];
    let kept: Vec<PathBuf> = vec![
        home.join("logs").join(&keep_id).join("stderr.log"),
        home.join("logs").join("hub").join("stderr.log"),
        home.join("harness/workspaces")
            .join(path_hash(&keep_path))
            .join("state.json"),
        home.join("data/oauth-clients/hub.json"),
    ];
    for path in leftovers.iter().chain(&kept) {
        touch(path);
    }
    drop(gone_dir);

    let listing = env.ok(&["ls"]);
    assert!(listing.contains("目录不在了"), "{listing}");
    assert!(listing.contains("gld prune"), "{listing}");

    let plan = env.json(&["--json", "prune", "--dry-run"]);
    assert_eq!(plan["missingProjects"][0]["name"], "gone", "{plan}");
    assert_eq!(
        plan["missingProjects"].as_array().unwrap().len(),
        1,
        "{plan}"
    );
    let listed: Vec<&str> = plan["leftovers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|leftover| leftover["path"].as_str().unwrap())
        .collect();
    for path in &leftovers {
        // 列出来的是顶层那一项（目录或文件），不是里面的每个文件。
        let top = if path.extension().is_some_and(|ext| ext == "json")
            && path.parent().unwrap().ends_with("oauth-clients")
        {
            path.clone()
        } else {
            path.parent().unwrap().to_path_buf()
        };
        assert!(
            listed.iter().any(|item| Path::new(item) == top),
            "{} 没列出来：{plan}",
            top.display()
        );
    }
    assert_eq!(listed.len(), leftovers.len(), "{plan}");

    // 非交互又没 -y：什么都不删。
    let refused = env.gld(&["prune"]);
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("-y"));
    assert!(leftovers.iter().all(|path| path.exists()));

    let done = env.json(&["--json", "prune", "-y"]);
    assert_eq!(done["applied"], true, "{done}");
    assert_eq!(done["failures"].as_array().unwrap().len(), 0, "{done}");
    for path in &leftovers {
        assert!(!path.exists(), "{} 还在", path.display());
    }
    for path in &kept {
        assert!(path.exists(), "{} 不该删", path.display());
    }
    let state = env.json(&["--json", "ls"]);
    let names: Vec<&str> = state["service"]["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|member| member["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["keep"], "{state}");

    let again = env.ok(&["prune", "--dry-run"]);
    assert!(again.contains("没有要清的"), "{again}");
}
