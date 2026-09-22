//! `gld doctor` —— 配置体检。
//!
//! 输出按归属分组，每个有问题的项下面直接给出该跑的命令。
//! 有任何 ✗ 时退出码为 1，方便 CI / 脚本判断。

use gld_core::app::{Diagnosis, DoctorLevel};
use gld_daemon::Request;

use super::Ctx;
use crate::error::{CliError, CliResult};

pub async fn run(ctx: &mut Ctx) -> CliResult {
    let diagnosis: Diagnosis = ctx.backend.call_typed(Request::Doctor).await?;
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
