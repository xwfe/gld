//! 从命令文本里认出"往 Workspace 外面写"的路径。
//!
//! 只是文本启发式，不是沙箱：策略层只在命令同时像在写文件时才问这里
//! （`policy::validate_command_for_workspace`），脚本存成文件再跑就看不见了。
//! 规则写在 docs/security.md「写文件」那一节。

use std::cmp::Reverse;
use std::ops::Range;
use std::path::{Component, Path};
use std::sync::OnceLock;

/// 命令文本里有没有指向 Workspace 外面的路径：`../` 跳出去、`~/` 家目录、
/// 不在 Workspace 里的绝对路径（`root` 为 `None` 时绝对路径一律算外面）。
///
/// 以前是先把 `\` 全换成 `/`、再把引号或空白后面的任何 `/` 当绝对路径，于是
/// argv 里一个单引号（`shell_words::join` 写成 `'\''`）、`4 / 2`、Workspace 里面的
/// 绝对路径都被拒，反而 `shutil.rmtree("/")` 放行了（审查 §29）。
///
/// 拷贝的读取源不算（见 [`blank_copy_sources`]）。
pub(crate) fn command_contains_external_path(command: &str, root: Option<&Path>) -> bool {
    let command = blank_copy_sources(command);
    if command.contains("../") || command.contains("..\\") {
        return true;
    }
    path_like_tokens(&command, cfg!(windows))
        .into_iter()
        .any(|token| !root.is_some_and(|root| absolute_path_inside(token, root)))
}

/// 只读第一个路径、只写第二个路径的函数；`true` 表示目标后面还能跟别的参数。
///
/// `shutil.move`、`os.rename`、`os.replace` 这些不能进来：它们会把源从原来的地方删掉，
/// 源在外面就是在改外面。Python 的 `follow_symlinks=False`、`symlinks=True` 会在 Workspace
/// 里造出指向外面的软链，所以 Python 这几个目标后面不许再有参数；Node 的第三个参数只是
/// 拷贝模式或回调。
const COPY_CALLS: [(&str, bool); 6] = [
    ("shutil.copy(", false),
    ("shutil.copy2(", false),
    ("shutil.copyfile(", false),
    ("shutil.copytree(", false),
    ("fs.copyFile(", true),
    ("fs.copyFileSync(", true),
];

/// 把拷贝调用的源路径从命令文本里抠掉，后面的判断就只看得到目标和别的路径。
///
/// 只抠这一处文本：同一个路径在命令别处再出现，照样按外部路径算。
fn blank_copy_sources(command: &str) -> String {
    let mut sources: Vec<Range<usize>> = COPY_CALLS
        .iter()
        .flat_map(|&(name, more_args)| {
            command.match_indices(name).filter_map(move |(at, _)| {
                let before = command[..at].chars().next_back();
                if before.is_some_and(|ch| ch.is_alphanumeric() || "_.$".contains(ch)) {
                    return None;
                }
                copy_source(command, at + name.len(), more_args)
            })
        })
        .collect();
    sources.sort_by_key(|source| Reverse(source.start));
    let mut blanked = command.to_string();
    for source in sources {
        blanked.replace_range(source, "");
    }
    blanked
}

/// 参数从 `args_at` 开始的那次调用，源路径在命令里的位置。
///
/// 源和目标都得是单纯的字符串字面量，不然看不出到底读哪、写哪，返回 `None`，路径照常查。
fn copy_source(command: &str, args_at: usize, more_args: bool) -> Option<Range<usize>> {
    let args = command[args_at..].trim_start();
    let (source, rest) = string_literal(args)?;
    let (_, rest) = string_literal(rest.trim_start().strip_prefix(',')?.trim_start())?;
    let rest = rest.trim_start();
    let closed = if more_args {
        rest.starts_with([')', ','])
    } else {
        rest.strip_prefix(',')
            .unwrap_or(rest)
            .trim_start()
            .starts_with(')')
    };
    let args_at = command.len() - args.len();
    closed.then(|| args_at + source.start..args_at + source.end)
}

/// 开头的字符串字面量：内容的范围和它后面的文本。
///
/// argv 经 `shell_words::join` 后单引号写成 `'\''`，`cmd` 里的双引号常写成 `\"`，都要认。
/// 带前缀的（`f'…'` 会展开变量）、带转义的、反引号（JS 模板）都不算单纯的字面量。
fn string_literal(text: &str) -> Option<(Range<usize>, &str)> {
    let quote = [r"'\''", r#"\""#, "'", "\""]
        .into_iter()
        .find(|quote| text.starts_with(quote))?;
    let start = quote.len();
    let end = start + text[start..].find(['\'', '"', '`', '\\'])?;
    let rest = text[end..].strip_prefix(quote)?;
    Some((start..end, rest))
}

/// 命令文本里有没有一段路径落在 `.git` 或 `.github` 上。`.gitignore`、`.gitattributes` 不算。
/// `git~1`、尾巴上的点、`::$INDEX_ALLOCATION` 都是 Windows 上 `.git` 的别名。
pub(crate) fn mentions_repository_asset(command: &str) -> bool {
    static PATTERN: OnceLock<regex::Regex> = OnceLock::new();
    PATTERN
        .get_or_init(|| {
            regex::Regex::new(
                r#"(?i)(?:^|[\s'"`(,=\[{/\\:])(?:\.git\.*|git~1|\.github\.*)(?:$|[/\\\s'"`),\]}*?:])"#,
            )
            .expect("valid regex")
        })
        .is_match(command)
}

/// 像在写文件的命令有没有写到 `.git`、`.github`。
///
/// 从文本里分不清哪段是写入目标，所以把明显不是目标的先抠掉再看：拷贝的读取源、写进文件的内容
/// （`open('.gitignore','a').write('.git/')`）、排除规则。别的提到 `.git` 的写法宁可误拒。
///
/// `apply_patch` 永远不写 `.git`，改 workflow 也要确认；经命令写不该比它宽。
pub(crate) fn command_writes_repository_asset(command: &str) -> bool {
    if let Ok(parts) = shell_words::split(command) {
        // 这几个的最后一个参数才是目标，`cp .git/config backup.txt` 是读。
        if let Some(("cp" | "rsync" | "install", arguments)) = split_program(&parts) {
            return arguments
                .last()
                .is_some_and(|target| mentions_repository_asset(target));
        }
    }
    static NOT_TARGETS: OnceLock<regex::Regex> = OnceLock::new();
    let not_targets = NOT_TARGETS.get_or_init(|| {
        regex::Regex::new(
            r#"(?i)\.write(?:_text|_bytes)?\([^;\n]*|ignore_patterns\([^)]*\)|--exclude[\w-]*(?:=|\s+)\S+|[!=]=\s*(?:'\\''|\\?['"])\.git(?:hub)?(?:'\\''|\\?['"])"#,
        )
        .expect("valid regex")
    });
    let blanked = blank_copy_sources(command);
    mentions_repository_asset(&not_targets.replace_all(&blanked, ""))
}

/// 拆出程序名（去掉路径和 `.exe`，转小写）和它的参数。
fn split_program(parts: &[String]) -> Option<(&str, &[String])> {
    let (program, arguments) = parts.split_first()?;
    let name = program.rsplit(['/', '\\']).next().unwrap_or(program);
    let name = name
        .strip_suffix(".exe")
        .or_else(|| name.strip_suffix(".EXE"))
        .unwrap_or(name);
    // 只认全小写：这里的名字都是小写，Windows 上写成 `CP` 的按没认出来算，后面还有白名单。
    Some((name, arguments))
}

/// `git config` 或 `git -c` 设置的值会不会让 git 去跑别的程序。
///
/// 这些键和往 `.git/hooks` 写脚本是一回事：`git -c core.fsmonitor=./x status` 直接就跑起来了，
/// `only:git` 这种收窄过的白名单也拦不住。列表挡的是常见的，不是全部（docs/security.md）。
/// 要解析参数，不能在整行里找：`git log --grep 'git config core.pager x'` 只是在搜字符串。
pub(crate) fn git_config_runs_programs(command: &str) -> bool {
    let Ok(parts) = shell_words::split(command) else {
        return false;
    };
    let Some(("git", arguments)) = split_program(&parts) else {
        return false;
    };
    let mut arguments = arguments.iter().map(String::as_str);
    while let Some(argument) = arguments.next() {
        match argument {
            "-c" => {
                if arguments.next().is_some_and(inline_setting_runs_programs) {
                    return true;
                }
            }
            "--config-env" => {
                if arguments.next().is_some_and(env_setting_runs_programs) {
                    return true;
                }
            }
            "-C" | "--git-dir" | "--work-tree" | "--namespace" | "--attr-source" => {
                arguments.next();
            }
            "config" => return config_call_runs_programs(arguments),
            "clone" => return clone_call_runs_programs(arguments),
            _ if argument.starts_with("--config-env=") => {
                if env_setting_runs_programs(&argument["--config-env=".len()..]) {
                    return true;
                }
            }
            _ if argument.starts_with('-') => {}
            _ => return false,
        }
    }
    false
}

fn config_call_runs_programs<'a>(mut arguments: impl Iterator<Item = &'a str>) -> bool {
    while let Some(argument) = arguments.next() {
        match argument {
            "-f" | "--file" | "--blob" | "-t" | "--type" | "--default" | "--comment"
            | "--value" => {
                arguments.next();
            }
            "set" => {}
            "get" | "list" | "unset" | "edit" | "-l" | "--list" | "-e" | "--edit" => return false,
            _ if argument.starts_with("--get") || argument.starts_with("--unset") => return false,
            _ if argument.starts_with('-') => {}
            // 键后面没有值就是读：`git config core.hooksPath`。
            key => {
                return arguments
                    .find(|argument| !argument.starts_with('-'))
                    .is_some_and(|value| setting_runs_programs(key, value));
            }
        }
    }
    false
}

fn clone_call_runs_programs<'a>(mut arguments: impl Iterator<Item = &'a str>) -> bool {
    while let Some(argument) = arguments.next() {
        let setting = match argument {
            "-c" | "--config" => arguments.next(),
            _ => argument.strip_prefix("--config="),
        };
        if setting.is_some_and(inline_setting_runs_programs) {
            return true;
        }
    }
    false
}

/// `-c key=value`。没有 `=` 时 git 当成 true。
fn inline_setting_runs_programs(setting: &str) -> bool {
    let (key, value) = setting.split_once('=').unwrap_or((setting, "true"));
    setting_runs_programs(key, value)
}

/// `--config-env=key=ENVVAR`：值在环境变量里，看不到，按会跑算。
fn env_setting_runs_programs(setting: &str) -> bool {
    let key = setting.split('=').next().unwrap_or(setting);
    program_key_pattern().is_match(key)
}

fn program_key_pattern() -> &'static regex::Regex {
    static PATTERN: OnceLock<regex::Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        regex::Regex::new(
            r"(?i)^(core\.(hookspath|fsmonitor|sshcommand|pager|editor|askpass|gitproxy|alternaterefscommand)|hook\..+\.command|sequence\.editor|interactive\.difffilter|diff\.external|alias\..+|pager\..+|credential\.(.+\.)?helper|gpg\.(.+\.)?program|gpg\.ssh\.defaultkeycommand|filter\..+\.(clean|smudge|process)|diff\..+\.(textconv|command)|merge\..+\.driver|(diff|merge)tool\..+\.cmd|tar\..+\.command|trailer\..+\.(cmd|command)|remote\..+\.(uploadpack|receivepack)|uploadpack\.packobjectshook|submodule\..+\.update|include\.path|includeif\..+\.path|init\.templatedir|browser\..+\.cmd|man\..+\.cmd|sendemail\.(.+\.)?sendmailcmd|protocol\.(.+\.)?allow)$",
        )
        .expect("valid regex")
    })
}

fn setting_runs_programs(key: &str, value: &str) -> bool {
    if !program_key_pattern().is_match(key) {
        return false;
    }
    let key = key.to_ascii_lowercase();
    // 这两个只有 `!` 开头才跑程序。别名还能以 `-c` 开头再套一层配置，也不放。
    if key.starts_with("alias.") {
        return value.starts_with(['!', '-']);
    }
    if key.starts_with("submodule.") {
        return value.starts_with('!');
    }
    // `git -c core.pager=cat log`、`-c credential.helper=` 是在关掉它们，不是在跑东西。
    !matches!(
        value.to_ascii_lowercase().as_str(),
        "" | "false" | "true" | "cat" | "off" | "no" | "0" | "never"
    )
}

/// 挑出像路径的片段：前面是开头、空白、引号或 `(` `=` `,` `[` `{`，本身以 `/`、`~/`、
/// 盘符（`C:/`、`C:\`）开头；引号里的 `..` 也算。
///
/// 引号后面的取到同一个引号为止，所以 `"/"` 和带空格的路径都认得出；不在引号里的，
/// `/` 后面得紧跟名字字符，不然 `a / b`、`a // b` 这种除法会被当成根目录。
///
/// `\` 只在 Windows 上算分隔符（`backslash_separates`，测试里两种都跑）。就算在 Windows 上，
/// `\` 开头的也要像路径——`\\server` 或者后面还有一个分隔符（`\Windows\System32`）——才算，
/// 不然 Python 里的 `'\n'`、argv 里的 `'\''` 都成了"根目录下的文件"。
fn path_like_tokens(command: &str, backslash_separates: bool) -> Vec<&str> {
    let is_separator = |ch: char| ch == '/' || (backslash_separates && ch == '\\');
    let is_name =
        |ch: Option<char>| ch.is_some_and(|ch| ch.is_alphanumeric() || "_.~-$@+%".contains(ch));
    let mut tokens = Vec::new();
    let mut prev: Option<char> = None;
    for (start, ch) in command.char_indices() {
        let before = prev.replace(ch);
        let quote = before.filter(|p| matches!(p, '"' | '\'' | '`'));
        let at_boundary = match before {
            None => true,
            Some(p) => {
                quote.is_some() || p.is_whitespace() || matches!(p, '(' | '=' | ',' | '[' | '{')
            }
        };
        if !at_boundary {
            continue;
        }
        let rest = &command[start..];
        let end = match quote {
            Some(q) => rest.find(q),
            None => rest.find(|c: char| {
                c.is_whitespace() || matches!(c, '"' | '\'' | '`' | ',' | ';' | ')' | ']' | '}')
            }),
        }
        .unwrap_or(rest.len());
        let token = &rest[..end];
        let mut chars = token.chars();
        let second = chars.clone().nth(1);
        let path_like = match chars.next() {
            Some('/') => quote.is_some() || is_name(second),
            Some('\\') if backslash_separates => {
                second == Some('\\') || (is_name(second) && token[1..].contains(is_separator))
            }
            Some('~') => second.is_some_and(is_separator) || (quote.is_some() && second.is_none()),
            Some('.') => quote.is_some() && token == "..",
            Some(letter) if letter.is_ascii_alphabetic() => {
                second == Some(':') && token[2..].starts_with(['/', '\\'])
            }
            _ => false,
        };
        if path_like {
            tokens.push(token);
        }
    }
    tokens
}

/// 绝对路径是不是落在 Workspace 里面。从最长的已经存在的那一级 canonicalize，
/// 所以软链指到外面的、悬空的软链都算外面；`starts_with` 按路径组件比，
/// `/a/gld2` 不算在 `/a/gld` 里。`root` 是 `Workspace::root`，已经 canonicalize 过。
fn absolute_path_inside(token: &str, root: &Path) -> bool {
    let path = Path::new(token);
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir))
    {
        return false;
    }
    let mut existing = path;
    while std::fs::symlink_metadata(existing).is_err() {
        match existing.parent() {
            Some(parent) => existing = parent,
            None => return false,
        }
    }
    existing
        .canonicalize()
        .is_ok_and(|real| real.starts_with(root))
}

#[cfg(test)]
mod tests {
    use super::{blank_copy_sources, path_like_tokens};

    /// Windows 上 `command_write_paths` 不编译，字面量的几种引号写法在这里跨平台钉住。
    #[test]
    fn copy_sources_are_blanked_in_every_quoting() {
        for (command, blanked) in [
            ("shutil.copy('/a', 'b')", "shutil.copy('', 'b')"),
            (r#"shutil.copy("/a", "b")"#, r#"shutil.copy("", "b")"#),
            (
                r#"shutil.copy(\"/a\", \"b\")"#,
                r#"shutil.copy(\"\", \"b\")"#,
            ),
            (
                r"shutil.copy('\''/a'\'', '\''b'\'')",
                r"shutil.copy('\'''\'', '\''b'\'')",
            ),
            (
                "fs.copyFile('/a', 'b', cb); shutil.copy2('/c', 'd',\n)",
                "fs.copyFile('', 'b', cb); shutil.copy2('', 'd',\n)",
            ),
        ] {
            assert_eq!(blank_copy_sources(command), blanked);
        }
    }

    /// 带转义的路径不认（Windows 的 `C:\a` 因此还会被拒），`'a\', '` 这种也拼不出假的字面量。
    #[test]
    fn copy_sources_stay_when_the_call_is_not_plain() {
        for command in [
            r"shutil.copy('C:\a', 'b')",
            r"shutil.copy('/a\', '/b', 'c')",
            "shutil.copy('/a', 'b', follow_symlinks=False)",
            "shutil.copy('/a', b)",
            "shutil.copy('/a')",
            "shutil.move('/a', 'b')",
            "myshutil.copy('/a', 'b')",
        ] {
            assert_eq!(blank_copy_sources(command), command);
        }
    }

    #[test]
    fn unix_mode_ignores_backslashes() {
        assert!(path_like_tokens(r"open('a.txt','w').write('\n')", false).is_empty());
        assert_eq!(
            path_like_tokens(r#"open("/tmp/x", 'w')"#, false),
            ["/tmp/x"]
        );
        assert_eq!(path_like_tokens(r#"rmtree("/")"#, false), ["/"]);
        assert!(path_like_tokens("print(4 / 2, 9 // 2)", false).is_empty());
    }

    /// Windows 上 `\` 是分隔符，但 `'\n'`、`'\''` 这种转义不能当成路径。
    #[test]
    fn windows_mode_takes_backslash_paths_but_not_escapes() {
        assert!(path_like_tokens(r"open('a.txt','w').write('\n')", true).is_empty());
        assert!(path_like_tokens(r"python3 -c 'print('\''x'\'')'", true).is_empty());
        assert_eq!(
            path_like_tokens(r"open('\Windows\x.txt','w')", true),
            [r"\Windows\x.txt"]
        );
        assert_eq!(
            path_like_tokens(r"open('\\server\share\x','w')", true),
            [r"\\server\share\x"]
        );
        assert_eq!(
            path_like_tokens(r"open('C:\Users\x.txt','w')", true),
            [r"C:\Users\x.txt"]
        );
    }
}
