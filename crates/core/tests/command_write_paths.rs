//! 解释器写文件的命令：路径在 Workspace 里面就放行，在外面就拒（审查 §28）。
//!
//! 这条是文本启发式，不是沙箱；这里钉住的是它别把常见写法误判成"外部路径"，
//! 也别漏掉明摆着往外写的。
mod common;

use std::path::{Path, PathBuf};

use gld_core::tools::{call_tool, ToolContext};
use serde_json::{json, Value};

struct Fixture {
    _temp: tempfile::TempDir,
    /// 和 Workspace 同级的目录，名字是 Workspace 名字加个后缀：按字符串前缀比会误放行。
    sibling: PathBuf,
    workspace: PathBuf,
    ctx: ToolContext,
}

fn fixture() -> Fixture {
    common::isolate_data_home();
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let workspace = root.join("ws");
    let sibling = root.join("ws2");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::create_dir(&sibling).unwrap();
    let ctx = ToolContext::for_test(workspace.clone(), root.join("harness")).unwrap();
    Fixture {
        _temp: temp,
        sibling,
        workspace,
        ctx,
    }
}

fn check(fx: &Fixture, argv: &[&str]) -> Value {
    let result = call_tool(&fx.ctx, "check_command", &json!({ "argv": argv }));
    assert_eq!(result["ok"], true, "{result}");
    result
}

#[track_caller]
fn assert_allowed(fx: &Fixture, script: &str) {
    let result = check(fx, &["python3", "-c", script]);
    assert_eq!(result["decision"], "allow", "该放行：{script}\n{result}");
}

#[track_caller]
fn assert_external(fx: &Fixture, script: &str) {
    let result = check(fx, &["python3", "-c", script]);
    assert_eq!(result["decision"], "deny", "该拒：{script}\n{result}");
    assert_eq!(result["rule"], "workspace_path_protected", "{result}");
}

fn inside(fx: &Fixture, relative: &str) -> String {
    fx.workspace.join(relative).display().to_string()
}

fn path_str(path: &Path) -> String {
    path.display().to_string()
}

#[test]
fn ordinary_write_scripts_are_not_mistaken_for_external_paths() {
    let fx = fixture();
    // argv 里的单引号被 shell_words::join 写成 '\''，以前反斜杠换成 / 之后成了 '/'。
    assert_allowed(&fx, "open('a.txt', 'w').write('hi')");
    assert_allowed(
        &fx,
        "from pathlib import Path; Path('b.txt').write_text('x')",
    );
    // 除法、整除不是根目录。
    assert_allowed(&fx, "open('c.txt', 'w').write(str(4 / 2))");
    assert_allowed(&fx, "open('d.txt', 'w').write(str(9 // 2))");
    // '\n' 里的反斜杠不是路径分隔符。
    assert_allowed(&fx, "open('e.txt', 'w').write('\\n')");
    // 网址里的 // 不是路径。
    assert_allowed(&fx, "open('f.txt', 'w').write('https://example.com/x')");
}

#[cfg(unix)]
#[test]
fn absolute_paths_inside_the_workspace_are_allowed() {
    let fx = fixture();
    let log = inside(&fx, "target/test-home/run.log");
    assert_allowed(&fx, &format!("open('{log}', 'w').write('x')"));
    assert_allowed(
        &fx,
        &format!(
            "from pathlib import Path; Path(\"{}\").write_text('x')",
            inside(&fx, "notes.md")
        ),
    );
    // 真的执行：文件落在 Workspace 里，目录是命令自己建的。
    let result = call_tool(
        &fx.ctx,
        "exec_command",
        &json!({
            "argv": ["python3", "-c", format!(
                "from pathlib import Path; p = Path('{log}'); p.parent.mkdir(parents=True, exist_ok=True); p.write_text('ok')"
            )],
            "yield_time_ms": common::UNTIL_EXIT_MS
        }),
    );
    assert_eq!(result["command_ok"], true, "{result}");
    assert_eq!(std::fs::read_to_string(&log).unwrap(), "ok");
}

#[cfg(unix)]
#[test]
fn writes_that_leave_the_workspace_are_still_rejected() {
    let fx = fixture();
    let sibling = path_str(&fx.sibling.join("x.txt"));
    let cases = [
        "open('/tmp/gld-policy-probe.txt', 'w').write('x')".to_string(),
        // 前缀相同的兄弟目录。
        format!("open('{sibling}', 'w').write('x')"),
        format!("open(\"{sibling}\", 'w').write('x')"),
        // 写在 Workspace 路径里，再用 .. 跳出去。
        format!(
            "open('{}', 'w').write('x')",
            inside(&fx, "a/../../ws2/x.txt")
        ),
        "open('../outside.txt', 'w').write('x')".to_string(),
        "from pathlib import Path; (Path('..') / 'x.txt').write_text('x')".to_string(),
        // 家目录。
        "from pathlib import Path; Path('~/x.txt').expanduser().write_text('x')".to_string(),
        "import os; p = os.path.expanduser('~/x.txt'); open(p, 'w').write('x')".to_string(),
        // 根目录本身：以前 "/" 后面紧跟引号，正则没认出来。
        "import shutil; shutil.rmtree(\"/\")".to_string(),
        "import shutil; shutil.rmtree('/')".to_string(),
        // 盘符路径在 Unix 上不可能在 Workspace 里。
        "open('C:/Windows/x.txt', 'w').write('x')".to_string(),
    ];
    for script in &cases {
        assert_external(&fx, script);
    }
}

#[cfg(unix)]
#[test]
fn symlinks_inside_the_workspace_that_point_outside_count_as_outside() {
    let fx = fixture();
    std::os::unix::fs::symlink(&fx.sibling, fx.workspace.join("out")).unwrap();
    std::os::unix::fs::symlink(
        fx.sibling.join("not-there-yet.txt"),
        fx.workspace.join("dangling"),
    )
    .unwrap();
    assert_external(
        &fx,
        &format!("open('{}', 'w').write('x')", inside(&fx, "out/x.txt")),
    );
    // 悬空软链：写它会在外面新建文件。
    assert_external(
        &fx,
        &format!("open('{}', 'w').write('x')", inside(&fx, "dangling")),
    );
}

#[test]
fn without_a_workspace_every_absolute_path_counts_as_outside() {
    let policy = gld_core::tools::policy::PolicySettings::default();
    let error = gld_core::tools::policy::validate_tool_arguments(
        "exec_command",
        &json!({
            "argv": ["python3", "-c", "open('/anywhere/x.txt', 'w').write('x')"]
        }),
        &policy,
    )
    .expect_err("不知道 Workspace 在哪时不能放行绝对路径");
    assert_eq!(
        error.reason,
        gld_core::tools::policy::PolicyReason::WorkspacePathProtected
    );
}

/// 写文件的样子以前漏认了几种：`open(` 里有括号、追加模式、改名、touch。认出来才会去查路径。
#[cfg(unix)]
#[test]
fn more_ways_of_writing_are_recognised() {
    let fx = fixture();
    for script in [
        // 以前 `open\([^)]*` 碰到 expanduser 的 `)` 就停，根本不算写文件。
        "import os; open(os.path.expanduser('~/x.txt'), 'w').write('x')",
        "open('/tmp/gld-policy-probe.txt', 'a').write('x')",
        "open('/tmp/gld-policy-probe.txt', mode='r+').write('x')",
        "import os; os.rename('notes.md', '/tmp/gld-policy-probe.txt')",
        "import shutil; shutil.copy('notes.md', '/tmp/gld-policy-probe.txt')",
        "from pathlib import Path; Path('/tmp/gld-policy-probe.txt').touch()",
    ] {
        assert_external(&fx, script);
    }
    // 读外面的文件不归这条管。
    assert_allowed(&fx, "print(open('/etc/hosts').read())");
    // 文件名里带 w 不是写模式。
    assert_allowed(&fx, "print(open('/etc/w.txt').read())");
}
