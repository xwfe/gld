//! `list_project_commands`：从清单里找出项目自己写好的命令（D10 任务发现）。

mod common;

use std::fs;
use std::path::Path;

use common::*;
use serde_json::{json, Value};

fn write(root: &Path, rel: &str, content: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    fs::write(path, content).expect("write");
}

/// 一个 Cargo workspace 加一个 pnpm 前端，外加两处不该被扫到的清单。
fn mixed_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("workspace");
    let root = dir.path();
    write(
        root,
        "Cargo.toml",
        "[workspace]\nmembers = [\"crates/*\"]\nresolver = \"2\"\n",
    );
    write(
        root,
        "crates/app/Cargo.toml",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    write(root, "crates/app/src/main.rs", "fn main() {}\n");
    write(
        root,
        "crates/lib/Cargo.toml",
        "[package]\nname = \"lib\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    write(root, "crates/lib/src/lib.rs", "");
    write(
        root,
        ".cargo/config.toml",
        "[alias]\nci = [\"test\", \"--locked\"]\nxtask = \"run --package xtask --\"\n",
    );
    write(
        root,
        "rust-toolchain.toml",
        "[toolchain]\nchannel = \"1.80.0\"\n",
    );
    write(
        root,
        "web/package.json",
        &json!({
            "name": "web",
            "packageManager": "pnpm@9.12.0",
            "scripts": {
                "dev": "vite",
                "build": "vite build",
                "test": "vitest run",
                "pretest": "node scripts/check.js",
                "deploy": "wrangler deploy"
            },
            "devDependencies": { "vite": "^7.0.0" }
        })
        .to_string(),
    );
    write(root, "web/pnpm-lock.yaml", "lockfileVersion: '9.0'\n");
    // 依赖目录和构建产物里的清单不是这个项目的入口。这个 node_modules 在 web 的包管理器
    // 根目录（web/）之外，pnpm 照样要在 web/ 里装，所以 web 仍算没装依赖。
    write(
        root,
        "node_modules/left-pad/package.json",
        r#"{"name":"left-pad","scripts":{"test":"x"}}"#,
    );
    write(
        root,
        "target/package/Cargo.toml",
        "[package]\nname = \"stale\"\n",
    );
    write(root, ".github/workflows/ci.yml", "on: push\n");
    write(root, "docs/development.md", "# dev\n");
    dir
}

fn command<'a>(payload: &'a Value, argv: &[&str]) -> &'a Value {
    payload["commands"]
        .as_array()
        .expect("commands")
        .iter()
        .find(|command| command["argv"] == json!(argv))
        .unwrap_or_else(|| panic!("no command {argv:?} in {:#}", payload["commands"]))
}

fn file_list(root: &Path) -> Vec<String> {
    let mut files: Vec<String> = walkdir::WalkDir::new(root)
        .into_iter()
        .filter_map(Result::ok)
        .map(|entry| entry.path().display().to_string())
        .collect();
    files.sort();
    files
}

#[test]
fn lists_cargo_and_package_json_entry_points() {
    let repo = mixed_repo();
    let ctx = ctx_for(repo.path());
    let out = invoke(&ctx, "list_project_commands", json!({}));
    let payload = assert_ok(&out);

    let projects = payload["projects"].as_array().expect("projects");
    assert_eq!(projects.len(), 2, "{projects:#?}");
    let cargo = &projects[0];
    assert_eq!(cargo["kind"], "cargo");
    assert_eq!(cargo["workdir"], ".");
    assert_eq!(cargo["members"], json!(["crates/app", "crates/lib"]));
    assert_eq!(cargo["toolchain"], "1.80.0 (rust-toolchain.toml)");
    let web = &projects[1];
    assert_eq!(web["kind"], "node");
    assert_eq!(web["workdir"], "web");
    assert_eq!(web["package_manager"], "pnpm");
    assert_eq!(web["dependencies_installed"], false);

    // 成员包不单独出 cargo build / test：在根上跑一遍就覆盖了。
    assert_eq!(command(payload, &["cargo", "build"])["workdir"], ".");
    assert_eq!(command(payload, &["cargo", "test"])["role"], "test");
    let run = command(payload, &["cargo", "run", "--bin", "app"]);
    assert_eq!(run["declared"], false);
    let alias = command(payload, &["cargo", "ci"]);
    assert_eq!(alias["declared"], true);
    assert_eq!(alias["script"], "test --locked");
    assert_eq!(alias["source"], ".cargo/config.toml alias.ci");

    let dev = command(payload, &["pnpm", "run", "dev"]);
    assert_eq!(dev["workdir"], "web");
    assert_eq!(dev["role"], "dev_server");
    assert_eq!(dev["long_running"], true);
    assert_eq!(dev["source"], "web/package.json scripts.dev");
    assert_eq!(command(payload, &["pnpm", "run", "test"])["role"], "test");
    assert_eq!(
        command(payload, &["pnpm", "run", "pretest"])["role"],
        "hook"
    );
    assert_eq!(
        command(payload, &["pnpm", "run", "deploy"])["role"],
        "deploy"
    );
    let install = command(payload, &["pnpm", "install"]);
    assert_eq!(install["workdir"], "web");
    assert_eq!(install["role"], "install");

    let all = payload["commands"].to_string();
    assert!(!all.contains("stale"), "target/ must not be scanned");
    assert!(
        payload["commands"]
            .as_array()
            .unwrap()
            .iter()
            .all(|command| command["source"]
                .as_str()
                .is_some_and(|source| !source.contains("node_modules"))),
        "node_modules must not be scanned"
    );
    assert_eq!(
        payload["other_sources"],
        json!([".github/workflows/ci.yml", "docs/development.md"])
    );
    let notes = payload["notes"].to_string();
    assert!(notes.contains("long_running"), "{notes}");
    assert!(notes.contains("install downloads"), "{notes}");
    assert!(notes.contains("deploy-like"), "{notes}");
    assert_eq!(payload["side_effects"], "none");
    assert_eq!(payload["truncated"], false);
}

/// 发现说能跑、真跑被拒，是这个工具最不能出的错：每条的 exec 必须和 check_command 对同一组
/// argv + workdir 的判定一字不差。
#[test]
fn exec_verdict_is_the_one_check_command_gives() {
    let repo = mixed_repo();
    for ctx in [
        ctx_for(repo.path()),
        ctx_with_allowed_commands(repo.path(), "only:cargo"),
    ] {
        let out = invoke(&ctx, "list_project_commands", json!({}));
        let payload = assert_ok(&out);
        for listed in payload["commands"].as_array().expect("commands") {
            let checked = invoke(
                &ctx,
                "check_command",
                json!({ "argv": listed["argv"], "workdir": listed["workdir"] }),
            );
            assert_eq!(listed["exec"]["decision"], checked["decision"], "{listed}");
            assert_eq!(listed["exec"]["rule"], checked["rule"], "{listed}");
            assert_eq!(
                listed["exec"]["program_found"], checked["program"]["found"],
                "{listed}"
            );
        }
    }

    let only_cargo = ctx_with_allowed_commands(repo.path(), "only:cargo");
    let out = invoke(&only_cargo, "list_project_commands", json!({}));
    let payload = assert_ok(&out);
    let dev = command(payload, &["pnpm", "run", "dev"]);
    assert_eq!(dev["exec"]["decision"], "deny");
    assert_eq!(dev["exec"]["rule"], "command_not_allowlisted");
    assert!(dev["exec"]["suggestion"].is_string());
    assert_ne!(
        command(payload, &["cargo", "test"])["exec"]["rule"],
        "command_not_allowlisted"
    );
}

/// 只读：不起进程、不装依赖。cargo 一跑就会生成 Cargo.lock 和 target/，pnpm 会生成
/// node_modules/，所以文件树一个字节都不能变。
#[test]
fn discovery_runs_nothing() {
    let repo = mixed_repo();
    let before = file_list(repo.path());
    let ctx = ctx_for(repo.path());
    assert_ok(&invoke(&ctx, "list_project_commands", json!({})));
    assert_eq!(file_list(repo.path()), before);
}

#[test]
fn path_narrows_the_scan_and_stays_inside_the_workspace() {
    let repo = mixed_repo();
    let ctx = ctx_for(repo.path());

    let out = invoke(&ctx, "list_project_commands", json!({ "path": "web" }));
    let payload = assert_ok(&out);
    assert_eq!(payload["path"], "web");
    assert_eq!(payload["projects"].as_array().unwrap().len(), 1);
    assert_eq!(payload["projects"][0]["kind"], "node");

    // 指到成员包里时那个包就是根：在它的目录里跑 cargo test 只测它自己。
    let out = invoke(
        &ctx,
        "list_project_commands",
        json!({ "path": "crates/app" }),
    );
    let payload = assert_ok(&out);
    assert_eq!(command(payload, &["cargo", "run"])["workdir"], "crates/app");

    assert_err(&invoke(
        &ctx,
        "list_project_commands",
        json!({ "path": "../" }),
    ));
    assert_err(&invoke(
        &ctx,
        "list_project_commands",
        json!({ "path": "Cargo.toml" }),
    ));
    assert_err(&invoke(
        &ctx,
        "list_project_commands",
        json!({ "max_depth": 3 }),
    ));
}

/// monorepo：成员包自己没有锁文件，要往上找到根；根上的锁文件冲突只报一次。
#[test]
fn monorepo_members_use_the_root_package_manager() {
    let dir = tempfile::tempdir().expect("workspace");
    let root = dir.path();
    write(
        root,
        "package.json",
        r#"{"private":true,"workspaces":["packages/*"],"scripts":{"build":"turbo build"}}"#,
    );
    write(root, "pnpm-lock.yaml", "");
    write(root, "package-lock.json", "{}");
    fs::create_dir_all(root.join("node_modules")).expect("node_modules");
    for name in ["a", "b"] {
        write(
            root,
            &format!("packages/{name}/package.json"),
            r#"{"scripts":{"test":"vitest run"},"dependencies":{"x":"1"}}"#,
        );
    }
    let ctx = ctx_for(root);
    let out = invoke(&ctx, "list_project_commands", json!({}));
    let payload = assert_ok(&out);

    let members: Vec<&Value> = payload["projects"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|project| project["workdir"] != ".")
        .collect();
    assert_eq!(members.len(), 2);
    for member in members {
        assert_eq!(member["package_manager"], "pnpm");
        assert_eq!(member["package_manager_root"], ".");
        // 依赖装在根上的 node_modules 里，Node 往上找得到。
        assert_eq!(member["dependencies_installed"], true);
    }
    assert_eq!(
        command(payload, &["pnpm", "run", "test"])["workdir"],
        "packages/a"
    );
    let ambiguities = payload["ambiguities"].as_array().expect("ambiguities");
    assert_eq!(ambiguities.len(), 1, "{ambiguities:#?}");
    assert!(ambiguities[0]
        .as_str()
        .unwrap()
        .contains("several lockfiles"));
    assert!(
        !payload["commands"].to_string().contains("\"install\""),
        "deps are installed, so no install command"
    );
}

/// 坏掉的清单报出来，别的照常列；不能因为一个文件整次失败，也不能悄悄跳过。
#[test]
fn a_broken_manifest_is_reported_not_fatal() {
    let dir = tempfile::tempdir().expect("workspace");
    let root = dir.path();
    write(root, "broken/package.json", "{ not json");
    write(root, "bad/Cargo.toml", "[package\nname = 1");
    write(
        root,
        "ok/package.json",
        r#"{"scripts":{"lint":"eslint ."}}"#,
    );
    let ctx = ctx_for(root);
    let out = invoke(&ctx, "list_project_commands", json!({}));
    let payload = assert_ok(&out);
    let problems = payload["problems"].to_string();
    assert!(problems.contains("broken/package.json"), "{problems}");
    assert!(problems.contains("bad/Cargo.toml"), "{problems}");
    assert_eq!(command(payload, &["npm", "run", "lint"])["role"], "lint");
    // 没锁文件就按 npm 猜，猜的要说出来。
    assert!(payload["ambiguities"].to_string().contains("assumed npm"));
}

/// 超了额度从深处截，说出来。按名字深度优先走到第几个就停的话，排在后面的浅层项目
/// （这里的 `z/`）会整个丢掉——真实的 monorepo 里丢的是整棵 `packages/`。
#[test]
fn too_many_manifests_drop_the_deepest_and_say_so() {
    let dir = tempfile::tempdir().expect("workspace");
    for n in 0..100 {
        write(
            dir.path(),
            &format!("a/pkg{n:03}/package.json"),
            r#"{"scripts":{"test":"node t.js"}}"#,
        );
    }
    write(
        dir.path(),
        "z/package.json",
        r#"{"scripts":{"build":"tsc"}}"#,
    );
    let ctx = ctx_for(dir.path());
    let out = invoke(&ctx, "list_project_commands", json!({}));
    let payload = assert_ok(&out);
    assert_eq!(payload["truncated"], true);
    assert!(payload["notes"].to_string().contains("deeper path"));
    let projects = payload["projects"].as_array().unwrap();
    assert_eq!(projects.len(), 100);
    assert!(projects.iter().any(|project| project["workdir"] == "z"));
    assert!(!projects
        .iter()
        .any(|project| project["workdir"] == "a/pkg099"));
}
