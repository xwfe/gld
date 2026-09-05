//! 跑真实 `gld` 二进制的测试环境。
//!
//! 每个 [`Env`] 一套独立的 `GLD_HOME` 和临时项目目录，互不干扰，也不会碰
//! 真实的 `~/.gld`——集成测试是真的会拉起守护进程、真的会写数据文件的。

use std::net::TcpListener;
use std::process::{Command, Output};

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
pub fn free_port() -> u16 {
    TcpListener::bind(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}
