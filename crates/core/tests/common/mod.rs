#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};

use gld_core::tools::policy::{validate_tool_arguments, PolicySettings};
use gld_core::tools::{call_tool, ToolContext};
use serde_json::{json, Value};

pub struct FixtureWorkspace {
    pub root: PathBuf,
    pub outside_secret: PathBuf,
    _temp: tempfile::TempDir,
}

pub fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

pub fn tiny_js_fixture() -> FixtureWorkspace {
    prepare_fixture("tiny-js-project", false)
}

pub fn malicious_fixture() -> FixtureWorkspace {
    prepare_fixture("malicious-project", true)
}

/// A20 回放用的仓库：README、`.github/` 下的 workflow 和普通配置、一个会吐
/// 长日志的构建脚本，外加一个**假的 `gh`**（`tools/gh`，不联网）。
pub fn repo_maintenance_fixture() -> FixtureWorkspace {
    prepare_fixture("repo-maintenance", false)
}

fn prepare_fixture(name: &str, symlink_escape: bool) -> FixtureWorkspace {
    let temp = tempfile::tempdir().expect("tempdir");
    let parent = temp.path();
    let source = fixtures_root().join(name);
    assert!(source.is_dir(), "missing fixture: {}", source.display());
    let root = parent.join(name);
    copy_dir_all(&source, &root).expect("copy fixture");
    let outside_secret = parent.join("outside-secret.txt");
    fs::write(
        &outside_secret,
        fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/outside-secret.txt"),
        )
        .expect("outside-secret.txt"),
    )
    .expect("write outside secret");
    materialize_runtime_files(&root, &outside_secret, name);
    if symlink_escape {
        let link = root.join("outside-link.txt");
        let _ = fs::remove_file(&link);
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside_secret, &link).expect("symlink");
        #[cfg(windows)]
        {
            if std::os::windows::fs::symlink_file(&outside_secret, &link).is_err() {
                eprintln!("skip symlink setup on windows");
            }
        }
    }
    FixtureWorkspace {
        root,
        outside_secret,
        _temp: temp,
    }
}

fn materialize_runtime_files(root: &Path, outside_secret: &Path, name: &str) {
    let reference = root.join(".reference");
    let _ = fs::create_dir_all(&reference);
    let _ = fs::write(
        reference.join("cache.txt"),
        "reference cache must be excluded\n",
    );
    let _ = fs::create_dir_all(root.join("node_modules/leftpad"));
    let _ = fs::write(
        root.join("node_modules/leftpad/index.js"),
        "module.exports = 1;\n",
    );
    let _ = fs::create_dir_all(root.join("dist"));
    let _ = fs::write(
        root.join("dist/bundle.js"),
        "bundle output must be excluded\n",
    );
    let _ = fs::write(root.join("ignored.log"), "ignored by fixture gitignore\n");
    if name == "tiny-js-project" {
        let _ = fs::create_dir_all(root.join("assets"));
        let _ = fs::write(root.join("assets/raw.bin"), b"\x00\xff\x00binary\x00");
        let _ = fs::write(root.join("src/large.txt"), "0123456789abcdef\n".repeat(256));
        let _ = fs::create_dir_all(root.join("search"));
        for index in 0..12 {
            let _ = fs::write(
                root.join(format!("search/bulk_{index:02}.txt")),
                format!("common-token bulk line {index}\n"),
            );
        }
    }
    let _ = outside_secret;
}

fn copy_dir_all(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let target = dst.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_all(&entry.path(), &target)?;
        } else if file_type.is_symlink() {
            #[cfg(unix)]
            {
                let link = fs::read_link(entry.path())?;
                std::os::unix::fs::symlink(link, target)?;
            }
            #[cfg(windows)]
            {
                let link = fs::read_link(entry.path())?;
                if link.is_dir() {
                    std::os::windows::fs::symlink_dir(link, target)?;
                } else {
                    std::os::windows::fs::symlink_file(link, target)?;
                }
            }
        } else {
            fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

/// 把数据目录指到临时目录，别让测试写脏真实的 `~/.config/gld`。
///
/// 集成测试链接的是**非 test 构建**的 core，所以 `home.rs` 里那个 `cfg(test)`
/// 兜底在这儿不生效：`ToolContext::new` 会去 `~/.config/gld/harness` 建目录，
/// 于是 `cargo test` 会在用户主目录里留下一堆 workspace 状态目录（踩过一次）。
///
/// 已经设了 `GLD_HOME` 就不动它——CI 和冒烟脚本会自己指目录。
///
/// 这里不用 `tempfile::TempDir`，理由和 `core/src/home.rs` 里那份一样：它得存在
/// `static` 里（`GLD_HOME` 是进程级的），而 Rust 不跑 `static` 的析构函数，
/// 目录就永远留在 `$TMPDIR` 下。按测试二进制取固定名字、每次先清上一轮的，
/// 残留才不会一轮一轮堆上去。
pub fn isolate_data_home() {
    use std::sync::OnceLock;
    static HOME: OnceLock<PathBuf> = OnceLock::new();
    let dir = HOME.get_or_init(|| {
        let name = std::env::current_exe()
            .ok()
            .and_then(|path| {
                path.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
            })
            .map(|name| match name.rsplit_once('-') {
                Some((head, _hash)) if !head.is_empty() => head.to_string(),
                _ => name,
            })
            .unwrap_or_else(|| "unknown".into());
        let dir = std::env::temp_dir().join("gld-test-homes").join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp GLD_HOME");
        dir
    });
    if std::env::var_os("GLD_HOME").is_none() {
        std::env::set_var("GLD_HOME", dir);
    }
}

pub fn ctx_for(root: &Path) -> ToolContext {
    isolate_data_home();
    ToolContext::new(root.to_path_buf()).expect("tool context")
}

/// `permission-mode=dangerous` 下的上下文。
pub fn ctx_for_dangerous_mode(root: &Path) -> ToolContext {
    isolate_data_home();
    let workspace = gld_core::tools::Workspace::new(root.to_path_buf()).expect("workspace");
    ToolContext::from_workspace(
        workspace,
        gld_core::workspace::AuthConfig {
            auth_type: "noauth".into(),
            ..gld_core::workspace::AuthConfig::default()
        },
        PolicySettings {
            permission_mode: "dangerous".into(),
            ..PolicySettings::default()
        },
        "full".into(),
        "dangerous".into(),
    )
}

/// 指定命令白名单配置的上下文。`configured` 就是 `mcp.allowed-commands`
/// 的原文，`only:` 前缀等语义由生产代码自己解析，测试不另写一份。
pub fn ctx_with_allowed_commands(root: &Path, configured: &str) -> ToolContext {
    isolate_data_home();
    let workspace = gld_core::tools::Workspace::new(root.to_path_buf()).expect("workspace");
    let actions = gld_core::workspace::ActionsConfig {
        allowed_commands: configured.into(),
        ..gld_core::workspace::ActionsConfig::default()
    };
    ToolContext::from_workspace(
        workspace,
        gld_core::workspace::AuthConfig {
            auth_type: "noauth".into(),
            ..gld_core::workspace::AuthConfig::default()
        },
        PolicySettings::from_actions_config(&actions),
        "full".into(),
        "trusted".into(),
    )
}

/// 白名单之外再把一个目录放到 PATH 前面。
///
/// A20 要"查看 CI 状态"，而这套测试没有 GitHub 授权、也不该联网。于是把假的
/// `gh` 放进工作区的 `tools/`，用 `executable_paths` 让它排在系统 PATH 前面：
/// 这样**不用改进程自己的环境变量**（`set_var` 是全进程的，会波及并行跑的
/// 别的测试），本机装没装真 `gh` 也不影响结论。
pub fn ctx_with_allowed_commands_and_path(
    root: &Path,
    configured: &str,
    extra_path: &Path,
) -> ToolContext {
    let mut ctx = ctx_with_allowed_commands(root, configured);
    ctx.executable_paths = vec![extra_path.to_path_buf()];
    ctx
}

/// 直接指定一份 `PolicySettings` 的上下文。
///
/// 用来做"只差一个字段"的对照：别的都一样，才能说明观察到的差异是那个字段
/// 造成的。
pub fn ctx_with_policy(root: &Path, policy: PolicySettings) -> ToolContext {
    isolate_data_home();
    let workspace = gld_core::tools::Workspace::new(root.to_path_buf()).expect("workspace");
    ToolContext::from_workspace(
        workspace,
        gld_core::workspace::AuthConfig {
            auth_type: "noauth".into(),
            ..gld_core::workspace::AuthConfig::default()
        },
        policy,
        "full".into(),
        "trusted".into(),
    )
}

/// 关掉"读只许在 Workspace 内"的上下文。
///
/// 默认是开着的（0.3.0 起）。专门测越界读行为的用例得显式关掉，
/// 否则它们测到的只是外层那道门，而不是自己想验的东西。
pub fn ctx_for_unconfined_reads(root: &Path) -> ToolContext {
    isolate_data_home();
    let workspace = gld_core::tools::Workspace::new(root.to_path_buf()).expect("workspace");
    ToolContext::from_workspace(
        workspace,
        gld_core::workspace::AuthConfig {
            auth_type: "noauth".into(),
            ..gld_core::workspace::AuthConfig::default()
        },
        PolicySettings {
            confine_reads: false,
            ..PolicySettings::default()
        },
        "full".into(),
        "trusted".into(),
    )
}

pub fn invoke(ctx: &ToolContext, name: &str, args: Value) -> Value {
    call_tool(ctx, name, &args)
}

pub fn assert_ok(result: &Value) -> &Value {
    assert_eq!(result.get("ok"), Some(&json!(true)), "{result}");
    result
}

pub fn assert_err(result: &Value) -> &Value {
    assert_eq!(result.get("ok"), Some(&json!(false)), "{result}");
    let err = result.get("error").expect("error object");
    assert!(err.get("code").and_then(Value::as_str).is_some());
    assert!(err.get("message").and_then(Value::as_str).is_some());
    assert!(err.get("category").and_then(Value::as_str).is_some());
    assert!(err.get("retryable").map(Value::is_boolean).unwrap_or(false));
    assert!(err.get("details").map(Value::is_object).unwrap_or(false));
    result
}

pub fn assert_security_or_policy_err(result: &Value) {
    let err = assert_err(result);
    let cat = err["error"]["category"].as_str().unwrap_or("");
    assert!(
        matches!(cat, "security" | "policy" | "permission" | "validation"),
        "unexpected category: {cat}"
    );
}

pub fn assert_policy_rejects(tool: &str, args: Value) {
    let policy = PolicySettings::default();
    assert!(validate_tool_arguments(tool, &args, &policy).is_err());
}
