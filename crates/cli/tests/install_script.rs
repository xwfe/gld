//! 安装脚本的失败路径：下载出问题时不能动原来能用的那个 gld。
//! 用本地目录冒充 Release（curl 能读 file://），发布包是个假的 shell 脚本。install.ps1 在 CI 的 Windows 任务里跑真实发布包。
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const VERSION: &str = "v9.9.9";
const TARGETS: [&str; 3] = [
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "x86_64-unknown-linux-musl",
];

fn script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/install.sh")
}

/// Debian、Ubuntu 上 sh 是 dash，经管道执行时不能用 bash 语法；有 dash 的机器都跑一遍。
fn shells() -> Vec<&'static str> {
    let mut shells = vec!["sh"];
    if Command::new("dash")
        .arg("-c")
        .arg("exit 0")
        .output()
        .is_ok()
    {
        shells.push("dash");
    }
    shells
}

struct Release {
    dir: tempfile::TempDir,
    install: tempfile::TempDir,
    stubs: tempfile::TempDir,
}

impl Release {
    fn new() -> Self {
        Self::with_binary("#!/bin/sh\necho 'gld 9.9.9 (new)'\n")
    }

    fn with_binary(content: &str) -> Self {
        let release = Self {
            dir: tempfile::tempdir().unwrap(),
            install: tempfile::tempdir().unwrap(),
            stubs: tempfile::tempdir().unwrap(),
        };
        for target in TARGETS {
            let name = format!("gld-{VERSION}-{target}");
            let folder = release.dir.path().join(&name);
            std::fs::create_dir(&folder).unwrap();
            write_executable(&folder.join("gld"), content);
            run_in(
                release.dir.path(),
                "tar",
                &["czf", &format!("{name}.tar.gz"), &name],
            );
            std::fs::remove_dir_all(&folder).unwrap();
        }
        run_in(
            release.dir.path(),
            "sh",
            &[
                "-c",
                "(command -v sha256sum >/dev/null && sha256sum gld-* || shasum -a 256 gld-*) > SHA256SUMS",
            ],
        );
        release
    }

    fn run(&self, shell: &str) -> Output {
        let path = format!(
            "{}:{}",
            self.stubs.path().display(),
            std::env::var("PATH").unwrap()
        );
        Command::new(shell)
            .arg(script())
            .env("GLD_VERSION", VERSION)
            .env("GLD_INSTALL_DIR", self.install.path())
            .env(
                "GLD_BASE_URL",
                format!("file://{}", self.dir.path().display()),
            )
            .env("PATH", path)
            // macOS 的 sh 是 bash 3.2，这个 locale 下中文紧跟变量名才会出错，本机默认的 C locale 测不出来。
            .env("LC_ALL", "en_US.UTF-8")
            .output()
            .unwrap()
    }

    fn installed(&self) -> PathBuf {
        self.install.path().join("gld")
    }

    fn version(&self) -> String {
        let output = Command::new(self.installed()).output().unwrap();
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    /// 已经装着一个能用的旧版。
    fn with_previous(self) -> Self {
        write_executable(&self.installed(), "#!/bin/sh\necho 'gld 1.0.0 (old)'\n");
        self
    }

    fn stub_uname(&self, system: &str, machine: &str) {
        write_executable(
            &self.stubs.path().join("uname"),
            &format!("#!/bin/sh\ncase \"$1\" in -s) echo {system} ;; -m) echo {machine} ;; esac\n"),
        );
    }

    fn entries(&self) -> Vec<String> {
        std::fs::read_dir(self.install.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect()
    }
}

fn write_executable(path: &Path, content: &str) {
    std::fs::write(path, content).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn run_in(dir: &Path, program: &str, arguments: &[&str]) {
    let status = Command::new(program)
        .args(arguments)
        .current_dir(dir)
        .status()
        .unwrap();
    assert!(status.success(), "{program} {arguments:?}");
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn a_good_release_installs_and_replaces_the_previous_version() {
    for shell in shells() {
        let release = Release::new();
        let first = release.run(shell);
        assert!(first.status.success(), "{shell}: {}", stderr(&first));
        assert_eq!(release.version(), "gld 9.9.9 (new)");
        let release = release.with_previous();
        let second = release.run(shell);
        assert!(second.status.success(), "{shell}: {}", stderr(&second));
        assert_eq!(release.version(), "gld 9.9.9 (new)");
        assert!(String::from_utf8_lossy(&second.stdout).contains("gld 1.0.0 (old)"));
        assert_eq!(release.entries(), ["gld"], "{shell}: left temporary files");
    }
}

#[test]
fn a_tampered_download_leaves_the_previous_version_alone() {
    for shell in shells() {
        let release = Release::new().with_previous();
        let sums = release.dir.path().join("SHA256SUMS");
        let text = std::fs::read_to_string(&sums).unwrap();
        let tampered: String = text
            .lines()
            .map(|line| {
                let (_, name) = line.split_once("  ").unwrap();
                format!("{}  {name}\n", "0".repeat(64))
            })
            .collect();
        std::fs::write(&sums, tampered).unwrap();
        let output = release.run(shell);
        assert!(!output.status.success());
        assert!(
            stderr(&output).contains("校验和对不上"),
            "{}",
            stderr(&output)
        );
        assert_eq!(release.version(), "gld 1.0.0 (old)");
    }
}

#[test]
fn a_missing_version_or_checksum_entry_installs_nothing() {
    let release = Release::new().with_previous();
    std::fs::write(release.dir.path().join("SHA256SUMS"), "").unwrap();
    let output = release.run("sh");
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("SHA256SUMS 里没有"),
        "{}",
        stderr(&output)
    );
    assert_eq!(release.version(), "gld 1.0.0 (old)");

    let release = Release::new().with_previous();
    for entry in std::fs::read_dir(release.dir.path()).unwrap() {
        std::fs::remove_file(entry.unwrap().path()).unwrap();
    }
    let output = release.run("sh");
    assert!(!output.status.success());
    assert!(stderr(&output).contains("下载不了"), "{}", stderr(&output));
    assert_eq!(release.version(), "gld 1.0.0 (old)");
}

#[test]
fn a_binary_that_cannot_run_here_does_not_replace_a_working_one() {
    let release = Release::with_binary("#!/bin/sh\nexit 1\n").with_previous();
    let output = release.run("sh");
    assert!(!output.status.success());
    assert!(stderr(&output).contains("跑不起来"), "{}", stderr(&output));
    assert_eq!(release.version(), "gld 1.0.0 (old)");
}

#[test]
fn unsupported_platforms_are_named_instead_of_failing_on_a_missing_file() {
    for (system, machine, expected) in [
        ("Linux", "aarch64", "Linux aarch64"),
        ("FreeBSD", "amd64", "FreeBSD"),
        ("MINGW64_NT-10.0", "x86_64", "install.ps1"),
    ] {
        let release = Release::new();
        release.stub_uname(system, machine);
        let output = release.run("sh");
        assert!(!output.status.success());
        assert!(stderr(&output).contains(expected), "{}", stderr(&output));
    }
}

#[test]
fn linux_uses_the_static_package_so_old_systems_can_run_it() {
    let release = Release::new();
    release.stub_uname("Linux", "x86_64");
    let output = release.run("sh");
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(String::from_utf8_lossy(&output.stdout).contains("x86_64-unknown-linux-musl"));
}

#[test]
fn the_scripts_avoid_the_encoding_traps_of_their_shells() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts");
    // Windows PowerShell 5.1 按错误的代码页解码下载的脚本，中文字节可能变成弯引号，被当成真引号解析。
    let powershell = std::fs::read(root.join("install.ps1")).unwrap();
    assert!(powershell.is_ascii(), "install.ps1 must stay ASCII");
    let shell = std::fs::read_to_string(root.join("install.sh")).unwrap();
    let mut chars = shell.char_indices().peekable();
    while let Some((start, c)) = chars.next() {
        if c != '$' {
            continue;
        }
        let mut end = start + 1;
        while let Some((index, next)) = chars.peek().copied() {
            if next.is_ascii_alphanumeric() || next == '_' {
                end = index + 1;
                chars.next();
            } else {
                assert!(
                    next.is_ascii() || end == start + 1,
                    "write ${{var}} before non-ASCII text in install.sh: {}",
                    &shell[start..index + next.len_utf8()]
                );
                break;
            }
        }
    }
}
