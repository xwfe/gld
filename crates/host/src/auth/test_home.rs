use std::path::{absolute, Path};
use std::process::Command;

use gld_core::home::HOME_ENV;

const CHILD_TEST: &str = "GLD_HOST_TEST_CHILD";

/// Give a test that uses core's persistent state its own home without mutating the test harness's
/// process environment. The parent's TempDir also cleans up files left by a panicking child.
pub(crate) fn run_isolated(module: &str, name: &str, test: impl FnOnce()) {
    let (_, module) = module.split_once("::").expect("test module path");
    let name = format!("{module}::{name}");
    if std::env::var(CHILD_TEST).as_deref() == Ok(name.as_str()) {
        assert!(std::env::var_os(HOME_ENV).is_some_and(|path| {
            let path = Path::new(&path);
            path.is_absolute() && path.is_dir()
        }));
        test();
        return;
    }

    let temp = tempfile::tempdir().expect("test home");
    let home = absolute(temp.path()).expect("absolute test home");
    let output = Command::new(std::env::current_exe().expect("test executable"))
        .args(["--exact", &name, "--nocapture", "--format=pretty"])
        .env(CHILD_TEST, &name)
        .env(HOME_ENV, home)
        .output()
        .expect("run isolated test");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("test result: ok. 1 passed; 0 failed;"),
        "{name} ({})\n{stdout}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );
}
