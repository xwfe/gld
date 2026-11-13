//! Cargo 层守住已经拆出的信任边界，包含可选、目标平台和构建依赖。
//!
//! 不靠源码字符串判断 import；Cargo 会展开 workspace 继承和 package 别名。
//! 这是代码依赖约束，不是子进程或操作系统沙箱。

use std::path::Path;
use std::process::Command;

use serde_json::Value;

#[test]
fn extracted_crates_keep_their_dependency_boundaries() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = Command::new(env!("CARGO"))
        .current_dir(root)
        .args([
            "metadata",
            "--format-version",
            "1",
            "--no-deps",
            "--offline",
            "--locked",
        ])
        .output()
        .expect("读取 Cargo 依赖");
    assert!(
        output.status.success(),
        "cargo metadata 失败：{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: Value = serde_json::from_slice(&output.stdout).expect("Cargo metadata JSON");
    let packages = metadata["packages"].as_array().expect("packages");

    for (name, normal, development) in [
        ("gld-policy", &["serde"][..], &["serde_json"][..]),
        ("gld-protocol", &["serde", "serde_json"][..], &[][..]),
        ("gld-transport", &["serde_json"][..], &[][..]),
        (
            "gld-runtime",
            &[
                "gld-policy",
                "gld-protocol",
                "serde",
                "serde_json",
                "thiserror",
                "uuid",
                "toexec-fs",
                "toexec-text",
                "walkdir",
                "regex",
                "glob",
                "image",
                "base64",
                "tokio",
                "fs2",
                "which",
                "shell-words",
                "libc",
                "windows",
            ][..],
            &["tempfile"][..],
        ),
    ] {
        let package = packages
            .iter()
            .find(|package| package["name"] == name)
            .unwrap_or_else(|| panic!("缺少边界 crate {name}"));
        let dependencies = package["dependencies"].as_array().expect("dependencies");
        for dependency in dependencies {
            let dependency_name = dependency["name"].as_str().expect("dependency name");
            let allowed = match dependency["kind"].as_str() {
                None => normal,
                Some("dev") => development,
                // 拆出的 crate 不执行 build.rs，也不通过构建依赖绕过边界。
                _ => &[],
            };
            assert!(
                allowed.contains(&dependency_name),
                "{name} 的 {:?} 依赖 {dependency_name} 越过了边界（target: {}）",
                dependency["kind"],
                dependency["target"]
            );
        }
        assert!(
            !package["targets"]
                .as_array()
                .expect("targets")
                .iter()
                .any(|target| target["kind"]
                    .as_array()
                    .expect("target kind")
                    .iter()
                    .any(|kind| kind == "custom-build")),
            "{name} 不应通过构建脚本引入隐式副作用"
        );
    }
}
