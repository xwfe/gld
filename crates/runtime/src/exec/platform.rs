//! 当前平台直接启动命令与脚本的既有规则。

#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::path::Path;

use tokio::process::Command;

#[cfg(windows)]
fn windows_hidden_creation_flags() -> u32 {
    // Match frpc/cloudflared: hide console-subsystem children (python/cmd/powershell)
    // so remote exec_command does not flash a console or steal focus.
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x00000200;
    const CREATE_NO_WINDOW: u32 = 0x08000000;
    CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW
}

pub(super) fn command_for_program(program: &str, args: &[String]) -> Command {
    #[cfg(windows)]
    {
        let extension = Path::new(program)
            .extension()
            .and_then(|value| value.to_str())
            .map(str::to_ascii_lowercase);
        match extension.as_deref() {
            Some("bat") | Some("cmd") => {
                let mut command = Command::new("cmd.exe");
                command.args(["/d", "/s", "/c"]);
                command
                    .as_std_mut()
                    .raw_arg(windows_batch_command_line(program, args));
                command.creation_flags(windows_hidden_creation_flags());
                return command;
            }
            Some("ps1") => {
                let shell = which::which("pwsh")
                    .or_else(|_| which::which("powershell"))
                    .unwrap_or_else(|_| std::path::PathBuf::from("powershell.exe"));
                let mut command = Command::new(shell);
                command
                    .args([
                        "-NoLogo",
                        "-NoProfile",
                        "-NonInteractive",
                        "-ExecutionPolicy",
                        "Bypass",
                        "-File",
                        windows_command_path(program).as_str(),
                    ])
                    .args(args);
                command.creation_flags(windows_hidden_creation_flags());
                return command;
            }
            _ => {}
        }
    }

    let mut command = Command::new(program);
    command.args(args);
    #[cfg(windows)]
    command.creation_flags(windows_hidden_creation_flags());
    command
}

#[cfg(windows)]
fn windows_batch_command_line(program: &str, args: &[String]) -> String {
    let mut command_line = String::from("call ");
    command_line.push_str(&windows_batch_token(&windows_command_path(program)));
    for arg in args {
        command_line.push(' ');
        command_line.push_str(&windows_batch_token(arg));
    }
    command_line
}

#[cfg(windows)]
fn windows_batch_token(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

pub(super) fn platform_command_path(path: &Path) -> std::path::PathBuf {
    #[cfg(windows)]
    {
        std::path::PathBuf::from(windows_command_path(&path.to_string_lossy()))
    }
    #[cfg(not(windows))]
    path.to_path_buf()
}

#[cfg(windows)]
fn windows_command_path(path: &str) -> String {
    path.strip_prefix("\\\\?\\").unwrap_or(path).to_string()
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn windows_hidden_creation_flags_match_frpc_no_window_pattern() {
        assert_eq!(
            windows_hidden_creation_flags(),
            0x0000_0200 | 0x0800_0000,
            "must keep CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_scripts_use_their_platform_runners() {
        let batch = command_for_program("C:/workspace/run-anything.cmd", &[]);
        assert_eq!(batch.as_std().get_program().to_string_lossy(), "cmd.exe");
        assert!(batch.as_std().get_args().any(|arg| arg == "/c"));
        assert_eq!(
            windows_batch_command_line(
                r"\\?\C:\workspace\Life Brain\run & tooling.cmd",
                &["argument & value".to_string()]
            ),
            r#"call "C:\workspace\Life Brain\run & tooling.cmd" "argument & value""#
        );

        let script = command_for_program("C:/workspace/run-anything.ps1", &[]);
        let runner = script
            .as_std()
            .get_program()
            .to_string_lossy()
            .to_ascii_lowercase();
        assert!(runner.contains("powershell") || runner.contains("pwsh"));
        assert!(script.as_std().get_args().any(|arg| arg == "-File"));

        // Ensure console-subsystem programs (python.exe) also go through the
        // hidden-window flag path; Command does not expose creation_flags for
        // direct assertion, so this only verifies construction still succeeds.
        let python =
            command_for_program("C:/Python312/python.exe", &["-c".into(), "print(1)".into()]);
        assert_eq!(
            python.as_std().get_program().to_string_lossy(),
            "C:/Python312/python.exe"
        );
    }
}
