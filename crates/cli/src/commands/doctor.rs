//! `gld doctor` —— 配置体检。
//!
//! 输出按归属分组，每个有问题的项下面直接给出该跑的命令。
//! 有任何 ✗ 时退出码为 1，方便 CI / 脚本判断。

use gld_core::app::{Diagnosis, DoctorCheck, DoctorLevel, SERVICE_SCOPE};
use gld_core::health::HealthItem;
use gld_daemon::Request;

use super::Ctx;
use crate::error::{CliError, CliResult};

pub async fn run(ctx: &mut Ctx, probe: bool) -> CliResult {
    let mut diagnosis: Diagnosis = ctx.backend.call_typed(Request::Doctor).await?;
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
    if ctx.out.json_or(&diagnosis) {
        return if diagnosis.is_healthy() {
            Ok(())
        } else {
            Err(CliError::new(""))
        };
    }

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
                ctx.out.line("");
            }
            ctx.out.line(ctx.out.bold(&check.scope));
            current_scope = check.scope.clone();
        }
        let mark = match check.level {
            DoctorLevel::Ok => ctx.out.green("✓"),
            DoctorLevel::Warn => ctx.out.yellow("!"),
            DoctorLevel::Fail => ctx.out.red("✗"),
        };
        ctx.out
            .line(format!("  {mark} {}  {}", check.label, check.detail));
        if !check.fix.is_empty() {
            ctx.out.line(format!("      {}", ctx.out.dim(&check.fix)));
        }
    }

    let failures = diagnosis.count(DoctorLevel::Fail);
    let warnings = diagnosis.count(DoctorLevel::Warn);
    ctx.out.line("");
    if !probe {
        // 体检不发网络请求，所以"公网地址此刻通不通"它答不了——尤其是自建反代
        // 那种 gld 看不到的链路。把这句话放在这里，而不是等用户去翻文档。
        ctx.out
            .line(ctx.out.dim("公网那一头此刻通不通：gld doctor --probe"));
    }
    if failures == 0 && warnings == 0 {
        ctx.out.line(ctx.out.green("全部正常。"));
        return Ok(());
    }
    ctx.out.line(format!(
        "{} 项需要处理，{} 项提醒。",
        if failures > 0 {
            ctx.out.red(&failures.to_string())
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
