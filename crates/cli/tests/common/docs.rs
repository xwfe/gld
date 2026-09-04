//! 校验一段文本里出现的 `gld ...` 是不是真命令。
//!
//! 两处在用：`gld doctor` 给出的修复命令、以及 README 与 docs 里的示例。
//! 这两处写错的代价一样——照着做的人会得到 "unrecognized subcommand"。

/// 一条从文本里抽出来的命令，以及它出现的位置（报错时要指得出来）。
pub struct Extracted {
    pub tokens: Vec<String>,
    pub source: String,
}

/// 从 Markdown 文档里抽命令。
///
/// 只认「命令位置」上的 gld：行首（允许缩进和 `$` 提示符）或反引号之后。
/// 否则 `cargo test -p gld --test x` 里的包名也会被当成命令，
/// 报出一堆假问题——误报会让人直接把这个测试关掉。
pub fn extract_from_docs(text: &str, source: &str) -> Vec<Extracted> {
    extract_with(text, source, |before| {
        let line_start = before.rfind('\n').map(|at| at + 1).unwrap_or(0);
        let prefix = before[line_start..].trim();
        prefix.is_empty() || prefix == "$" || prefix.ends_with('`')
    })
}

/// 从任意文本里抽命令，不限位置。
///
/// 用于 `gld doctor` 的修复提示——那是我们自己生成的短句，
/// 形如「换端口：gld ws set …」，命令不在行首。
pub fn extract_anywhere(text: &str, source: &str) -> Vec<Extracted> {
    extract_with(text, source, |before| {
        !before
            .chars()
            .next_back()
            .is_some_and(|ch| ch.is_alphanumeric() || ch == '-' || ch == '/')
    })
}

fn extract_with(
    text: &str,
    source: &str,
    is_command_position: impl Fn(&str) -> bool,
) -> Vec<Extracted> {
    let mut found = Vec::new();
    for (index, _) in text.match_indices("gld ") {
        if !is_command_position(&text[..index]) {
            continue;
        }
        let tokens: Vec<String> = text[index..]
            .lines()
            .next()
            .unwrap_or_default()
            .split_whitespace()
            .take_while(|token| is_command_shaped(token))
            .map(str::to_string)
            .collect();
        if tokens.len() > 1 {
            found.push(Extracted {
                tokens,
                source: source.to_string(),
            });
        }
    }
    found
}

/// token 是否还像命令的一部分：子命令词、短/长参数、或简单的取值。
///
/// 占位符 `<X>`、可选写法 `[--force]`、多选写法 `a|b`、`key=value`、
/// 中文说明都不算，遇到就停止解析这一条。
fn is_command_shaped(token: &str) -> bool {
    !token.is_empty()
        && token.is_ascii()
        && !token.contains(['<', '>', '[', ']', '|', '=', '"', '\'', '`', '(', ')', ','])
}

/// 逐级校验命令路径；出错时 panic 并指出是哪个文件的哪条命令。
pub fn assert_valid(extracted: &Extracted) {
    let printable = extracted.tokens.join(" ");
    let mut root = <gld::Cli as clap::CommandFactory>::command();
    // 必须先 build：num_args 这些派生属性是 build 阶段才填上的。
    root.build();
    let mut command = root.clone();

    let mut index = 1;
    while index < extracted.tokens.len() {
        let token = &extracted.tokens[index];
        index += 1;

        if let Some(name) = token.strip_prefix('-') {
            let arg =
                find_flag(&command, &root, name.trim_start_matches('-')).unwrap_or_else(|| {
                    panic!(
                        "{}：`{printable}` 用到的参数 {token} 不存在",
                        extracted.source
                    )
                });
            if arg
                .get_num_args()
                .map(|range| range.takes_values())
                .unwrap_or(false)
            {
                index += 1; // 跳过这个参数的值
            }
            continue;
        }

        if let Some(sub) = command.find_subcommand(token).cloned() {
            command = sub;
            continue;
        }

        // 不是子命令：只有当它长得像命令词、而当前命令又不接受位置参数时，
        // 才判定为写错了；其余情况（路径、密钥名、工具名…）都当作取值放过。
        let looks_like_subcommand = token.chars().all(|c| c.is_ascii_lowercase() || c == '-')
            && command.get_positionals().next().is_none();
        assert!(
            !looks_like_subcommand,
            "{}：`{printable}` 里的 {token} 不是 {} 的子命令",
            extracted.source,
            command.get_name()
        );
        break;
    }
}

fn find_flag(command: &clap::Command, root: &clap::Command, name: &str) -> Option<clap::Arg> {
    let matches = |arg: &clap::Arg| {
        arg.get_long().is_some_and(|long| long == name)
            || arg
                .get_short()
                .is_some_and(|short| short.to_string() == name)
    };
    command
        .get_arguments()
        .find(|arg| matches(arg))
        .or_else(|| {
            command
                .get_subcommands()
                .flat_map(|sub| sub.get_arguments())
                .find(|arg| matches(arg))
        })
        .or_else(|| root.get_arguments().find(|arg| matches(arg)))
        .cloned()
}
