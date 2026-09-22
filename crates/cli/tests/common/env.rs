//! 跑真实 `gld` 二进制的测试环境。
//!
//! 每个 [`Env`] 一套独立的 `GLD_HOME` 和临时项目目录，互不干扰，也不会碰
//! 真实的 `~/.config/gld`——集成测试是真的会拉起守护进程、真的会写数据文件的。

use std::net::TcpStream;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::OnceLock;

pub struct Env {
    pub home: tempfile::TempDir,
    pub project: tempfile::TempDir,
    /// 插到 PATH 最前面的目录，用来放假的 cloudflared / frpc。
    path_prefix: Option<std::path::PathBuf>,
}

impl Env {
    /// 一个空项目目录 + 一个空数据目录。需要固定文件的用 [`Env::write`] 补。
    pub fn new() -> Self {
        Self {
            home: tempfile::tempdir().expect("home dir"),
            project: tempfile::tempdir().expect("project dir"),
            path_prefix: None,
        }
    }

    /// 放一个假的外部命令（cloudflared 之类）到 PATH 最前面。
    ///
    /// gld 只按 PATH 找这些程序，所以这是唯一能在测试里走通隧道链路的办法。
    /// 必须在第一条会拉起守护进程的命令之前调用：守护进程是 fork 出去的，
    /// 它的 PATH 在那一刻就定死了，之后再改也进不去。
    pub fn fake_binary(&mut self, name: &str, script: &str) -> &mut Self {
        let dir = self
            .path_prefix
            .get_or_insert_with(|| self.home.path().join("fake-bin"))
            .clone();
        std::fs::create_dir_all(&dir).expect("create fake bin dir");
        let path = dir.join(name);
        std::fs::write(&path, script).expect("write fake binary");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                .expect("chmod fake binary");
        }
        self
    }

    /// 在项目目录里放一个文件，父目录会自动建出来。
    pub fn write(&self, relative: &str, content: &str) -> &Self {
        let path = self.project.path().join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent dir");
        }
        std::fs::write(path, content).expect("write fixture");
        self
    }

    /// 执行一条 gld 命令，拿到原始 Output（要看退出码时用它）。
    pub fn gld(&self, args: &[&str]) -> Output {
        self.gld_with_env(args, &[])
    }

    /// 同上，但额外设几个环境变量。
    ///
    /// 用来复现"环境里有 HTTP_PROXY"这类场景——那类问题只在特定环境下出现，
    /// 不显式造出来就永远测不到。
    pub fn gld_with_env(&self, args: &[&str], extra: &[(&str, &str)]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_gld"));
        command
            .args(args)
            .env("GLD_HOME", self.home.path())
            .env("NO_COLOR", "1")
            // 外面的环境变量会改变工作区推断，测试必须只认自己的临时目录。
            .env_remove("GLD_WORKSPACE")
            .current_dir(self.project.path());
        if let Some(prefix) = &self.path_prefix {
            let existing = std::env::var("PATH").unwrap_or_default();
            command.env("PATH", format!("{}:{existing}", prefix.display()));
        }
        for (key, value) in extra {
            command.env(key, value);
        }
        command.output().expect("run gld")
    }

    /// 执行并要求成功，返回 stdout。失败时把两个流都打出来——
    /// 只报一句 assert failed 的话，排查得重新手工跑一遍。
    pub fn ok(&self, args: &[&str]) -> String {
        let output = self.gld(args);
        assert!(
            output.status.success(),
            "gld {:?} failed (exit {:?})\nstdout:\n{}\nstderr:\n{}",
            args,
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    /// 执行并把 stdout 当 JSON 解析（记得带 `--json`）。
    pub fn json(&self, args: &[&str]) -> serde_json::Value {
        let text = self.ok(args);
        serde_json::from_str(&text).unwrap_or_else(|error| panic!("not json ({error}): {text}"))
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        // 测试失败时也别留下后台进程。
        let _ = self.gld(&["daemon", "stop", "--force", "--wait", "5"]);
        // 失败时把数据目录留在盘上：日志是唯一能说清"服务当时怎么了"的东西，
        // 跟着 TempDir 一起删掉的话，偶发问题就只剩一句断言失败。
        if std::thread::panicking() {
            let home = std::mem::replace(
                &mut self.home,
                tempfile::tempdir().expect("placeholder home"),
            );
            eprintln!("[env] 失败现场保留在 {}", home.keep().display());
        }
    }
}

impl Default for Env {
    fn default() -> Self {
        Self::new()
    }
}

/// 找一个当前空闲的端口。测试并行跑，写死端口会互相踩。
/// 测试要用的端口下界。**故意落在内核自动分配范围之外**，理由见 [`free_port`]。
const PORT_LOW: u16 = 20000;
/// 上界。Linux 默认从 32768 起自动分配，留一点余量。
const PORT_SPAN: u32 = 12000;

/// 一个空闲的本地端口，从 20000–32000 里挑。
///
/// **别改回 `TcpListener::bind(("127.0.0.1", 0))` 那种写法。** 那是让内核从它的
/// 自动分配池里挑一个（macOS 是 49152–65535，`sysctl net.inet.ip.portrange`；
/// Linux 默认 32768–60999），拿到号就把 listener 关掉，然后指望被测的 gld 稍后
/// 还能 bind 上。问题是这中间有一段窗口，而**那个池子是全系统共用的**：窗口里
/// 任何一次 `bind(0)`——另一个测试起的 gld、守护进程的 socket、HTTP 客户端的本地
/// 端口——都可能把这个号拿走。
///
/// 撞上的现象是 `gld start` 报
///
/// ```text
/// 错误：本地 Actions 端口 49884 已被占用：/…/target/debug/gld（pid 20914）
/// ```
///
/// 看着像"端口没放干净"，其实是两边要了同一个号。窗口有多宽取决于机器多忙，所以
/// **CI 上偶发、本机怎么跑都不复现**（2026-09-20 的 macOS CI 就是这么红的，20 次
/// 里红 2 次）。注意也测不出来："连着取 400 个端口有没有重复"是查不到这个的——
/// 内核给号是顺序往前的，重复的不是号，而是号被池子里的别人抢走了。
///
/// 从 20000 起就没这回事：这一段不在任何一个系统的自动分配范围里，只有明确写了
/// 端口号的人才会碰它，而那只有我们自己的测试。进程内用原子计数往前走，同一个号
/// 不发第二次；起点按 pid 错开，免得并行跑的几个测试二进制从同一处开始找；最后
/// 确认一次当前没人在听。
///
/// **确认的办法是"连一下"，别改回"bind 一下"。** bind 会在测试进程里开一个监听
/// socket，而 macOS 没有 `SOCK_CLOEXEC`：Rust 是先建 socket、再补 close-on-exec，
/// 中间有个空窗。另一个测试线程恰好在这个空窗里 `posix_spawn` 一个 `gld`，这个
/// 监听 socket 就被那个 CLI 继承了，CLI 再拉起守护进程时又传下去——于是隔壁测试
/// 分到的端口被一个毫不相干的守护进程一直占着，报
///
/// ```text
/// 错误：MCP 服务端口 20633 已被占用：/…/target/debug/gld（pid 84644）
/// ```
///
/// 并发跑 30 次左右红一次，`--test-threads=1` 永远不红。连一下只会建一个客户端
/// socket，就算被继承了也不占任何端口。
pub fn free_port() -> u16 {
    static NEXT: OnceLock<AtomicU32> = OnceLock::new();
    let next = NEXT.get_or_init(|| AtomicU32::new(std::process::id()));

    for _ in 0..PORT_SPAN {
        let port = PORT_LOW + (next.fetch_add(1, Ordering::Relaxed) % PORT_SPAN) as u16;
        let address = std::net::SocketAddr::from(([127, 0, 0, 1], port));
        if TcpStream::connect_timeout(&address, std::time::Duration::from_millis(200)).is_err() {
            return port;
        }
    }
    panic!("20000–32000 之间没有一个空闲端口");
}
