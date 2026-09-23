//! 数据目录解析。
//!
//! 所有持久化内容（工作区配置、密钥、日志、守护进程 socket 与 pid 文件）
//! 都放在同一个目录下：
//!
//! 1. 环境变量 `GLD_HOME` 指定的目录（测试和多实例隔离用）；
//! 2. 否则 `~/.config/gld`。
//!
//! 目录结构：
//!
//! ```text
//! ~/.config/gld/
//! ├── data/profiles.json   工作区、设置、密钥（单一事实来源）
//! ├── logs/<workspace-id>/ 每个工作区的 MCP / Actions / 隧道日志
//! ├── logs/daemon.log      守护进程自身日志
//! ├── frpc/<workspace-id>/ frpc 配置与 pid
//! ├── harness/             Durable Task 状态
//! ├── daemon.sock          守护进程 IPC（Unix）
//! ├── daemon.lock          守护进程单实例锁
//! └── daemon.json          守护进程 pid / 版本 / 启动时间
//! ```
//!
//! 0.3.0 之前这个目录是 `~/.gld`，**没有兼容读取**：从旧版本升级上来的话，
//! 先 `mv ~/.gld ~/.config/gld`（守护进程要先停），否则 gld 会当成全新安装，
//! 而工作区和密钥还在旧目录里躺着。

use std::path::PathBuf;

use crate::error::{AppError, AppResult};

/// 覆盖数据目录的环境变量名。
pub const HOME_ENV: &str = "GLD_HOME";

/// 默认数据目录相对用户主目录的位置。
///
/// 三个平台都用 `~/.config/gld`，不跟着各自的系统惯例走（macOS 的
/// `~/Library/Application Support`、Windows 的 `%APPDATA%`）：一份文档、
/// 一条路径，跨机器同步和排障时不用先问"你在哪个系统上"。
/// 要放别处用 `GLD_HOME`。
pub const DEFAULT_DIR_NAME: &str = ".config/gld";

/// 返回数据目录（不保证已存在）。
pub fn data_home() -> AppResult<PathBuf> {
    // 单元测试里绝不能落到真实的数据目录：有些"取路径"的函数会顺手 create_dir_all，
    // 跑一次 cargo test 就把用户主目录写脏，而且下一轮测试还会读到上一轮的残留。
    // 没有显式设 GLD_HOME 的测试，这里自动兜到临时目录。
    #[cfg(test)]
    isolate_for_tests_if_unset();

    if let Some(custom) = std::env::var_os(HOME_ENV) {
        let path = PathBuf::from(custom);
        if path.as_os_str().is_empty() {
            return Err(AppError::Message(format!("{HOME_ENV} 不能为空")));
        }
        // 守护进程会把工作目录切到数据目录，相对路径传给它就指错地方了；这里统一转成绝对路径。
        let path = if path.is_absolute() {
            path
        } else {
            std::env::current_dir()?.join(path)
        };
        return reject_non_directory(path);
    }
    let home = dirs::home_dir()
        .ok_or_else(|| AppError::Message("无法确定用户主目录（HOME 未设置？）".into()))?;
    reject_non_directory(default_home(&home))
}

/// 数据目录已经存在、但不是目录时，当场说清楚。
///
/// 不这么做的话，错误要等到后面某次 create_dir_all 才冒出来，用户看到的是
/// 光秃秃一句 `io error: Not a directory (os error 20)`——既不知道是哪个路径，
/// 也不知道是 GLD_HOME 指错了。踩这个的典型场景是 `GLD_HOME=$(mktemp)`
/// （mktemp 不带 -d，建出来的是文件）。
///
/// 目录不存在是正常的（第一次用），这里不管，交给 ensure_data_home 去建。
fn reject_non_directory(path: PathBuf) -> AppResult<PathBuf> {
    match std::fs::metadata(&path) {
        Ok(meta) if !meta.is_dir() => Err(AppError::Message(format!(
            "数据目录 {} 不是目录。{HOME_ENV} 要指向一个目录（没有会自动建），\
             现在指到了一个文件上。",
            path.display()
        ))),
        _ => Ok(path),
    }
}

/// 没设 `GLD_HOME` 时的默认数据目录。
///
/// 单独拆出来是为了能直接测：走 [`data_home`] 测的话要先把环境变量清掉，
/// 而测试是并行跑的，改进程级的环境变量会互相打架。
fn default_home(user_home: &std::path::Path) -> PathBuf {
    user_home.join(DEFAULT_DIR_NAME)
}

/// 返回数据目录并确保它存在。
pub fn ensure_data_home() -> AppResult<PathBuf> {
    let path = data_home()?;
    create_data_dir(&path)?;
    Ok(path)
}

/// 建数据目录（或它下面的子目录），失败时带上路径和排查方向。
///
/// 裸 `create_dir_all` 的错误是 `io error: Permission denied (os error 13)`，
/// 既没有路径也没有下一步。真实场景是 GLD_HOME 指到了别人的目录、只读挂载、
/// 或者上级目录权限不对。
pub fn create_data_dir(path: &std::path::Path) -> AppResult<()> {
    std::fs::create_dir_all(path).map_err(|error| {
        AppError::Message(format!(
            "建不了数据目录 {}：{error}。检查 {HOME_ENV} 指的位置和上级目录权限。",
            path.display()
        ))
    })
}

/// 守护进程日志目录（`logs/`）。
pub fn logs_dir() -> AppResult<PathBuf> {
    Ok(data_home()?.join("logs"))
}

/// Durable Task（Harness）状态目录。
pub fn harness_root() -> AppResult<PathBuf> {
    Ok(data_home()?.join("harness"))
}

/// 测试专用：把数据目录指向一个进程级临时目录，避免单元测试写坏真实的数据目录。
///
/// 同一进程内多次调用共享同一个目录（`cargo test` 并行跑用例时也安全）。
#[cfg(test)]
pub(crate) fn isolate_for_tests() {
    std::env::set_var(HOME_ENV, shared_test_home());
}

/// 只有在没设 `GLD_HOME` 时才兜底隔离，不覆盖测试自己指定的目录。
#[cfg(test)]
fn isolate_for_tests_if_unset() {
    if std::env::var_os(HOME_ENV).is_none() {
        std::env::set_var(HOME_ENV, shared_test_home());
    }
}

/// 这个测试二进制的所有临时目录都建在这一棵树下：
/// `$TMPDIR/gld-test-homes/<测试二进制名>`，进程里第一次用到时先清掉上一轮的。
///
/// **别用 `tempfile::TempDir` 替代它。** TempDir 靠析构函数删目录，而这里的目录
/// 要么得存在 `static` 里（`GLD_HOME` 是进程级环境变量，一个测试二进制里所有用例
/// 得指同一个地方），要么得穿过一个不方便带着它的函数签名——两种情况 Rust 都不会
/// 跑那个析构：`static` 的析构根本不执行，`keep()` 的字面意思就是"别删了"。
/// 结果是跑一次 `cargo test` 就在 `$TMPDIR` 根下留一堆目录，只增不减
/// （2026-09-23 实测一轮留 23 个，其中 18 个是空的）。
///
/// 换成固定名字 + 每轮先清：残留封顶在"测试二进制个数"，而且全在
/// `gld-test-homes/` 这一个父目录下，`rm -rf` 一条就清干净。
#[cfg(test)]
fn test_root() -> PathBuf {
    use std::sync::OnceLock;
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    ROOT.get_or_init(|| {
        let dir = std::env::temp_dir()
            .join("gld-test-homes")
            .join(test_binary_name());
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("测试临时目录");
        dir
    })
    .clone()
}

/// 整个测试二进制共用的那一个数据目录。
#[cfg(test)]
fn shared_test_home() -> PathBuf {
    let dir = test_root().join("home");
    std::fs::create_dir_all(&dir).expect("temp GLD_HOME");
    dir
}

/// 给单元测试用的一次性目录（工作区、harness 根之类），每次调用给一个新的。
///
/// 用它代替 `tempfile::tempdir().keep()`：后者建出来的目录没人删，见 [`test_root`]。
#[cfg(test)]
pub(crate) fn test_scratch_dir(tag: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU32, Ordering};
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let dir = test_root().join(format!("{tag}-{}", NEXT.fetch_add(1, Ordering::Relaxed)));
    std::fs::create_dir_all(&dir).expect("测试临时目录");
    dir
}

/// 当前测试二进制的名字，去掉 cargo 加的哈希后缀（`home-36476fa50fbac134` → `home`）。
/// 带着哈希的话源码一改名字就变，残留目录又开始一轮一轮累积。
#[cfg(test)]
fn test_binary_name() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|path| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .map(|name| match name.rsplit_once('-') {
            Some((head, _hash)) if !head.is_empty() => head.to_string(),
            _ => name,
        })
        .unwrap_or_else(|| "unknown".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_home_lives_under_the_user_config_dir() {
        assert_eq!(
            default_home(std::path::Path::new("/home/someone")),
            std::path::Path::new("/home/someone/.config/gld")
        );
    }

    /// `GLD_HOME=$(mktemp)`（忘了 -d）要当场说清楚，而不是等到后面
    /// 某次 create_dir_all 报一句光秃秃的 `Not a directory (os error 20)`。
    #[test]
    fn pointing_the_data_home_at_a_file_says_so() {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let error = reject_non_directory(file.path().to_path_buf())
            .expect_err("指向文件必须报错")
            .to_string();

        assert!(error.contains("不是目录"), "{error}");
        assert!(error.contains(HOME_ENV), "{error}");
        assert!(
            error.contains(&file.path().display().to_string()),
            "{error}"
        );
    }

    /// 还不存在是正常的（第一次用），不该报错。
    #[test]
    fn a_data_home_that_does_not_exist_yet_is_fine() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("not-created-yet");

        assert!(reject_non_directory(missing.clone()).is_ok());
    }

    /// 没设 GLD_HOME 的单元测试必须落到临时目录，绝不能碰真实的数据目录。
    ///
    /// 曾经就是这么被写脏的：`managed_frpc_config_path` 这类"取路径"的函数
    /// 顺手 create_dir_all，两个没做隔离的测试在用户主目录里建出了
    /// `<数据目录>/frpc/first-workspace` 这种测试夹具目录。
    #[test]
    fn unit_tests_never_touch_the_real_home() {
        let home = data_home().expect("home");
        let real = dirs::home_dir().expect("user home").join(DEFAULT_DIR_NAME);
        assert_ne!(home, real, "测试用的数据目录不该是真实的 ~/.config/gld");
    }
}
