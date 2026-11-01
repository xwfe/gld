//! 经命令写 `.git`、`.github`，和设置会让 git 跑别的程序的配置。
//!
//! 这是防手滑，不是安全边界（docs/security.md）：脚本存成文件再跑就看不见了。
//! 要钉住的是明显的写法都拦得住，正常的读和 `.gitignore` 不被连累。

use gld_core::tools::policy::{validate_tool_arguments, PolicyReason, PolicySettings};
use serde_json::{json, Value};

fn check(argv: &[&str], policy: &PolicySettings) -> Result<(), PolicyReason> {
    let argv: Vec<Value> = argv.iter().map(|part| json!(part)).collect();
    validate_tool_arguments("exec_command", &json!({ "argv": argv }), policy)
        .map_err(|error| error.reason)
}

fn all_commands() -> PolicySettings {
    gld_core::tools::policy::PolicySettings::from_runtime(&gld_core::workspace::RuntimeConfig {
        command_policy: "all".into(),
        ..Default::default()
    })
}

const WRITES: [(&str, &str); 21] = [
    ("python3", "open('.git/hooks/pre-commit','w').write('x')"),
    (
        "python3",
        "open(\".git/hooks/pre-commit\",\"a\").write('x')",
    ),
    (
        "python3",
        "import shutil;shutil.copy('a','.git/hooks/pre-commit')",
    ),
    (
        "python3",
        "import shutil;shutil.copy('/etc/hosts','.git/hooks/pre-commit')",
    ),
    (
        "python3",
        "from pathlib import Path;Path('.git/config').write_text('x')",
    ),
    ("python3", "open('vendor/x/.git/config','w').write('x')"),
    ("python3", "open('./.GIT/config','w').write('x')"),
    ("python3", "open('git~1/config','w').write('x')"),
    (
        "python3",
        "import os;open(os.path.join('.git','config'),'w').write('x')",
    ),
    (
        "python3",
        "from pathlib import Path;Path('.git/hooks/x').symlink_to('a')",
    ),
    ("python3", "import os;os.symlink('a','.git/hooks/x')"),
    ("python3", "import os;os.link('a','.git/hooks/x')"),
    (
        "python3",
        "from pathlib import Path;Path('a').copy('.git/hooks/x')",
    ),
    (
        "python3",
        "from pathlib import Path;Path('a').replace('.git/hooks/x')",
    ),
    ("node", "require('fs').writeFileSync('.git/hooks/x','y')"),
    ("node", "require('fs').cpSync('a','.git/hooks/x')"),
    ("node", "require('fs').copyFileSync('a','.git/hooks/x')"),
    ("python3", "open('.git/x','rb+')"),
    ("python3", "open('.git/x','w+b')"),
    ("python3", "open('.github/workflows/ci.yml','w').write('x')"),
    (
        "python3",
        "import shutil;shutil.copy('a','.github/workflows/ci.yml')",
    ),
];

#[test]
fn writing_into_the_repository_assets_is_refused() {
    let policy = PolicySettings::default();
    for (program, script) in WRITES {
        let flag = if program == "node" { "-e" } else { "-c" };
        assert_eq!(
            check(&[program, flag, script], &policy),
            Err(PolicyReason::ProtectedRepositoryAsset),
            "{script}"
        );
    }
}

/// cmd 形式的引号和 argv 合成出来的不一样，以前只有不带前缀的 token 才认得出来。
#[test]
fn the_cmd_form_is_refused_too() {
    let error = validate_tool_arguments(
        "exec_command",
        &json!({ "cmd": "python3 -c \"open('.git/hooks/x','w').write('x')\"" }),
        &PolicySettings::default(),
    )
    .expect_err("cmd 形式也要拦");
    assert_eq!(error.reason, PolicyReason::ProtectedRepositoryAsset);
}

#[test]
fn reading_and_files_merely_named_like_git_stay_allowed() {
    let policy = PolicySettings::default();
    for script in [
        "print(open('.git/HEAD').read())",
        "import shutil;shutil.copy('.git/config','backup.txt')",
        "open('.gitignore','w').write('x')",
        "from pathlib import Path;Path('.gitattributes').write_text('x')",
        "open('.gitmodules','a').write('x')",
        "open('a.txt','w').write('https://example.com/x.git')",
        "print('a'.replace('a','b'))",
        "open('a.txt','w').write('x')",
        // 写进文件的内容、排除规则都不是写入目标。
        "open('.gitignore','a').write('.git/')",
        "open('.dockerignore','w').write('.git')",
        "open('out.txt','w').write(open('.git/HEAD').read())",
        "import shutil;shutil.copytree('src','dst',ignore=shutil.ignore_patterns('.git','x'))",
        "import os;os.makedirs('a');print([d for d in os.listdir('.') if d!='.git'])",
        // 读完再 replace 是字符串方法，不是 Path.replace。
        "from pathlib import Path;print(Path('.git/HEAD').read_text().replace('a','b'))",
        "from pathlib import Path;print(Path('/etc/hosts').read_text().replace('a','b'))",
        "from pathlib import Path;print(str(Path('/usr/local/bin')).replace('a','b'))",
        "import os;print(os.path.realpath('/tmp').replace('a','b'))",
        // 只是名字里带 os.、fs. 的别的东西。
        "print(repos.link('/etc/x'))",
        "print(prefs.cpuCount, '/etc/x')",
    ] {
        assert_eq!(
            check(&["python3", "-c", script], &policy),
            Ok(()),
            "{script}"
        );
    }
    assert_eq!(check(&["git", "add", "."], &policy), Ok(()));
    for argv in [
        vec!["git", "commit", "-m", "block os.symlink into .git/hooks"],
        vec!["git", "commit", "-m", "os.remove .github/x"],
        vec!["grep", "-rn", "--exclude-dir=.git", "os.makedirs", "."],
    ] {
        assert_eq!(check(&argv, &policy), Ok(()), "{argv:?}");
    }
}

/// `workdir=.git/hooks` 之后写 `pre-commit`，命令文本里看不到 `.git`。审查实测写进去了。
#[test]
fn writing_from_inside_the_repository_assets_is_refused() {
    let policy = PolicySettings::default();
    for workdir in [".git/hooks", "sub/.git", ".github/workflows", ".GIT"] {
        let error = validate_tool_arguments(
            "exec_command",
            &json!({
                "argv": ["python3", "-c", "open('pre-commit','w').write('x')"],
                "workdir": workdir,
            }),
            &policy,
        )
        .expect_err(workdir);
        assert_eq!(
            error.reason,
            PolicyReason::ProtectedRepositoryAsset,
            "{workdir}"
        );
    }
    assert!(validate_tool_arguments(
        "exec_command",
        &json!({ "argv": ["python3", "-c", "print(open('HEAD').read())"], "workdir": ".git" }),
        &policy,
    )
    .is_ok());
}

/// 新认出来的写法也要过“写到外面”那一道，以前它们连写文件都不算。
#[test]
fn newly_recognised_writes_cannot_reach_outside_either() {
    let policy = PolicySettings::default();
    for script in [
        "import os;os.symlink('/etc/passwd','a')",
        "from pathlib import Path;Path('/etc/hosts').copy('a')",
    ] {
        assert_eq!(
            check(&["python3", "-c", script], &policy),
            Err(PolicyReason::WorkspacePathProtected),
            "{script}"
        );
    }
    assert_eq!(
        check(
            &["node", "-e", "require('fs').cpSync('a','/tmp/x')"],
            &policy
        ),
        Err(PolicyReason::WorkspacePathProtected)
    );
}

/// 这些命令默认不在白名单，用户加进去之后也别写到 `.git`。
#[test]
fn file_commands_the_user_allowed_cannot_write_there_either() {
    let policy = PolicySettings::from_runtime(&gld_core::workspace::RuntimeConfig {
        allowed_commands: "cp,mv,ln,tee,touch,chmod,sed".into(),
        ..Default::default()
    });
    for argv in [
        vec!["cp", "a", ".git/hooks/x"],
        vec!["mv", "a", ".git/hooks/x"],
        vec!["ln", "-s", "a", ".git/hooks/x"],
        vec!["tee", ".git/hooks/x"],
        vec!["touch", ".git/hooks/x"],
        vec!["chmod", "+x", ".git/hooks/x"],
        vec!["sed", "-i", "", "s/a/b/", ".git/config"],
        vec!["cp", "a", ".github/workflows/ci.yml"],
        vec!["sed", "-Ei", "s/a/b/", ".git/config"],
        vec!["sed", "--in-place", "s/a/b/", ".git/config"],
    ] {
        assert_eq!(
            check(&argv, &policy),
            Err(PolicyReason::ProtectedRepositoryAsset),
            "{argv:?}"
        );
    }
    for argv in [
        vec!["cp", "a", ".gitignore"],
        vec!["cp", "/etc/hosts", "a"],
        vec!["sed", "s/a/b/", ".git/config"],
        vec!["cp", ".git/config", "backup.txt"],
    ] {
        assert_eq!(check(&argv, &policy), Ok(()), "{argv:?}");
    }
}

#[test]
fn git_config_that_runs_programs_is_refused() {
    let policy = PolicySettings::default();
    for argv in [
        vec!["git", "config", "core.hooksPath", "h"],
        vec!["git", "config", "--local", "core.hooksPath", "h"],
        vec!["git", "config", "--global", "CORE.FSMONITOR", "./x"],
        vec!["git", "-C", "sub", "config", "core.sshCommand", "./x"],
        vec!["git", "config", "alias.x", "!./x"],
        vec!["git", "config", "diff.external", "./x"],
        vec!["git", "config", "filter.x.clean", "./x"],
        vec!["git", "config", "credential.helper", "./x"],
        vec!["git", "config", "hook.h.command", "./x"],
        vec!["git", "config", "include.path", "./x"],
        vec!["git", "config", "includeIf.onbranch:x.path", "./x"],
        vec!["git", "config", "tar.x.command", "./x"],
        vec!["git", "config", "trailer.x.cmd", "./x"],
        vec!["git", "config", "difftool.x.cmd", "./x"],
        vec!["git", "config", "remote.o.uploadpack", "./x"],
        vec!["git", "config", "submodule.x.update", "!./x"],
        vec!["git", "config", "protocol.allow", "always"],
        vec!["git", "config", "-t", "path", "core.hooksPath", "h"],
        vec![
            "git",
            "--attr-source",
            "HEAD",
            "config",
            "core.hooksPath",
            "h",
        ],
        vec!["git", "config", "set", "core.hooksPath", "h"],
        vec!["git", "clone", "-c", "core.fsmonitor=./x", "a", "b"],
        vec!["git", "clone", "--config=core.fsmonitor=./x", "a", "b"],
        vec!["git", "-c", "core.hooksPath=h", "commit", "-m", "x"],
        vec!["git", "-c", "core.fsmonitor=./x", "status"],
        vec!["git", "-c", "alias.x=!./x", "x"],
        vec!["git", "--config-env=core.sshCommand=X", "fetch"],
    ] {
        assert_eq!(
            check(&argv, &policy),
            Err(PolicyReason::ProtectedRepositoryAsset),
            "{argv:?}"
        );
    }
}

#[test]
fn ordinary_git_config_stays_allowed() {
    let policy = PolicySettings::default();
    for argv in [
        vec!["git", "config", "--get", "core.hooksPath"],
        vec!["git", "config", "--get-regexp", "alias.x"],
        vec!["git", "config", "--list"],
        vec!["git", "config", "get", "core.pager"],
        vec!["git", "config", "--unset", "core.hooksPath"],
        vec!["git", "config", "user.name", "example"],
        vec!["git", "-c", "user.name=example", "commit", "-m", "x"],
        vec!["git", "commit", "-m", "core.hooksPath"],
        vec!["git", "log", "--grep", "git config core.pager x"],
        // 不带值是读。
        vec!["git", "config", "core.hooksPath"],
        vec!["git", "config", "--global", "credential.helper"],
        // 关掉它们、不跑程序的值。
        vec!["git", "-c", "core.pager=cat", "log"],
        vec!["git", "-c", "pager.log=false", "log"],
        vec!["git", "-c", "core.fsmonitor=false", "status"],
        vec!["git", "-c", "credential.helper=", "fetch"],
        vec!["git", "config", "--global", "pager.branch", "false"],
        vec!["git", "config", "alias.st", "status"],
        vec!["git", "config", "submodule.x.update", "checkout"],
        vec!["git", "clone", "-c", "user.name=example", "a", "b"],
    ] {
        assert_eq!(check(&argv, &policy), Ok(()), "{argv:?}");
    }
}

/// `command-policy=all` 放开限制类关卡。写和配置都放行，只有删 `.git` 的保护照旧。
#[test]
fn all_commands_mode_still_only_guards_deletion() {
    let policy = all_commands();
    assert_eq!(
        check(
            &["python3", "-c", "open('.git/hooks/x','w').write('x')"],
            &policy
        ),
        Ok(())
    );
    assert_eq!(
        check(&["git", "config", "core.hooksPath", "h"], &policy),
        Ok(())
    );
    // `docs/security.md` 说这个模式下删 `.git` 仍然拦，以前只认 `-rf`。
    for argv in [
        vec!["rm", "-rf", ".git"],
        vec!["rm", "-fr", ".git"],
        vec!["rm", "-r", ".git"],
        vec!["rm", ".git/index"],
        vec!["rmdir", ".git"],
        vec!["find", ".", "-name", ".git", "-delete"],
        vec![
            "node",
            "-e",
            "require('fs').rmSync('.git',{recursive:true})",
        ],
        vec!["rm", "-rf", ".github."],
    ] {
        assert_eq!(
            check(&argv, &policy),
            Err(PolicyReason::ProtectedRepositoryAsset),
            "{argv:?}"
        );
    }
    assert_eq!(check(&["rm", "-rf", ".gitignore"], &policy), Ok(()));
    assert_eq!(check(&["rm", "-rf", "build"], &policy), Ok(()));
}
