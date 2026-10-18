//! 从命令文本里认出"往 Workspace 外面写"的路径。
//!
//! 只是文本启发式，不是沙箱：策略层只在命令同时像在写文件时才问这里
//! （`policy::validate_command_for_workspace`），脚本存成文件再跑就看不见了。
//! 规则写在 docs/security.md「写文件」那一节。

use std::path::{Component, Path};

/// 命令文本里有没有指向 Workspace 外面的路径：`../` 跳出去、`~/` 家目录、
/// 不在 Workspace 里的绝对路径（`root` 为 `None` 时绝对路径一律算外面）。
///
/// 以前是先把 `\` 全换成 `/`、再把引号或空白后面的任何 `/` 当绝对路径，于是
/// argv 里一个单引号（`shell_words::join` 写成 `'\''`）、`4 / 2`、Workspace 里面的
/// 绝对路径都被拒，反而 `shutil.rmtree("/")` 放行了（审查 §29）。
pub(crate) fn command_contains_external_path(command: &str, root: Option<&Path>) -> bool {
    if command.contains("../") || command.contains("..\\") {
        return true;
    }
    path_like_tokens(command, cfg!(windows))
        .into_iter()
        .any(|token| !root.is_some_and(|root| absolute_path_inside(token, root)))
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
    use super::path_like_tokens;

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
