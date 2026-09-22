use gld_core::agent_context::{AgentContextSnapshot, GlobalAgentContextScan};
use gld_core::health::HealthItem;
use gld_core::usage::ServiceUsageStats;
use gld_daemon::Request;
use serde_json::Value;

use super::Ctx;
use crate::cli::ContextArgs;
use crate::error::{CliError, CliResult};
use crate::output::human_time;

pub async fn health(ctx: &mut Ctx) -> CliResult {
    let items: Vec<HealthItem> = ctx
        .backend
        .call_typed(Request::Health {
            target: ctx.target.clone(),
        })
        .await?;
    if ctx.out.json_or(&items) {
        return Ok(());
    }
    for item in &items {
        ctx.out.line(format!(
            "{} {:<26} {}",
            ctx.out.ok_mark(item.ok),
            item.label,
            item.detail
        ));
        if !item.ok && !item.hint.is_empty() {
            ctx.out.line(format!("  {}", ctx.out.dim(&item.hint)));
        }
    }
    let failed = items.iter().filter(|item| !item.ok).count();
    ctx.out.line("");
    ctx.out.line(
        ctx.out
            .dim("没配置的入口（例如没开 Actions）显示失败是正常的；只看你实际使用的那几项。"),
    );
    if failed == items.len() {
        return Err(CliError::new(
            "所有检查项均失败：服务大概率没有启动，先执行 gld start。",
        ));
    }
    Ok(())
}

pub async fn history(ctx: &mut Ctx) -> CliResult {
    let value: Value = ctx
        .backend
        .call_typed(Request::HistorySessions {
            target: ctx.target.clone(),
        })
        .await?;
    if ctx.out.json_or(&value) {
        return Ok(());
    }
    let sessions = value
        .get("sessions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if sessions.is_empty() {
        ctx.out.line("还没有历史会话。AI 通过 history_manage / history_session_checkpoint 写入后会出现在 docs/history-session/。");
        return Ok(());
    }
    let rows: Vec<Vec<String>> = sessions
        .iter()
        .map(|s| {
            vec![
                pick(s, &["number", "id"]),
                pick(s, &["title", "summary"]),
                human_time(&pick(s, &["updated_at", "created_at"])),
                pick(s, &["path", "file"]),
            ]
        })
        .collect();
    ctx.out.table(&["编号", "标题", "更新时间", "文件"], &rows);
    Ok(())
}

fn pick(value: &Value, keys: &[&str]) -> String {
    for key in keys {
        if let Some(found) = value.get(key) {
            return match found {
                Value::String(s) => s.clone(),
                Value::Null => continue,
                other => other.to_string(),
            };
        }
    }
    "-".into()
}

pub async fn usage(ctx: &mut Ctx) -> CliResult {
    let stats: Vec<ServiceUsageStats> = ctx
        .backend
        .call_typed(Request::Usage {
            target: ctx.target.clone(),
        })
        .await?;
    if ctx.out.json_or(&stats) {
        return Ok(());
    }
    let rows = stats
        .iter()
        .map(|s| {
            vec![
                s.service.clone(),
                s.request_count.to_string(),
                s.tool_call_count.to_string(),
                s.error_count.to_string(),
                s.estimated_input_tokens.to_string(),
                s.estimated_output_tokens.to_string(),
            ]
        })
        .collect::<Vec<_>>();
    ctx.out.table(
        &[
            "服务",
            "请求",
            "工具调用",
            "错误",
            "输入Token≈",
            "输出Token≈",
        ],
        &rows,
    );
    ctx.out.line(ctx.out.dim(
        "Token 按请求 / 响应大小估算（4 字节 ≈ 1 token），不保存请求正文；守护进程重启后清零。",
    ));
    Ok(())
}

pub async fn context(ctx: &mut Ctx, args: ContextArgs) -> CliResult {
    if args.global {
        let scan: GlobalAgentContextScan =
            ctx.backend.call_typed(Request::GlobalAgentContext).await?;
        if ctx.out.json_or(&scan) {
            return Ok(());
        }
        if scan.sources.is_empty() {
            ctx.out
                .line("用户主目录下没有发现任何 IDE / Agent 的全局说明或 Skill。");
            return Ok(());
        }
        for source in &scan.sources {
            ctx.out.line(ctx.out.bold(&source.provider));
            for path in &source.instruction_paths {
                ctx.out.line(format!("  说明  {path}"));
            }
            for path in &source.skill_paths {
                ctx.out.line(format!("  Skill {path}"));
            }
        }
        ctx.out.line("");
        ctx.out.line(format!(
            "可启用：gld settings runtime --instruction-sources {} --skill-sources {}",
            scan.detected_instruction_sources.join(","),
            scan.detected_skill_sources.join(",")
        ));
        return Ok(());
    }
    let snapshot: AgentContextSnapshot = ctx
        .backend
        .call_typed(Request::AgentContext {
            target: ctx.target.clone(),
        })
        .await?;
    if ctx.out.json_or(&snapshot) {
        return Ok(());
    }
    // 扫到 ≠ 会注入。默认的 compact 工具集为了省 token 只留 AGENTS.md 一份，
    // 不分开标的话，人把规则写进 .cursorrules、在这里看到它列着，
    // 就会以为生效了——改了半天没反应也想不到是这儿。
    let injected = |path: &str| {
        snapshot
            .injected_instruction_paths
            .iter()
            .any(|p| p == path)
    };
    let skipped = snapshot.instructions.len() - snapshot.injected_instruction_paths.len();

    ctx.out.line(ctx.out.bold(&format!(
        "说明文件（扫到 {}，实际注入 {}）",
        snapshot.instructions.len(),
        snapshot.injected_instruction_paths.len()
    )));
    for doc in &snapshot.instructions {
        let mark = if injected(&doc.path) {
            ctx.out.green("✓")
        } else {
            ctx.out.dim("·")
        };
        ctx.out.line(format!(
            "  {mark} [{}/{}] {}  {} 字",
            doc.provider,
            doc.scope,
            doc.path,
            doc.content.chars().count()
        ));
    }

    let listed = if snapshot.skills_injected {
        snapshot.skills_listed
    } else {
        0
    };
    let not_taken = if snapshot.skills_skipped.is_empty() {
        String::new()
    } else {
        format!("，没收进来 {}", snapshot.skills_skipped.len())
    };
    ctx.out.line(ctx.out.bold(&format!(
        "Skill（扫到 {}，目录里列了 {}{not_taken}）",
        snapshot.skills.len(),
        listed
    )));
    for (index, skill) in snapshot.skills.iter().enumerate() {
        // 打 ✓ 的进了给 AI 的目录。打 · 的不是失效了——`list_skills` 照样
        // 拿得到——只是说明里看不见，模型自己想起它的机会小。
        let mark = if index < listed {
            ctx.out.green("✓")
        } else {
            ctx.out.dim("·")
        };
        ctx.out.line(format!(
            "  {mark} [{}/{}] {}  {}",
            skill.provider, skill.scope, skill.name, skill.path
        ));
    }
    // 扫到了却没收进来的：以前静默跳过，作者只看到 AI 不用他的 skill。
    for skipped in &snapshot.skills_skipped {
        ctx.out.line(format!(
            "  {} [{}/{}] {}  {}",
            ctx.out.red("✗"),
            skipped.provider,
            skipped.scope,
            skipped.path,
            ctx.out.dim(&skipped.reason)
        ));
    }

    if !snapshot.skills_skipped.is_empty() {
        ctx.out.line("");
        ctx.out.line(ctx.out.yellow(
            "打 ✗ 的 SKILL.md 没收进来，原因写在后面。改好之后 list_skills 马上看得到；进说明里的目录要等 AI 下次连上。",
        ));
    }
    if snapshot.instructions.is_empty()
        && snapshot.skills.is_empty()
        && snapshot.skills_skipped.is_empty()
    {
        ctx.out
            .line(ctx.out.dim(
                "没有发现可注入的内容。gld context --global 看看主目录下有哪些来源可以启用。",
            ));
        return Ok(());
    }
    let skills_cut = snapshot.skills.len() > listed;
    if skipped > 0 || skills_cut {
        ctx.out.line("");
        if skipped > 0 {
            ctx.out.line(ctx.out.yellow(
                "打 · 的说明没有注入。默认工具集 compact 为了省 token，说明只留工作区里的 AGENTS.md。",
            ));
        }
        if skills_cut {
            ctx.out.line(ctx.out.yellow(
                "打 · 的 Skill 没进目录：compact 档的目录有字符上限。它们没有失效——AI 调 list_skills 照样拿得到，只是不会自己想起来。",
            ));
        }
        ctx.out
            .line(ctx.out.dim("要全部进去：gld ws set tool-profile=advanced"));
    }
    Ok(())
}
