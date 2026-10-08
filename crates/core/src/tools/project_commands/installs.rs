//! exec_command 跑之前要不要先让用户点头：这条命令是拿来装东西的。
//!
//! 判据是命令的**用途**，不是副作用。`pip install`、`cargo install`、`go get`、`npx` 一个没装的包，
//! 本来就是去下载、安装；`cargo build`、`go build`、`mvn test`、`dotnet build` 也会顺手下载声明过的
//! 依赖，但它们是构建——拦了等于每次构建都要问，问多了用户只会闭眼点同意，那道门就没了。
//! npm / pnpm / yarn / bun 的显式安装和"依赖没装时 pnpm 先装"在 [`super::node`]，这里先问它。
//!
//! 只按子命令认，不按命令行里有没有 `install` 这个词：`grep install README.md`、
//! `git commit -m "pip install"` 不该被拦。`workspace` 为空时只认显式的安装命令。

use std::path::{Path, PathBuf};

use crate::tools::workspace::Workspace;

const DEPENDENCIES: &str =
    "installs dependencies: it downloads packages and can run their install or build code";
const PROGRAM: &str =
    "installs software onto this machine, outside the workspace: it downloads and builds or unpacks it";
const TOOL: &str = "downloads a tool from the package registry and runs it";

pub(crate) fn installs_before_running(
    workspace: Option<&Workspace>,
    stem: &str,
    workdir: &str,
    args: &[String],
) -> Option<String> {
    let program = stem.to_ascii_lowercase();
    if let Some(why) = super::node::installs_before_running(workspace, &program, workdir, args) {
        return Some(why);
    }
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    if let Some(why) = playwright_browsers(&program, &words) {
        return Some(why);
    }
    if let Some(why) = runner_fetches(workspace, &program, workdir, &words) {
        return Some(why);
    }
    // `python -m pip install`、`ruby -S gem install`：解释器后面跟的才是真正的程序。
    let (program, words) = match interpreter_module(&program, &words) {
        Some((module, rest)) => (module.to_string(), rest),
        None => (program, words.as_slice()),
    };
    let sub = subcommands(&program, words);
    if let Some(what) = explicit_install(&program, &sub) {
        let command = std::iter::once(program.as_str())
            .chain(sub.iter().copied())
            .collect::<Vec<_>>()
            .join(" ");
        return Some(format!("{command} {what}"));
    }
    uv_run_syncs_first(workspace, &program, workdir, words, &sub)
}

/// 本来就是"装"的子命令。`sub` 是跳过全局选项之后的头两个词。
fn explicit_install(program: &str, sub: &[&str]) -> Option<&'static str> {
    let first = sub.first().copied();
    let second = sub.get(1).copied();
    Some(match (program, first, second) {
        ("pip" | "pip3", Some("install" | "download" | "wheel"), _) => DEPENDENCIES,
        ("uv", Some("sync" | "add"), _) | ("uv", Some("pip"), Some("install" | "sync")) => {
            DEPENDENCIES
        }
        ("uv", Some("tool"), Some("install" | "run")) | ("uvx", Some(_), _) => TOOL,
        ("pipx", Some("install" | "run" | "inject"), _) => TOOL,
        ("poetry", Some("install" | "add" | "update" | "sync"), _) => DEPENDENCIES,
        ("pipenv", Some("install" | "sync" | "update"), _) => DEPENDENCIES,
        ("conda" | "mamba" | "micromamba", Some("install" | "create"), _) => DEPENDENCIES,
        // cargo install 把程序装进 ~/.cargo/bin；add、fetch 是为了拉依赖。build、test 不算（见模块说明）。
        ("cargo", Some("install" | "binstall"), _) => PROGRAM,
        ("cargo", Some("add" | "fetch"), _) => DEPENDENCIES,
        ("go", Some("install"), _) => PROGRAM,
        ("go", Some("get"), _) | ("go", Some("mod"), Some("download")) => DEPENDENCIES,
        ("gem", Some("install" | "update"), _) => DEPENDENCIES,
        ("bundle" | "bundler", Some("install" | "update" | "add"), _) => DEPENDENCIES,
        ("dotnet", Some("add"), Some("package")) | ("dotnet", Some("restore"), _) => DEPENDENCIES,
        ("dotnet", Some("tool" | "workload"), Some("install" | "update" | "restore")) => PROGRAM,
        ("deno", Some("install" | "add"), _) => DEPENDENCIES,
        ("composer", Some("install" | "require" | "update"), _) => DEPENDENCIES,
        ("brew", Some("install" | "reinstall" | "upgrade"), _) => PROGRAM,
        ("apt" | "apt-get" | "dnf" | "yum", Some("install"), _) | ("apk", Some("add"), _) => {
            PROGRAM
        }
        _ => return None,
    })
}

/// 子命令前面可能有带值的全局选项（`uv --directory web sync`、`cargo +nightly --config … install`），
/// 跳过它们和它们的值，取头两个词。没列在这里的选项当不带值。
fn subcommands<'a>(program: &str, words: &[&'a str]) -> Vec<&'a str> {
    let value_flags: &[&str] = match program {
        "uv" => &[
            "--directory",
            "--project",
            "--python",
            "--cache-dir",
            "--config-file",
            "--color",
        ],
        "cargo" => &["--config", "-Z", "-C", "--color"],
        "go" => &["-C"],
        "poetry" => &["-C", "--directory", "-P", "--project"],
        "pip" | "pip3" => &["--python", "--cache-dir", "--log", "--proxy"],
        _ => &[],
    };
    let mut found = Vec::new();
    let mut iter = words.iter().copied();
    while let Some(word) = iter.next() {
        if found.len() == 2 {
            break;
        }
        // cargo 的 `+nightly` 选工具链，不是子命令。
        if program == "cargo" && found.is_empty() && word.starts_with('+') {
            continue;
        }
        if word.starts_with('-') {
            if value_flags.contains(&word) {
                iter.next();
            }
            continue;
        }
        found.push(word);
    }
    found
}

/// `python -m pip …`、`python3 -I -m pip …`、`ruby -S gem …`：返回模块名和它后面的参数。
/// 先遇到脚本名或 `-c` 就不是模块调用。
fn interpreter_module<'a, 'b>(
    program: &str,
    words: &'b [&'a str],
) -> Option<(&'a str, &'b [&'a str])> {
    let (flag, modules): (&str, &[&str]) = if program == "py" || program.starts_with("python") {
        ("-m", &["pip", "pip3"])
    } else if program == "ruby" {
        ("-S", &["gem", "bundle", "bundler"])
    } else {
        return None;
    };
    let mut index = 0;
    while index < words.len() {
        let word = words[index];
        if word == flag {
            let module = *words.get(index + 1)?;
            return modules
                .contains(&module)
                .then(|| (module, &words[index + 2..]));
        }
        if word == "-c" || word == "-e" || !word.starts_with('-') {
            return None;
        }
        // python 的 -X、-W 带值。
        if matches!(word, "-X" | "-W") {
            index += 1;
        }
        index += 1;
    }
    None
}

/// `playwright install` 往用户缓存目录下几百 MB 的浏览器，不管经谁调起来。
fn playwright_browsers(program: &str, words: &[&str]) -> Option<String> {
    if !matches!(
        program,
        "npx" | "npm" | "pnpm" | "yarn" | "bun" | "bunx" | "playwright"
    ) {
        return None;
    }
    let positionals: Vec<&str> = words
        .iter()
        .copied()
        .filter(|word| !word.starts_with('-'))
        .collect();
    let installs = |word: &str| matches!(word, "install" | "install-deps");
    let direct = program == "playwright" && positionals.first().is_some_and(|word| installs(word));
    let via_runner = positionals
        .windows(2)
        .any(|pair| matches!(pair[0], "playwright" | "@playwright/test") && installs(pair[1]));
    (direct || via_runner).then(|| {
        "playwright install downloads browser builds (hundreds of MB) into the user's cache directory, outside the workspace".to_string()
    })
}

/// `npx 包名`、`npm exec`、`bunx`、`bun x`：包在项目里装了就直接跑，没装就从 registry 下载再跑——
/// stdin 不是终端时 npm 不问、直接当 `--yes`。`pnpm dlx`、`yarn dlx` 每次都下载。
fn runner_fetches(
    workspace: Option<&Workspace>,
    program: &str,
    workdir: &str,
    words: &[&str],
) -> Option<String> {
    let after_subcommand = |names: &[&str]| -> Option<usize> {
        let index = words.iter().position(|word| !word.starts_with('-'))?;
        names.contains(&words[index]).then_some(index + 1)
    };
    let rest = match program {
        "npx" | "bunx" => 0,
        "npm" => after_subcommand(&["exec", "x"])?,
        "bun" => after_subcommand(&["x"])?,
        "pnpm" | "yarn" => {
            after_subcommand(&["dlx"])?;
            return Some(format!("{program} dlx {TOOL}"));
        }
        _ => return None,
    };
    let workspace = workspace?;
    let mut packages = Vec::new();
    let mut words = words[rest..].iter().copied();
    let mut spec = None;
    while let Some(word) = words.next() {
        if let Some(package) = word.strip_prefix("--package=") {
            packages.push(package);
            continue;
        }
        match word {
            "--no-install" | "--no" => return None,
            "-p" | "--package" => packages.extend(words.next()),
            // `-c '…'` 在项目自己的环境里跑一段命令，不装包（除非另给了 -p）。
            "-c" | "--call" => {
                words.next();
                if packages.is_empty() {
                    return None;
                }
            }
            "--" => {
                spec = words.next();
                break;
            }
            _ if word.starts_with('-') => {}
            _ => {
                spec = Some(word);
                break;
            }
        }
    }
    let wanted: Vec<&str> = if packages.is_empty() {
        spec.into_iter().collect()
    } else {
        packages
    };
    let dir = workspace.resolve_existing(workdir).ok()?.path;
    let missing = wanted
        .into_iter()
        .find(|package| !installed_locally(workspace, &dir, package))?;
    Some(format!(
        "{program} {missing}: {missing} is not installed in this project (no node_modules/.bin entry up to the workspace root, or a version was asked for), so it {TOOL}"
    ))
}

/// npx 先找项目里的：从 `dir` 往上到工作区根，任何一层的 `node_modules/.bin/<名字>` 或
/// `node_modules/<包名>` 在就算装了。写了版本（`create-vite@latest`）的一律当没装：
/// 装着的未必是那个版本，npx 会去拉。
fn installed_locally(workspace: &Workspace, dir: &Path, package: &str) -> bool {
    if package.rfind('@').is_some_and(|at| at > 0) || package.contains(':') {
        return false;
    }
    let bin = package.rsplit('/').next().unwrap_or(package);
    let mut cursor: Option<PathBuf> = Some(dir.to_path_buf());
    while let Some(current) = cursor {
        if !current.starts_with(workspace.root()) {
            break;
        }
        let modules = current.join("node_modules");
        let bins = modules.join(".bin");
        if bins.join(bin).exists()
            || bins.join(format!("{bin}.cmd")).exists()
            || modules.join(package).join("package.json").is_file()
        {
            return true;
        }
        cursor = current.parent().map(Path::to_path_buf);
    }
    false
}

/// `uv run` 默认先把项目环境同步好：没有 `.venv` 就建一个、装上全部依赖，和 pnpm 跑脚本前先装是一回事。
/// 往上找到 `pyproject.toml` 那一层，那里没有 `.venv` 才拦；`--no-sync` 不同步，不拦。
fn uv_run_syncs_first(
    workspace: Option<&Workspace>,
    program: &str,
    workdir: &str,
    words: &[&str],
    sub: &[&str],
) -> Option<String> {
    if program != "uv" || sub.first() != Some(&"run") || words.contains(&"--no-sync") {
        return None;
    }
    let workspace = workspace?;
    let mut cursor = Some(workspace.resolve_existing(workdir).ok()?.path);
    while let Some(current) = cursor {
        if !current.starts_with(workspace.root()) {
            return None;
        }
        if current.join("pyproject.toml").is_file() {
            return (!current.join(".venv").is_dir()).then(|| {
                "uv run creates the project environment and installs its dependencies first (there is no .venv yet)".to_string()
            });
        }
        cursor = current.parent().map(Path::to_path_buf);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn held(cmd: &str) -> bool {
        let words = shell_words::split(cmd).expect("words");
        installs_before_running(None, &words[0], ".", &words[1..]).is_some()
    }

    /// 不在默认白名单里、要用户自己加的那些，也按用途认。
    #[test]
    fn explicit_installs_in_other_ecosystems_are_held() {
        for cmd in [
            "pip install -r requirements.txt",
            "pip3 install requests",
            "python -m pip install -e .",
            "python3 -I -m pip install x",
            "uv sync --frozen",
            "uv --directory api add httpx",
            "uv pip install ruff",
            "uvx ruff check",
            "pipx run black .",
            "poetry -C api install",
            "pipenv install",
            "cargo install ripgrep",
            "cargo +nightly install cargo-fuzz",
            "cargo add serde",
            "go install golang.org/x/tools/gopls@latest",
            "go get example.com/m@v1",
            "go mod download",
            "gem install rails",
            "ruby -S bundle install",
            "bundle install",
            "dotnet add package Newtonsoft.Json",
            "dotnet tool install -g dotnet-ef",
            "dotnet restore",
            "deno install",
            "composer require monolog/monolog",
            "brew install jq",
            "apt-get install -y curl",
            "pnpm dlx cowsay hi",
            "yarn dlx create-vite",
            "npx playwright install chromium",
            "pnpm exec playwright install --with-deps",
            "playwright install",
        ] {
            assert!(held(cmd), "{cmd} 该要 confirm");
        }
    }

    /// 构建、测试、查询不拦，哪怕它们会顺手下载声明过的依赖。
    #[test]
    fn builds_tests_and_queries_are_not_held() {
        for cmd in [
            "cargo build --locked",
            "cargo test",
            "cargo +nightly fmt",
            "cargo --config net.offline=true check",
            "go build ./...",
            "go test ./...",
            "go mod tidy",
            "python -m pytest",
            "python -c \"print('pip install')\"",
            "python scripts/pip.py install",
            "pip --version",
            "pip list",
            "uv --version",
            "uv run --no-sync pytest",
            "poetry run pytest",
            "dotnet build",
            "dotnet test",
            "mvn install",
            "gradle build",
            "npx playwright test",
            "pnpm exec playwright test",
            "grep install README.md",
            "git commit -m \"pip install\"",
            "echo cargo install",
        ] {
            assert!(!held(cmd), "{cmd} 不该拦");
        }
    }

    #[test]
    fn npx_downloads_only_what_the_project_does_not_have() {
        let dir = tempfile::tempdir().expect("dir");
        let root = dir.path();
        std::fs::create_dir_all(root.join("node_modules/.bin")).expect("bin");
        std::fs::write(root.join("node_modules/.bin/tsc"), "").expect("tsc");
        std::fs::create_dir_all(root.join("node_modules/@scope/tool")).expect("scoped");
        std::fs::write(root.join("node_modules/@scope/tool/package.json"), "{}").expect("pkg");
        std::fs::create_dir_all(root.join("web")).expect("web");
        let ws = Workspace::new(root.to_path_buf()).expect("workspace");
        let held = |cmd: &str, workdir: &str| {
            let words = shell_words::split(cmd).expect("words");
            installs_before_running(Some(&ws), &words[0], workdir, &words[1..]).is_some()
        };

        // 根目录装着的，子目录里也找得到（Node 往上找）。
        for (cmd, workdir) in [
            ("npx tsc --noEmit", "."),
            ("npx tsc", "web"),
            ("npx --yes tsc", "."),
            ("npx @scope/tool", "."),
            ("npm exec tsc", "."),
            ("npm exec -- tsc", "."),
            ("bun x tsc", "."),
            ("npx --no-install left-pad", "."),
            ("npx -c 'tsc --version'", "."),
            ("npx --version", "."),
        ] {
            assert!(!held(cmd, workdir), "{cmd} in {workdir} 不该拦");
        }
        for (cmd, workdir) in [
            ("npx left-pad", "."),
            ("npx create-vite@latest app", "."),
            ("npx tsc@5.6.2", "."),
            ("npx github:user/repo", "."),
            ("npx -p left-pad -c 'left-pad'", "."),
            ("npx --package=cowsay cowsay hi", "."),
            ("npm x -- left-pad", "."),
            ("bunx left-pad", "."),
        ] {
            assert!(held(cmd, workdir), "{cmd} in {workdir} 该要 confirm");
        }
    }

    #[test]
    fn uv_run_is_held_until_the_environment_exists() {
        let dir = tempfile::tempdir().expect("dir");
        let root = dir.path();
        std::fs::create_dir_all(root.join("api/src")).expect("api");
        std::fs::write(root.join("api/pyproject.toml"), "[project]\nname='api'\n").expect("toml");
        let ws = Workspace::new(root.to_path_buf()).expect("workspace");
        let held = |cmd: &str, workdir: &str| {
            let words = shell_words::split(cmd).expect("words");
            installs_before_running(Some(&ws), &words[0], workdir, &words[1..]).is_some()
        };
        assert!(held("uv run pytest", "api"));
        assert!(held("uv run pytest", "api/src"));
        assert!(!held("uv run pytest", "."), "根目录没有 pyproject.toml");
        std::fs::create_dir_all(root.join("api/.venv")).expect("venv");
        assert!(!held("uv run pytest", "api"));
        assert!(held("uv sync", "api"), "同步本身照样要问");
    }
}
