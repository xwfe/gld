use std::sync::OnceLock;

use regex::Regex;
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::model::{CheckpointRecord, InitialInputRecord};

const CHECKPOINT_HEADING: &str = "## 本轮检查点";
const INITIAL_INPUT_HEADING: &str = "## 首次用户输入";

pub fn metadata(content: &str, label: &str) -> Option<String> {
    let prefix = format!("**{label}:**");
    content.lines().find_map(|line| {
        line.trim()
            .strip_prefix(&prefix)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    })
}

pub fn document_title(content: &str, number: u64) -> String {
    content
        .lines()
        .find_map(|line| line.trim().strip_prefix("# "))
        .and_then(|line| line.split_once('：').map(|(_, title)| title.trim()))
        .filter(|title| !title.is_empty())
        .unwrap_or("开发会话")
        .to_string()
        .replace(&format!("会话 {number}"), "")
        .trim_matches(['：', ':', ' '])
        .to_string()
}

pub fn render_document(
    number: u64,
    title: &str,
    session_key: &str,
    created_at: &str,
    initial_input: Option<&InitialInputRecord>,
) -> String {
    let title = if title.trim().is_empty() {
        "开发会话"
    } else {
        title.trim()
    };
    let mut output = format!(
        "# 会话 {number}：{title}\n\n\
**Session key:** {session_key}\n\
**Created:** {created_at}\n\
**Updated:** {created_at}\n\
**Status:** active\n\n\
{INITIAL_INPUT_HEADING}\n\n"
    );
    if let Some(input) = initial_input {
        push_json_block(&mut output, "initial-input revision-1", input);
    } else {
        output.push_str("未提供首次用户输入；服务端无法读取未作为工具参数传入的聊天内容。\n\n");
    }
    output.push_str(CHECKPOINT_HEADING);
    output.push_str("\n\n");
    output
}

pub fn parse_initial_input_records(content: &str) -> Vec<InitialInputRecord> {
    section_body(content, INITIAL_INPUT_HEADING)
        .map(parse_json_blocks)
        .unwrap_or_default()
}

pub fn append_initial_input_revision(content: &str, record: &InitialInputRecord) -> String {
    let mut updated = content.to_string();
    let block = json_block(
        &format!("initial-input revision-{}", record.revision),
        record,
    );
    if let Some(start) = updated.find(INITIAL_INPUT_HEADING) {
        let tail = &updated[start + INITIAL_INPUT_HEADING.len()..];
        let end = tail
            .find("\n## ")
            .map(|offset| start + INITIAL_INPUT_HEADING.len() + offset);
        let insert_at = end.unwrap_or(updated.len());
        updated.insert_str(insert_at, &format!("\n\n{block}"));
    } else if let Some(checkpoint_start) = updated.find(CHECKPOINT_HEADING) {
        updated.insert_str(
            checkpoint_start,
            &format!("{INITIAL_INPUT_HEADING}\n\n{block}\n\n"),
        );
    } else {
        updated.push_str(&format!("\n{INITIAL_INPUT_HEADING}\n\n{block}\n"));
    }
    updated
}

pub fn parse_checkpoint_records(content: &str) -> Vec<CheckpointRecord> {
    section_body(content, CHECKPOINT_HEADING)
        .map(parse_json_blocks)
        .unwrap_or_default()
}

pub fn append_checkpoint_record(content: &str, record: &CheckpointRecord) -> String {
    let mut updated = content.to_string();
    if !updated.contains(CHECKPOINT_HEADING) {
        if !updated.ends_with('\n') {
            updated.push('\n');
        }
        updated.push_str(&format!("\n{CHECKPOINT_HEADING}\n"));
    }
    if !updated.ends_with('\n') {
        updated.push('\n');
    }
    push_json_block(
        &mut updated,
        &format!("{} revision-{}", record.turn_id, record.revision),
        record,
    );
    updated
}

pub fn with_updated_at(content: &str, timestamp: &str) -> String {
    let prefix = "**Updated:**";
    let Some(start) = content.find(prefix) else {
        return content.to_string();
    };
    let tail = &content[start..];
    let Some(end) = tail.find('\n') else {
        return content.to_string();
    };
    let mut updated = content.to_string();
    updated.replace_range(start..start + end, &format!("{prefix} {timestamp}"));
    updated
}

fn section_body<'a>(content: &'a str, heading: &str) -> Option<&'a str> {
    let start = content.find(heading)? + heading.len();
    let tail = &content[start..];
    let end = tail.find("\n## ").unwrap_or(tail.len());
    Some(tail[..end].trim())
}

fn parse_json_blocks<T>(content: &str) -> Vec<T>
where
    T: serde::de::DeserializeOwned,
{
    let mut records = Vec::new();
    let mut remaining = content;
    while let Some(fence_start) = remaining.find("```json\n") {
        let json_start = fence_start + "```json\n".len();
        let Some(fence_end) = remaining[json_start..].find("\n```") else {
            break;
        };
        let json_text = &remaining[json_start..json_start + fence_end];
        if let Ok(record) = serde_json::from_str::<T>(json_text) {
            records.push(record);
        }
        remaining = &remaining[json_start + fence_end + "\n```".len()..];
    }
    records
}

fn json_block<T: serde::Serialize>(heading: &str, value: &T) -> String {
    format!(
        "### {heading}\n\n```json\n{}\n```\n",
        serde_json::to_string_pretty(value).expect("history record is serializable")
    )
}

fn push_json_block<T: serde::Serialize>(output: &mut String, heading: &str, value: &T) {
    output.push_str(&json_block(heading, value));
    output.push('\n');
}

pub fn checkpoint_from_args(
    args: &Value,
    default_timestamp: &str,
) -> Result<CheckpointRecord, String> {
    let explicit_turn_id = args
        .get("turn_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let explicit_timestamp = string_field(args, "timestamp");
    let mut record = CheckpointRecord {
        turn_id: explicit_turn_id.unwrap_or_default().to_string(),
        timestamp: explicit_timestamp.clone().unwrap_or_default(),
        user_intent: string_field(args, "user_intent").unwrap_or_default(),
        raw_user_input: string_field(args, "raw_user_input").unwrap_or_default(),
        findings: string_array(args, "findings")?,
        decisions: string_array(args, "decisions")?,
        files_changed: string_array(args, "files_changed")?,
        tests: string_array(args, "tests")?,
        runtime_state: string_array(args, "runtime_state")?,
        remaining_issues: string_array(args, "remaining_issues")?,
        next_actions: string_array(args, "next_actions")?,
        notes: string_field(args, "notes").unwrap_or_default(),
        ..CheckpointRecord::default()
    };
    if record.turn_id.is_empty() {
        record.turn_id = automatic_turn_id(&record);
    }
    record.timestamp = explicit_timestamp.unwrap_or_else(|| default_timestamp.to_string());
    Ok(record)
}

pub fn checkpoint_fingerprint(record: &CheckpointRecord) -> String {
    let mut canonical = record.clone();
    canonical.timestamp.clear();
    canonical.revision = 0;
    canonical.supersedes = None;
    canonical.content_hash.clear();
    let encoded = serde_json::to_vec(&canonical).expect("checkpoint record is serializable");
    format!("{:x}", Sha256::digest(encoded))
}

pub fn initial_input_fingerprint(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

fn automatic_turn_id(record: &CheckpointRecord) -> String {
    let hash = checkpoint_fingerprint(record);
    format!("auto-{}", &hash[..16])
}

fn string_field(args: &Value, name: &str) -> Option<String> {
    args.get(name).and_then(Value::as_str).map(str::to_string)
}

fn string_array(args: &Value, name: &str) -> Result<Vec<String>, String> {
    let Some(value) = args.get(name) else {
        return Ok(Vec::new());
    };
    let array = value
        .as_array()
        .ok_or_else(|| format!("{name} must be an array of strings"))?;
    array
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::to_string)
                .ok_or_else(|| format!("{name} must contain only strings"))
        })
        .collect()
}

pub fn redact_record(record: &mut CheckpointRecord) -> bool {
    let mut changed = redact_text(&mut record.timestamp);
    changed |= redact_text(&mut record.user_intent);
    changed |= redact_text(&mut record.raw_user_input);
    changed |= redact_text(&mut record.notes);
    for values in [
        &mut record.findings,
        &mut record.decisions,
        &mut record.files_changed,
        &mut record.tests,
        &mut record.runtime_state,
        &mut record.remaining_issues,
        &mut record.next_actions,
    ] {
        for value in values {
            changed |= redact_text(value);
        }
    }
    changed
}

pub fn redact_text(value: &mut String) -> bool {
    // 名字取「以关键词结尾的整个词」而不是 \b 起头：`_` 是单词字符，\b 下
    // GITHUB_TOKEN=、ADMIN_PASSWORD: 这类最常见的环境变量写法整条漏掉。
    const NAME: &str = r"[A-Za-z0-9_-]*(?:(?:api|access|secret)[_ -]?key|token|secret|cookie|password|passwd|pwd|auth)";
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
            // --password -u root、--with-token < file 里的是开关。短参数（-p、-u）
            // 含义因命令而异，不管。
            Regex::new(&format!(
                r#"(?i)(--{NAME})[ \t]+(?:{QUOTED}|[^\s,;:<>|&-][^\s,;]*)"#
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
        .replace_all(&redacted, "${1}=[REDACTED]")
        .into_owned();
    redacted = patterns[4]
        .replace_all(&redacted, "[REDACTED]")
        .into_owned();
    *value = redacted;
    *value != original
}

#[cfg(test)]
mod tests {
    use super::redact_text;

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
