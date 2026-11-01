//! `gld doctor` —— 配置体检。
//!
//! 输出按归属分组，每个有问题的项下面直接给出该跑的命令。
//! 有任何 ✗ 时退出码为 1，方便 CI / 脚本判断。

use gld_core::app::{
    DesktopStatus, Diagnosis, DoctorCheck, DoctorLevel, DESKTOP_SCOPE, SERVICE_SCOPE,
};
use gld_core::health::HealthItem;
use gld_daemon::Request;

use super::Ctx;
use crate::error::{CliError, CliResult};
use crate::output::Output;

pub async fn run(ctx: &mut Ctx, probe: bool, desktop: bool) -> CliResult {
    let mut diagnosis: Diagnosis = ctx.backend.call_typed(Request::Doctor).await?;
    // 项目放行了 screencapture 时守护进程已经查过了。
    if desktop
        && !diagnosis
            .checks
            .iter()
            .any(|check| check.scope == DESKTOP_SCOPE)
    {
        // 不自动拉起守护进程：从这个终端拉起的，会话类型跟着这个终端，查出来的可能不是平时那个的结果。
        diagnosis.checks.push(if ctx.backend.is_remote() {
            let status: DesktopStatus = ctx
                .backend
                .call_typed(Request::DesktopProbe {
                    only_if_used: false,
                })
                .await?;
            status.check
        } else {
            DoctorCheck {
                scope: DESKTOP_SCOPE.to_string(),
                label: "截屏".to_string(),
                level: DoctorLevel::Warn,
                detail: "守护进程没在跑，查不了：AI 的命令能不能截屏要看守护进程".to_string(),
                fix: "gld daemon start".to_string(),
            }
        });
    }
    if probe {
        // 和 `gld health` 同一个请求：探活的判断只有一处实现。体检这边只是把
        // 结果并进同一张表，好让"配置对不对"和"此刻通不通"一起看、一起算退出码。
        let items: Vec<HealthItem> = ctx.backend.call_typed(Request::HubHealth).await?;
        diagnosis
            .checks
            .extend(items.into_iter().map(|item| DoctorCheck {
                scope: SERVICE_SCOPE.to_string(),
                label: format!("探活 {}", item.label),
                level: if item.ok {
                    DoctorLevel::Ok
                } else {
                    DoctorLevel::Fail
                },
                detail: item.detail,
                fix: if item.ok { String::new() } else { item.hint },
            }));
    }
    if probe {
        return report(ctx.out, &diagnosis);
    }
    if ctx.out.json_or(&diagnosis) {
        return exit_status(&diagnosis);
    }
    print_checks(ctx.out, &diagnosis);
    ctx.out.line("");
    // 体检不发网络请求，所以"公网地址此刻通不通"它答不了——尤其是自建反代
    // 那种 gld 看不到的链路。把这句话放在这里，而不是等用户去翻文档。
    ctx.out
        .line(ctx.out.dim("公网那一头此刻通不通：gld doctor --probe"));
    summary(ctx.out, &diagnosis)
}

/// 输出一份体检结果；有 ✗ 时退出码为 1。`gld node doctor` 也走这里。
pub fn report(out: Output, diagnosis: &Diagnosis) -> CliResult {
    if out.json_or(diagnosis) {
        return exit_status(diagnosis);
    }
    print_checks(out, diagnosis);
    out.line("");
    summary(out, diagnosis)
}

fn exit_status(diagnosis: &Diagnosis) -> CliResult {
    if diagnosis.is_healthy() {
        Ok(())
    } else {
        Err(CliError::new(""))
    }
}

fn print_checks(out: Output, diagnosis: &Diagnosis) {
    // 同一个归属的检查来自好几轮（配置一轮、端口一轮），按第一次出现的顺序归到一起，
    // 不然"MCP 服务"会在输出里出现两段。
    let mut scopes: Vec<&str> = Vec::new();
    for check in &diagnosis.checks {
        if !scopes.contains(&check.scope.as_str()) {
            scopes.push(&check.scope);
        }
    }
    let mut ordered = Vec::with_capacity(diagnosis.checks.len());
    for scope in &scopes {
        ordered.extend(
            diagnosis
                .checks
                .iter()
                .filter(|check| check.scope == *scope),
        );
    }
    let mut current_scope = String::new();
    for check in ordered {
        if check.scope != current_scope {
            if !current_scope.is_empty() {
                out.line("");
            }
            out.line(out.bold(&check.scope));
            current_scope = check.scope.clone();
        }
        let mark = match check.level {
            DoctorLevel::Ok => out.green("✓"),
            DoctorLevel::Warn => out.yellow("!"),
            DoctorLevel::Fail => out.red("✗"),
        };
        out.line(format!("  {mark} {}  {}", check.label, check.detail));
        if !check.fix.is_empty() {
            out.line(format!("      {}", out.dim(&check.fix)));
        }
    }
}

fn summary(out: Output, diagnosis: &Diagnosis) -> CliResult {
    let failures = diagnosis.count(DoctorLevel::Fail);
    let warnings = diagnosis.count(DoctorLevel::Warn);
    if failures == 0 && warnings == 0 {
        out.line(out.green("全部正常。"));
        return Ok(());
    }
    out.line(format!(
        "{} 项需要处理，{} 项提醒。",
        if failures > 0 {
            out.red(&failures.to_string())
        } else {
            "0".into()
        },
        warnings
    ));
    if failures == 0 {
        return Ok(());
    }
    Err(CliError::new(""))
}
