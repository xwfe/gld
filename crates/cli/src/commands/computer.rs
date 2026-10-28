use crate::cli::ComputerCmd;
use crate::error::{CliError, CliResult};
use crate::output::Output;
use gld_core::computer::probe::{self, Options};

pub async fn run(command: &ComputerCmd, out: Output) -> CliResult {
    let ComputerCmd::Probe {
        binary,
        connect,
        sha256,
        tools_sha256,
        timeout_ms,
    } = command;
    let options = Options {
        binary: binary.clone(),
        connect: *connect,
        expected_sha256: sha256.clone(),
        expected_tools_sha256: tools_sha256.clone(),
        timeout_ms: *timeout_ms,
    };
    let report = tokio::task::spawn_blocking(move || probe::run(&options))
        .await
        .map_err(|_| CliError::new("执行器预检线程异常退出；未将其视为桌面可用"))?;
    if !out.json_or(&report) {
        out.line(if report.ok {
            "预检完成。"
        } else {
            "预检未通过。"
        });
        if let Some(binary) = &report.binary {
            out.kv(&[
                ("可执行文件", binary.path.display().to_string()),
                ("SHA-256", binary.sha256.clone()),
            ]);
        }
        if let Some(protocol) = &report.protocol {
            out.kv(&[
                ("MCP", protocol.version.clone()),
                ("工具数", protocol.tools.len().to_string()),
                ("工具目录指纹", protocol.tools_sha256.clone()),
            ]);
        }
        out.line("本次不验证桌面权限、截图、输入或停止；二进制摘要不能代替来源与签名核验。");
        if let Some(path) = &report.retained_state_directory {
            out.note(format!("预检目录未清理：{}", path.display()));
        }
    }
    match report.failure {
        Some(failure) => Err(CliError::new(format!(
            "{}: {}",
            failure.code, failure.message
        ))),
        None => Ok(()),
    }
}
