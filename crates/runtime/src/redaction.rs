//! 环境变量筛选和命令文本脱敏共用的密钥判据。
//!
//! 只处理显式传入的文本；会话历史、运行记录和执行环境使用同一份规则。

use std::sync::OnceLock;

use regex::Regex;

/// 像密钥的名字：以 token、secret、password、auth、api key 这些关键词结尾的整个词。
///
/// 取「以关键词结尾的整个词」而不是 \b 起头：`_` 是单词字符，\b 下 GITHUB_TOKEN=、ADMIN_PASSWORD:
/// 这类最常见的环境变量写法整条漏掉。脱敏命令文本和不交给项目命令的环境变量（[`looks_like_secret_name`]）
/// 用的是同一个判据，改这里两边一起变。
const SECRET_NAME: &str = r"[A-Za-z0-9_-]*(?:(?:api|access|secret)[_ -]?key|token|secret|cookie|password|passwd|pwd|auth)";

/// 这个环境变量名像不像装着密钥。`PWD`、`OLDPWD` 是当前目录，不算。
pub fn looks_like_secret_name(name: &str) -> bool {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    let pattern = PATTERN
        .get_or_init(|| Regex::new(&format!("(?i)^{SECRET_NAME}$")).expect("secret name regex"));
    !matches!(name, "PWD" | "OLDPWD") && pattern.is_match(name)
}

pub fn redact_text(value: &mut String) -> bool {
    const NAME: &str = SECRET_NAME;
    // 带引号的值：已脱敏的 [REDACTED]（再跑一遍结果不变）、"..."（认 \" 转义）、
    // \"...\"、'...'。引号没闭合就抹到行尾——命令原文先截到 500 字再脱敏，
    // 截断常落在 JSON 里。
    const QUOTED: &str =
        r#"\[REDACTED\]|"(?:[^"\\\n]|\\.)*"?|\\"(?:[^"\\\n]|\\[^"\n])*(?:\\")?|'[^'\n]*'?"#;
    static PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();
    let patterns = PATTERNS.get_or_init(|| {
        vec![
            Regex::new(r"(?i)\b(bearer\s+)[A-Za-z0-9._~+/=-]{6,}").expect("bearer regex"),
            // URL 里的 user:口令@。口令抹到主机前最后一个 @，没转义的 @ 也不漏后半截。
            // 要排在赋值规则前面，否则 https://x-access-token:xxx@github.com/o/r
            // 会被当成 x-access-token=... 连主机和路径一起抹掉。
            Regex::new(r"(?i)\b([a-z][a-z0-9+.-]*://[^\s:/@]*):[^\s/?#]+@")
                .expect("url password regex"),
            // 键：成对引号包着（JSON、shell 里转义的 \"）或不带引号。只认成对的，
            // 否则 echo "TOKEN=x" 前面那个属于 echo 的引号也会被吃掉。不带引号的值
            // 不许以 `:` 开头，否则 auth::login、token::Kind 这种 Rust 路径也会被抹掉。
            Regex::new(&format!(
                r#"(?i)(?:\\?["']({NAME})\\?["']|({NAME}))\s*[:=]\s*(?:{QUOTED}|[^\s,;:][^\s,;]*)"#
            ))
            .expect("secret assignment regex"),
            // --token abc 这种空格隔开的长参数。值以 - < > | & 开头就当后面没跟值：
            // --password -u root、--with-token < file 里的是开关。值是网址（第 2 组）
            // 也当开关：curl --anyauth https://x 的网址是要访问的地址，网址里的口令
            // 前面那条已经抹过。短参数（-p、-u）含义因命令而异，不管。
            Regex::new(&format!(
                r#"(?i)(--{NAME})[ \t]+(?:(["']?[a-z][a-z0-9+.-]*://\S*)|{QUOTED}|[^\s,;:<>|&-][^\s,;]*)"#
            ))
            .expect("secret flag regex"),
            Regex::new(r"(?is)-----BEGIN[^\n]*PRIVATE KEY-----.*?-----END[^\n]*PRIVATE KEY-----")
                .expect("private key regex"),
        ]
    });
    let original = value.clone();
    let mut redacted = value.clone();
    redacted = patterns[0]
        .replace_all(&redacted, "${1}[REDACTED]")
        .into_owned();
    redacted = patterns[1]
        .replace_all(&redacted, "${1}:[REDACTED]@")
        .into_owned();
    redacted = patterns[2]
        .replace_all(&redacted, |captures: &regex::Captures<'_>| {
            let name = captures.get(1).or_else(|| captures.get(2));
            format!("{}=[REDACTED]", name.map_or("", |name| name.as_str()))
        })
        .into_owned();
    redacted = patterns[3]
        .replace_all(&redacted, |captures: &regex::Captures<'_>| {
            if captures.get(2).is_some() {
                captures[0].to_string()
            } else {
                format!("{}=[REDACTED]", &captures[1])
            }
        })
        .into_owned();
    redacted = patterns[4]
        .replace_all(&redacted, "[REDACTED]")
        .into_owned();
    *value = redacted;
    *value != original
}

#[cfg(test)]
mod tests {
    use super::{looks_like_secret_name, redact_text};

    /// 2026-10-08 本机守护进程环境里就有 CLAUDE_CODE_MESSAGING_TOKEN（它是从 Claude Code 会话里起的）。
    #[test]
    fn secret_looking_environment_names() {
        for name in [
            "GITHUB_TOKEN",
            "CLAUDE_CODE_MESSAGING_TOKEN",
            "OPENAI_API_KEY",
            "AWS_SECRET_ACCESS_KEY",
            "AWS_SESSION_TOKEN",
            "npm_config__authToken",
            "NPM_CONFIG__AUTH",
            "DB_PASSWORD",
            "MYSQL_PWD",
            "SESSION_COOKIE",
            "SLACK_SIGNING_SECRET",
        ] {
            assert!(looks_like_secret_name(name), "{name} 该算密钥");
        }
        for name in [
            "PATH",
            "HOME",
            "PWD",
            "OLDPWD",
            "SSH_AUTH_SOCK",
            "HTTPS_PROXY",
            "JAVA_HOME",
            "CLAUDE_CODE_MESSAGING_SOCKET",
            "TOKENIZERS_PARALLELISM",
            "AWS_ACCESS_KEY_ID",
        ] {
            assert!(!looks_like_secret_name(name), "{name} 不该算密钥");
        }
    }

    fn redacted(input: &str) -> String {
        let mut text = input.to_string();
        redact_text(&mut text);
        text
    }

    #[test]
    fn env_var_style_names_are_redacted() {
        for (input, expected) in [
            (
                "GITHUB_TOKEN=ghp_abc123 cargo test",
                "GITHUB_TOKEN=[REDACTED] cargo test",
            ),
            ("ADMIN_PASSWORD:hunter2", "ADMIN_PASSWORD=[REDACTED]"),
            (
                "NPM_API_KEY=xyz npm publish",
                "NPM_API_KEY=[REDACTED] npm publish",
            ),
            ("FOO_SECRET=s3cr3t", "FOO_SECRET=[REDACTED]"),
            (
                "AWS_SECRET_ACCESS_KEY=abc",
                "AWS_SECRET_ACCESS_KEY=[REDACTED]",
            ),
            (
                "export BASIC_AUTH=user:pass",
                "export BASIC_AUTH=[REDACTED]",
            ),
            ("PGPASSWORD=x psql", "PGPASSWORD=[REDACTED] psql"),
            ("githubToken=abc", "githubToken=[REDACTED]"),
        ] {
            assert_eq!(redacted(input), expected, "{input}");
        }
    }

    #[test]
    fn previously_handled_forms_still_redacted() {
        for (input, expected) in [
            ("token=abc", "token=[REDACTED]"),
            ("--password=x", "--password=[REDACTED]"),
            ("api key: abc", "api key=[REDACTED]"),
            (
                "Authorization: Bearer abcdef123456",
                "Authorization: Bearer [REDACTED]",
            ),
        ] {
            assert_eq!(redacted(input), expected, "{input}");
        }
        let header = redacted("curl -H \"x-api-key: abc123\" https://example.com");
        assert!(header.contains("x-api-key=[REDACTED]"), "{header}");
        assert!(!header.contains("abc123"), "{header}");
    }

    #[test]
    fn quoted_keys_and_values_are_redacted() {
        for (input, expected) in [
            (r#"{"password": "hunter 2"}"#, "{password=[REDACTED]}"),
            (
                r#"{"user":"bob","api_key":"a,b;c"}"#,
                r#"{"user":"bob",api_key=[REDACTED]}"#,
            ),
            (
                r#"curl -d '{"token":"abc def"}' https://x"#,
                "curl -d '{token=[REDACTED]}' https://x",
            ),
            (
                r#"curl -d "{\"password\":\"hunter2\"}" https://x"#,
                r#"curl -d "{password=[REDACTED]}" https://x"#,
            ),
            ("{'secret': 'x y'}", "{secret=[REDACTED]}"),
            (r#"{"password":"pa\"ss word"}"#, "{password=[REDACTED]}"),
            (
                r#"DB_PASSWORD="p@ss w0rd" ./run"#,
                "DB_PASSWORD=[REDACTED] ./run",
            ),
            ("export TOKEN='a b c'", "export TOKEN=[REDACTED]"),
            // 命令原文先截到 500 字再脱敏，引号可能没闭合：抹到行尾。
            ("TOKEN=\"abc def\nnext", "TOKEN=[REDACTED]\nnext"),
            // 前面那个引号属于 echo 的参数，不是键的，不能吃掉。
            (r#"echo "TOKEN=abc def""#, r#"echo "TOKEN=[REDACTED] def""#),
        ] {
            assert_eq!(redacted(input), expected, "{input}");
        }
    }

    #[test]
    fn long_flags_with_space_separated_values_are_redacted() {
        for (input, expected) in [
            (
                "cargo publish --token abc123 --allow-dirty",
                "cargo publish --token=[REDACTED] --allow-dirty",
            ),
            (
                "gh secret set FOO --github-token ghp_x",
                "gh secret set FOO --github-token=[REDACTED]",
            ),
            (
                r#"mysql --password "a b" -u root"#,
                "mysql --password=[REDACTED] -u root",
            ),
            (
                "curl --cookie 'a=b; c=d' https://x",
                "curl --cookie=[REDACTED] https://x",
            ),
        ] {
            assert_eq!(redacted(input), expected, "{input}");
        }
    }

    #[test]
    fn url_passwords_are_redacted() {
        for (input, expected) in [
            (
                "git clone https://user:ghp_abc@github.com/o/r.git",
                "git clone https://user:[REDACTED]@github.com/o/r.git",
            ),
            // 口令里带没转义的 @：抹到主机前最后一个 @，不漏后半截。
            (
                "DATABASE_URL=postgres://app:p@ss@db:5432/app",
                "DATABASE_URL=postgres://app:[REDACTED]@db:5432/app",
            ),
            (
                "redis-cli -u redis://:pw123@localhost:6379",
                "redis-cli -u redis://:[REDACTED]@localhost:6379",
            ),
            (
                "jdbc:mysql://root:pw@localhost/db",
                "jdbc:mysql://root:[REDACTED]@localhost/db",
            ),
        ] {
            assert_eq!(redacted(input), expected, "{input}");
        }
        let actions = redacted("git clone https://x-access-token:ghs_abc@github.com/o/r");
        assert!(!actions.contains("ghs_abc"), "{actions}");
        assert!(actions.contains("@github.com/o/r"), "{actions}");
        // 参数后面跟的网址会留着，但网址里的口令照样抹。
        assert_eq!(
            redacted("curl --anyauth https://user:pw@example.com"),
            "curl --anyauth https://user:[REDACTED]@example.com"
        );
    }

    #[test]
    fn names_without_secret_assignment_are_untouched() {
        for input in [
            "gh auth login --token-file path/to/file",
            "tool --token-file=path/to/file",
            "TOKEN_FILE=/run/secrets/x",
            "tokenizer=bert",
            "max_tokens=4096",
            "GIT_AUTHOR_NAME=alice",
            "AUTH_URL=https://example.com/login",
            "调用 crate::auth::verify 和 token::Kind",
            r#"{"tokenizer": "bert", "max_tokens": 4096, "author": "alice"}"#,
            r#"{"token_file": "/run/x"}"#,
            // 后面跟的是重定向、下一个参数或什么都没有：这些是开关，不是带值的参数。
            "gh auth login --with-token < token.txt",
            "mysql --password -u root",
            "docker login --password-stdin",
            "tool --password",
            // 值是网址：这些参数是开关，网址是 curl 要访问的地址。
            "curl --anyauth https://example.com/a?b=1",
            r#"curl --proxy-anyauth "http://proxy:8080" https://x"#,
            "curl http://localhost:8080/api?email=a@b.com",
            "git clone ssh://git@github.com/o/r.git",
            "git clone git@github.com:o/r.git",
            "cargo build --release",
        ] {
            let mut text = input.to_string();
            assert!(!redact_text(&mut text), "{input} -> {text}");
            assert_eq!(text, input);
        }
    }

    #[test]
    fn redaction_is_idempotent() {
        for input in [
            "GITHUB_TOKEN=ghp_abc123 ADMIN_PASSWORD:hunter2",
            r#"{"password":"x","token":"y"}"#,
            "tool --token abc https://u:p@h/x https://x-access-token:t@h/y",
        ] {
            let mut text = input.to_string();
            assert!(redact_text(&mut text), "{input}");
            let once = text.clone();
            assert!(!redact_text(&mut text), "{once} -> {text}");
            assert_eq!(text, once);
        }
    }
}
