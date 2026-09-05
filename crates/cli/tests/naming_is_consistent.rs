//! 名字只有一个，而且到处都得对得上。
//!
//! 这个仓库从 `dtm` 改名成 `gld`，改名要动 90 个文件：crate 名、二进制名、
//! 环境变量、数据目录、上百条文档示例、shell 脚本、CI 工作流。漏一处的表现
//! 各不相同，共同点是**编译和普通测试全绿**：
//!
//! - 漏在 `scripts/gen-cli-docs.sh` → 脚本 `set -e` 直接退出，docs/cli.md
//!   停在旧版本，只有 CI 那道 `git diff --exit-code` 会发现（这事真发生过一次）；
//! - 漏在 `.github/workflows/release.yml` → 构建成功、`cp` 找不到文件，
//!   要等真发版那天才知道；
//! - 漏在文档里 → 读者照着敲得到 "unrecognized subcommand"。
//!
//! 所以在这里统一拦一道：源码树里不许再出现旧名字，而且几个"名字必须一致"
//! 的地方要真的一致。

mod common;

use std::path::{Path, PathBuf};

/// 改名前用的名字。留着是为了能拦住"从旧文档复制粘贴"这种回潮。
const OLD_NAMES: &[&str] = &["dtm", "DTM"];

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

/// 遍历仓库里该检查的文本文件。
///
/// 跳过三类：`target/`（构建产物）、`_original/`（桌面版参考源码，不属于本仓库、
/// 也不该被改名）、`.git/`。
fn source_files() -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if path.is_dir() {
                if matches!(
                    name.as_ref(),
                    "target" | "_original" | ".git" | "node_modules"
                ) {
                    continue;
                }
                walk(&path, out);
            } else if path.is_file() {
                out.push(path);
            }
        }
    }
    let mut files = Vec::new();
    walk(&repo_root(), &mut files);
    files.sort();
    files
}

#[test]
fn the_old_project_name_is_gone_everywhere() {
    let root = repo_root();
    let mut offenders = Vec::new();

    for path in source_files() {
        // 二进制文件（图标、fixture 里的图片等）读不成 UTF-8，直接跳过。
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let relative = path
            .strip_prefix(&root)
            .unwrap_or(&path)
            .display()
            .to_string();
        // 本文件是唯一有正当理由写旧名字的地方——它就是那个检查器。
        // 用 file!() 而不是写死路径：文件改名了这里跟着走，不会变成永远排除不到。
        if relative.replace('\\', "/") == file!().replace('\\', "/") {
            continue;
        }
        for (index, line) in text.lines().enumerate() {
            for old in OLD_NAMES {
                if line.contains(old) {
                    offenders.push(format!("{relative}:{} {}", index + 1, line.trim()));
                }
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "还有 {} 处旧名字没改（项目已改名为 gld）：\n{}",
        offenders.len(),
        offenders.join("\n")
    );
}

/// 二进制名、包名、打包脚本里写的那个名字，必须是同一个。
///
/// 三处各写各的，改了一处忘了另一处不会有任何编译错误：`cargo build` 照样过，
/// 打包那一步才会"构建产物不在 target/…/release/xxx"——而发版一次要跑五个目标、
/// 十几分钟，失败在最后一步最难受。
#[test]
fn the_packaging_script_names_the_binary_that_actually_gets_built() {
    let name = env!("CARGO_PKG_NAME");
    assert_eq!(
        name, "gld",
        "包名变了，这个测试和 scripts/package.sh 都要跟着改"
    );

    // CARGO_BIN_EXE_<bin> 是 cargo 在编译集成测试时注入的真实产物路径，
    // 拿它的文件名就是"实际构建出来的二进制叫什么"。
    let built = Path::new(env!("CARGO_BIN_EXE_gld"))
        .file_stem()
        .and_then(|stem| stem.to_str())
        .expect("binary name")
        .to_string();
    assert_eq!(built, name, "[[bin]] name 和包名对不上");

    let script =
        std::fs::read_to_string(repo_root().join("scripts/package.sh")).expect("读取 package.sh");
    assert!(
        script.contains(&format!("BIN=\"{name}\"")),
        "scripts/package.sh 里的 BIN 不是 {name}——打包会找不到构建产物"
    );
    assert!(
        script.contains(&format!("{name}-*")),
        "scripts/package.sh 生成校验和时匹配的前缀不是 {name}-*"
    );
}

/// 发版工作流必须真的去调那个打包脚本。
///
/// 把打包逻辑收进 scripts/package.sh 的意义就在于"只有一份"。哪天有人图省事
/// 在工作流里又内联写一遍 cargo build + tar，本地打出来的包和 Release 页上的包
/// 就会在文件名、目录结构、附带文件上慢慢分家——而两边都"看起来正常"，
/// 只有用户下载解压之后才发现对不上。
#[test]
fn the_release_workflow_delegates_to_the_packaging_script() {
    let workflow = std::fs::read_to_string(repo_root().join(".github/workflows/release.yml"))
        .expect("读取 release.yml");

    assert!(
        workflow.contains("scripts/package.sh \"${{ matrix.target }}\""),
        "release.yml 没有调用 scripts/package.sh 打包"
    );
    assert!(
        workflow.contains("scripts/package.sh --checksums"),
        "release.yml 没有用 scripts/package.sh 生成校验和"
    );
    // 生成校验和的那个 job 只收构件、不编译，很容易忘了 checkout，
    // 结果脚本根本不在盘上。它必须至少 checkout 一次。
    assert_eq!(
        workflow.matches("actions/checkout@").count(),
        3,
        "test / build / publish 三个 job 都要 checkout（publish 也要，它得有 scripts/）"
    );
    assert!(
        !workflow.contains("tar czf"),
        "release.yml 里又内联写打包了——打包逻辑只该有 scripts/package.sh 一份"
    );
}

/// 数据目录的环境变量和默认目录名要跟着项目名走。
///
/// 这两个是用户直接接触的（`GLD_HOME=... gld ...`、`~/.config/gld`），
/// 文档里到处在写。改名漏了这里，文档说的和程序读的就是两回事。
#[test]
fn the_data_home_env_and_directory_follow_the_project_name() {
    assert_eq!(gld_core::home::HOME_ENV, "GLD_HOME");
    assert_eq!(gld_core::home::DEFAULT_DIR_NAME, ".config/gld");
}
