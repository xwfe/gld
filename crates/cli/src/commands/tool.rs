//! `gld tool` —— 在命令行里直接调工具内核。
//!
//! 排查“AI 说它读不到文件 / 命令被拒”时，在这里跑一次同样的调用，
//! 看到的结果和 AI 看到的一模一样（同一个上下文、同一个 `call_tool`）。

use gld_daemon::Request;
use serde_json::{Map, Value};

use super::Ctx;
use crate::cli::ToolCmd;
use crate::error::{CliError, CliResult};

pub async fn run(ctx: &mut Ctx, command: ToolCmd) -> CliResult {
    match command {
        ToolCmd::List => list(ctx).await,
        ToolCmd::Schema { name } => schema(ctx, &name).await,
        ToolCmd::Call {
            name,
            args,
            args_json,
        } => call(ctx, &name, &args, args_json.as_deref()).await,
    }
}

async fn list(ctx: &mut Ctx) -> CliResult {
    let tools: Vec<Value> = ctx
        .backend
        .call_typed(Request::ListTools {
            target: ctx.target.clone(),
        })
        .await?;
    if ctx.out.json_or(&tools) {
        return Ok(());
    }
    let rows: Vec<Vec<String>> = tools
        .iter()
        .map(|tool| {
            vec![
                field(tool, "name"),
                truncate(&field(tool, "description"), 78),
            ]
        })
        .collect();
    ctx.out.table(&["工具", "说明"], &rows);
    ctx.out.line("");
    ctx.out.line(
        ctx.out
            .dim("参数：gld tool schema <工具名>；调用：gld tool call <工具名> key=value"),
    );
    Ok(())
}

async fn schema(ctx: &mut Ctx, name: &str) -> CliResult {
    let tools: Vec<Value> = ctx
        .backend
        .call_typed(Request::ListTools {
            target: ctx.target.clone(),
        })
        .await?;
    let found = tools
        .iter()
        .find(|tool| field(tool, "name") == name)
        .ok_or_else(|| {
            CliError::new(format!(
                "当前工具集里没有「{name}」。`gld tool list` 查看可用工具。"
            ))
        })?;
    println!("{}", serde_json::to_string_pretty(found)?);
    Ok(())
}

async fn call(ctx: &mut Ctx, name: &str, args: &[String], args_json: Option<&str>) -> CliResult {
    let mut object = parse_args(args)?;
    if let Some(raw) = args_json {
        let extra: Value = serde_json::from_str(raw)
            .map_err(|error| CliError::new(format!("--args-json 不是合法 JSON：{error}")))?;
        let Value::Object(extra) = extra else {
            return Err(CliError::new("--args-json 必须是一个 JSON 对象"));
        };
        object.extend(extra);
    }

    let result: Value = ctx
        .backend
        .call_typed(Request::CallTool {
            target: ctx.target.clone(),
            name: name.to_string(),
            args: Value::Object(object),
        })
        .await?;

    println!("{}", serde_json::to_string_pretty(&result)?);
    // 工具用 ok=false 表示业务失败（权限、路径越界、命令被拒），
    // 这不是命令行本身出错，所以只用退出码表达，不再打印一遍错误。
    if result.get("ok").and_then(Value::as_bool) == Some(false) {
        return Err(CliError::new(""));
    }
    Ok(())
}

/// 解析 `key=value` / `key:=json` / `key=@文件` 三种写法。
fn parse_args(args: &[String]) -> CliResult<Map<String, Value>> {
    let mut object = Map::new();
    for raw in args {
        if let Some((key, json)) = raw.split_once(":=") {
            let value = serde_json::from_str(json).map_err(|error| {
                CliError::new(format!("{key} 的值不是合法 JSON：{error}（{json}）"))
            })?;
            object.insert(key.trim().to_string(), value);
            continue;
        }
        let Some((key, value)) = raw.split_once('=') else {
            return Err(CliError::new(format!(
                "参数要写成 key=value、key:=json 或 key=@文件，收到「{raw}」"
            )));
        };
        let key = key.trim().to_string();
        if let Some(path) = value.strip_prefix('@') {
            let content = std::fs::read_to_string(path)
                .map_err(|error| CliError::new(format!("读取 {path} 失败：{error}")))?;
            object.insert(key, Value::String(content));
            continue;
        }
        object.insert(key, infer(value));
    }
    Ok(object)
}

/// 看起来像 JSON 标量或容器就按 JSON 解析，否则当字符串。
///
/// 这样 `confirm=true`、`limit=100` 直接可用，而 `path=src/main.rs` 不会被误解析。
fn infer(value: &str) -> Value {
    let trimmed = value.trim();
    let looks_like_json = matches!(trimmed, "true" | "false" | "null")
        || trimmed.starts_with('[')
        || trimmed.starts_with('{')
        || trimmed.parse::<f64>().is_ok();
    if looks_like_json {
        if let Ok(parsed) = serde_json::from_str(trimmed) {
            return parsed;
        }
    }
    Value::String(value.to_string())
}

fn field(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn truncate(text: &str, max_chars: usize) -> String {
    let flat = text.replace('\n', " ");
    if flat.chars().count() <= max_chars {
        return flat;
    }
    flat.chars().take(max_chars - 1).collect::<String>() + "…"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(items: &[&str]) -> Map<String, Value> {
        parse_args(&items.iter().map(|s| s.to_string()).collect::<Vec<_>>()).expect("parse")
    }

    #[test]
    fn strings_stay_strings_and_scalars_are_inferred() {
        let parsed = args(&["path=src/main.rs", "confirm=true", "limit=100", "q=1.2.3"]);
        assert_eq!(parsed["path"], Value::String("src/main.rs".into()));
        assert_eq!(parsed["confirm"], Value::Bool(true));
        assert_eq!(parsed["limit"], serde_json::json!(100));
        // 1.2.3 不是合法数字，保持字符串。
        assert_eq!(parsed["q"], Value::String("1.2.3".into()));
    }

    #[test]
    fn explicit_json_form_wins_over_inference() {
        let parsed = args(&[r#"paths:=["a","b"]"#, "n:=42"]);
        assert_eq!(parsed["paths"], serde_json::json!(["a", "b"]));
        assert_eq!(parsed["n"], serde_json::json!(42));
        assert!(parse_args(&["bad:=not json".to_string()]).is_err());
    }

    #[test]
    fn at_prefix_reads_a_file() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("patch.txt");
        std::fs::write(&path, "hello\nworld\n").unwrap();
        let parsed = args(&[&format!("patch=@{}", path.display())]);
        assert_eq!(parsed["patch"], Value::String("hello\nworld\n".into()));
        assert!(parse_args(&["p=@/definitely/missing".to_string()]).is_err());
    }

    #[test]
    fn a_bare_word_is_rejected_with_the_accepted_forms() {
        let error = parse_args(&["oops".to_string()]).unwrap_err();
        assert!(error.message.contains("key=value"), "{}", error.message);
    }
}
