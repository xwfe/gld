//! 解释器写文件的命令：路径在 Workspace 里面就放行，在外面就拒（审查 §28）。
//!
//! 这条是文本启发式，不是沙箱；这里钉住的是它别把常见写法误判成"外部路径"，
//! 也别漏掉明摆着往外写的。
//!
//! **整个文件只在 unix 上编译**：大部分用例写的是 unix 路径、用了软链。只给用例加 `#[cfg(unix)]`
//! 不够——辅助函数和夹具字段在 Windows 上就成了没人用的，CI 的 Windows 编译检查开着 `-D warnings`
//! 直接报错（2026-10-18 推送后就是这么红的，审查 §36）。Windows 那一路的路径识别由
//! `tools/command_paths.rs` 的单测在任何平台上跑。

#![cfg(unix)]

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

/// 拷进来只读外面的文件，和 `print(open('/etc/hosts').read())` 一样不归这条管（审查 §108）。
#[test]
fn copying_an_outside_file_into_the_workspace_is_allowed() {
    let fx = fixture();
    let source = path_str(&fx.sibling.join("a.txt"));
    let target = inside(&fx, "a.txt");
    for function in ["copy", "copy2", "copyfile", "copytree"] {
        assert_allowed(
            &fx,
            &format!("import shutil; shutil.{function}('{source}', 'a.txt')"),
        );
        assert_allowed(
            &fx,
            &format!("import shutil; shutil.{function}(\"{source}\", \"{target}\")"),
        );
    }
    // §108 撞到的就是这种：文件名带空格的桌面截图。
    assert_allowed(
        &fx,
        "import shutil; shutil.copy('/Users/x/Desktop/Screenshot 2026-11-01 at 13.22.25.png', 'shot.png')",
    );
    assert_allowed(&fx, "import shutil; shutil.copy('../a.txt', 'a.txt')");
    assert_allowed(&fx, "import shutil; shutil.copy('~/a.txt', 'a.txt')");
    assert_allowed(
        &fx,
        "import shutil\nshutil.copy(\n    '/etc/hosts',\n    'hosts',\n)",
    );

    for script in [
        format!("const fs = require('fs'); fs.copyFileSync('{source}', 'a.txt')"),
        format!("const fs = require('fs'); fs.copyFile('{source}', 'a.txt', () => {{}})"),
    ] {
        let result = check(&fx, &["node", "-e", &script]);
        assert_eq!(result["decision"], "allow", "该放行：{script}\n{result}");
    }

    // 不走 argv、自己写引号的 cmd 形式，字面量的引号长得不一样。
    for cmd in [
        format!("python3 -c \"import shutil; shutil.copy('{source}', 'a.txt')\""),
        format!("python3 -c \"import shutil; shutil.copy(\\\"{source}\\\", \\\"a.txt\\\")\""),
        format!("python3 -c 'import shutil; shutil.copy(\"{source}\", \"a.txt\")'"),
    ] {
        let result = call_tool(&fx.ctx, "check_command", &json!({ "cmd": cmd }));
        assert_eq!(result["decision"], "allow", "该放行：{cmd}\n{result}");
    }
}

#[test]
fn copying_into_the_workspace_really_runs_and_keeps_the_source() {
    let fx = fixture();
    let source = fx.sibling.join("a.txt");
    std::fs::write(&source, "from outside").unwrap();
    let result = call_tool(
        &fx.ctx,
        "exec_command",
        &json!({
            "argv": ["python3", "-c", format!(
                "import shutil; shutil.copy('{}', 'a.txt')", path_str(&source)
            )],
            "yield_time_ms": common::UNTIL_EXIT_MS
        }),
    );
    assert_eq!(result["command_ok"], true, "{result}");
    assert_eq!(
        std::fs::read_to_string(fx.workspace.join("a.txt")).unwrap(),
        "from outside"
    );
    assert_eq!(std::fs::read_to_string(&source).unwrap(), "from outside");
}

#[test]
fn copying_out_of_the_workspace_is_still_rejected() {
    let fx = fixture();
    let sibling = path_str(&fx.sibling.join("x.txt"));
    let source = inside(&fx, "a.txt");
    let cases = [
        format!("import shutil; shutil.copy('a.txt', '{sibling}')"),
        format!("import shutil; shutil.copy('{source}', '{sibling}')"),
        // 源也在外面：只有源被放过，目标照查。
        format!("import shutil; shutil.copy('/etc/hosts', '{sibling}')"),
        "import shutil; shutil.copy('/etc/hosts', '../hosts')".to_string(),
        "import shutil; shutil.copy('/etc/hosts', '~/hosts')".to_string(),
        "import shutil; shutil.copytree('/etc', '/')".to_string(),
        format!("const fs = require('fs'); fs.copyFileSync('a.txt', '{sibling}')"),
        // 放过的只是源那一处文本，同一条命令里别的写法照查。
        format!("import shutil; shutil.copy('/etc/hosts', 'a'); open('{sibling}', 'w')"),
        "import shutil; shutil.copy('/etc/hosts', 'a'); open('/etc/hosts', 'w')".to_string(),
        "import shutil; shutil.copy('/etc/hosts', 'a'); shutil.copy('b', '/etc/hosts')".to_string(),
    ];
    for script in &cases {
        assert_external(&fx, script);
    }
    // 目标是 Workspace 里指向外面的软链，拷进去等于写到外面。
    std::os::unix::fs::symlink(&fx.sibling, fx.workspace.join("out")).unwrap();
    assert_external(
        &fx,
        &format!(
            "import shutil; shutil.copy('/etc/hosts', '{}')",
            inside(&fx, "out/hosts")
        ),
    );
}

/// 移动和改名会把源从原来的地方删掉（Python 3.12 实测），所以源在外面也算往外写。
#[test]
fn moving_a_file_out_of_its_outside_location_is_rejected() {
    let fx = fixture();
    let source = path_str(&fx.sibling.join("a.txt"));
    for script in [
        format!("import shutil; shutil.move('{source}', 'a.txt')"),
        format!("import os; os.rename('{source}', 'a.txt')"),
        format!("import os; os.renames('{source}', 'a.txt')"),
        format!("import os; os.replace('{source}', 'a.txt')"),
        format!("from pathlib import Path; Path('{source}').rename('a.txt')"),
        format!("const fs = require('fs'); fs.renameSync('{source}', 'a.txt')"),
    ] {
        assert_external(&fx, &script);
    }
}

/// 看不全这次调用干了什么的，源不放过。
#[test]
fn copy_calls_that_cannot_be_read_in_full_stay_rejected() {
    let fx = fixture();
    let source = path_str(&fx.sibling.join("a.txt"));
    for script in [
        // 目标不是字面量：看不出写到哪。
        format!("import shutil; shutil.copy('{source}', target)"),
        format!("import shutil; shutil.copy('{source}', 'a' + name)"),
        format!("import shutil; shutil.copy('{source}', f'a{{name}}')"),
        // follow_symlinks=False、symlinks=True 会在 Workspace 里造出指向外面的软链（实测）。
        format!("import shutil; shutil.copy('{source}', 'a', follow_symlinks=False)"),
        format!("import shutil; shutil.copytree('{source}', 'a', symlinks=True)"),
        // 源不是单纯的字面量。
        format!("import shutil; source = '{source}'; shutil.copy(source, 'a')"),
        format!("import shutil; shutil.copy(f'{source}', 'a')"),
        format!("import shutil; shutil.copy('{source}' + name, 'a')"),
        format!("import shutil; shutil.copy(src='{source}', dst='a')"),
        // 名字只是碰巧结尾一样的别的函数。
        format!("import myshutil; myshutil.copy('{source}', 'a')"),
        format!("import shutil; x.shutil.copy('{source}', 'a')"),
        // 不是纯拷贝的那几个函数没进名单。
        format!("import shutil; shutil.copystat('{source}', 'a')"),
    ] {
        assert_external(&fx, &script);
    }
}

#[test]
fn deleting_outside_paths_is_still_rejected() {
    let fx = fixture();
    for script in [
        "import shutil; shutil.rmtree(\"/\")",
        "import shutil; shutil.rmtree('/')",
        "import shutil; shutil.rmtree('/etc')",
        "import shutil; shutil.rmtree('../other')",
        "import os; os.remove('/etc/hosts')",
    ] {
        assert_external(&fx, script);
    }
}
