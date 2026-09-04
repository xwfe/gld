//! 源码里写给用户看的 `gld ...` 必须是真命令。
//!
//! 已经有两个同类测试：docs 里的示例、doctor 给的修复命令。差的就是第三处——
//! 运行时的报错和提示，而这恰恰是最多人照着做的地方：人是撞了墙才去看提示的。
//!
//! 这里连注释一起扫。注释里写错命令虽然不会坑到用户，但会坑到下一个照着注释
//! 改代码的人，而且加上注释也不用多写一行代码。

mod common;

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("repo root")
        .to_path_buf()
}

/// 收集 crates 下的全部 .rs。跳过 tests：测试里会故意写不存在的命令来验报错。
fn sources(dir: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|name| name == "tests") {
                continue;
            }
            sources(&path, found);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            found.push(path);
        }
    }
}

#[test]
fn every_gld_command_in_the_source_is_a_real_command() {
    let root = repo_root();
    let mut files = Vec::new();
    sources(&root.join("crates"), &mut files);
    assert!(files.len() > 50, "源码文件没扫到几个，路径大概错了");

    let mut checked = 0;
    for file in files {
        let text = std::fs::read_to_string(&file).expect("read source");
        let name = file
            .strip_prefix(&root)
            .unwrap_or(&file)
            .display()
            .to_string();
        for command in common::docs::extract_anywhere(&text, &name) {
            common::docs::assert_valid(&command);
            checked += 1;
        }
    }
    assert!(checked > 20, "只校验到 {checked} 条命令，提取器大概没工作");
}
