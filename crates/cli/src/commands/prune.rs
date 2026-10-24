//! `gld prune`：先列清单，确认后让守护进程删清单里这次还扫得出来的那些。
//!
//! 确认之后把整份清单发回去，而不是只发一句"删吧"：看清单和按回车之间可能有目录被加回来、
//! 有命令刚起，守护进程只删两次都扫得出来的，用户没看到过的一样都不删。

use gld_core::app::{Leftover, PruneReport};
use gld_daemon::Request;

use super::workspace::confirm;
use super::Ctx;
use crate::cli::PruneArgs;
use crate::error::{CliError, CliResult};

pub async fn run(ctx: &mut Ctx, args: PruneArgs) -> CliResult {
    let plan: PruneReport = ctx
        .backend
        .call_typed(Request::Prune { confirmed: None })
        .await?;
    if plan.missing_projects.is_empty() && plan.leftovers.is_empty() {
        if !ctx.out.json_or(&plan) {
            ctx.out
                .line("没有要清的：登记的项目目录都在，数据目录里也没有已删项目的残留。");
            print_busy(ctx, &plan.busy);
        }
        return Ok(());
    }
    if args.dry_run {
        if !ctx.out.json_or(&plan) {
            print_plan(ctx, &plan);
            ctx.out.line("（--dry-run，什么都没删。）");
        }
        return Ok(());
    }
    if !args.yes {
        if ctx.out.json {
            return Err(CliError::new(
                "--json 下不会问确认：先 gld prune --dry-run --json 看清单，确定了再加 -y。",
            ));
        }
        print_plan(ctx, &plan);
        if !confirm("删掉上面这些？项目文件不动，删掉的配置和记录找不回来。")?
        {
            ctx.out.line("已取消。");
            return Ok(());
        }
    }

    let done: PruneReport = ctx
        .backend
        .call_typed(Request::Prune {
            confirmed: Some(plan),
        })
        .await?;
    if !ctx.out.json_or(&done) {
        let bytes: u64 = done.leftovers.iter().map(|leftover| leftover.bytes).sum();
        ctx.out.line(format!(
            "删了 {} 个项目、{} 项残留，腾出 {}。",
            done.missing_projects.len(),
            done.leftovers.len(),
            human_bytes(bytes)
        ));
        for failure in &done.failures {
            ctx.out.line(format!(
                "{} {}：{}",
                ctx.out.red("✗"),
                tilde(&failure.target),
                failure.error
            ));
        }
    }
    if !done.failures.is_empty() {
        return Err(CliError::new(format!(
            "有 {} 项没删掉，原因在上面；修好后再跑一次 gld prune。",
            done.failures.len()
        )));
    }
    Ok(())
}

fn print_plan(ctx: &Ctx, plan: &PruneReport) {
    let out = ctx.out;
    if !plan.missing_projects.is_empty() {
        out.line(out.bold(&format!(
            "目录不在了的项目（{}）：从服务里删掉，和 gld rm 一样",
            plan.missing_projects.len()
        )));
        let rows: Vec<Vec<String>> = plan
            .missing_projects
            .iter()
            .map(|project| {
                vec![
                    project.name.clone(),
                    gld_core::short_id(&project.id).to_string(),
                    project.path.clone(),
                ]
            })
            .collect();
        out.table(&["名称", "ID", "登记的路径"], &rows);
        out.line(out.dim(
            "只是搬了目录、还要它：先 gld upgrade <项目> --path <新目录>。外接盘没插上时，盘上的项目也会列在这里。",
        ));
        out.line("");
    }
    if !plan.leftovers.is_empty() {
        let bytes: u64 = plan.leftovers.iter().map(|leftover| leftover.bytes).sum();
        out.line(out.bold(&format!(
            "对不上任何留下的项目的数据（{} 项，共 {}）",
            plan.leftovers.len(),
            human_bytes(bytes)
        )));
        out.table(
            &["是什么", "位置", "大小", "属于"],
            &leftover_rows(plan, &plan.leftovers),
        );
        out.line("");
    }
    print_busy(ctx, &plan.busy);
}

fn print_busy(ctx: &Ctx, busy: &[Leftover]) {
    if busy.is_empty() {
        return;
    }
    let out = ctx.out;
    out.line(out.yellow(&format!(
        "还有命令或 Agent 在跑、这次不动的（{} 项）：",
        busy.len()
    )));
    for leftover in busy {
        out.line(format!(
            "  {}  {}",
            kind_label(&leftover.kind),
            tilde(&leftover.path)
        ));
    }
}

fn leftover_rows(plan: &PruneReport, leftovers: &[Leftover]) -> Vec<Vec<String>> {
    leftovers
        .iter()
        .map(|leftover| {
            let owner = plan
                .missing_projects
                .iter()
                .find(|project| project.id == leftover.project)
                .map(|project| project.name.clone())
                .unwrap_or_else(|| "已删的项目".into());
            vec![
                kind_label(&leftover.kind).into(),
                tilde(&leftover.path),
                human_bytes(leftover.bytes),
                owner,
            ]
        })
        .collect()
}

fn kind_label(kind: &str) -> &'static str {
    match kind {
        "logs" => "日志",
        "oauth" => "OAuth 客户端注册",
        "harness" => "任务记录",
        "runs" => "命令运行记录",
        "agents" => "委派记录",
        _ => "其他",
    }
}

/// 主目录换成 `~`：路径一般都在 `~/.config/gld` 下，整串打出来又长又暴露用户名。
fn tilde(path: &str) -> String {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
    match home.and_then(|home| {
        std::path::Path::new(path)
            .strip_prefix(&home)
            .ok()
            .map(|rest| format!("~/{}", rest.display()))
    }) {
        Some(short) => short,
        None => path.to_string(),
    }
}

fn human_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    let value = bytes as f64;
    if value < KB {
        format!("{bytes} B")
    } else if value < KB * KB {
        format!("{:.1} KB", value / KB)
    } else if value < KB * KB * KB {
        format!("{:.1} MB", value / KB / KB)
    } else {
        format!("{:.1} GB", value / KB / KB / KB)
    }
}

#[cfg(test)]
mod tests {
    use super::human_bytes;

    #[test]
    fn sizes_read_like_a_person_wrote_them() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(1023), "1023 B");
        assert_eq!(human_bytes(1536), "1.5 KB");
        assert_eq!(human_bytes(5 * 1024 * 1024), "5.0 MB");
        assert_eq!(human_bytes(3 * 1024 * 1024 * 1024), "3.0 GB");
    }
}
