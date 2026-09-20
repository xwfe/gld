//! 一条命令是什么——策略判定和真正执行读的是同一个它。
//!
//! 以前只有 `cmd` 一个入口：一行字符串，先按 shell 的规矩挑掉 `|` `;` `>`
//! 这些操作符，再用 `shell_words` 拆开直接 spawn。**接口长得像 shell，执行
//! 却不是 shell**（审查 C04）。后果是模型想跑
//! `rg "foo|bar"` 这种参数里带管道符的命令，得自己琢磨引号该怎么加，加错了
//! 就被"不允许 shell 串联"拒掉——而那个 `|` 从头到尾都只是 ripgrep 的正则。
//!
//! 现在多一个 `argv`：程序和参数各占一格，谁也不用再猜引号。
//!
//! ```json
//! {"cmd": "cargo test --workspace"}
//! {"argv": ["cargo", "test", "--workspace"]}
//! {"argv": ["rg", "foo|bar", "src"]}
//! ```
//!
//! **两种形式只是入口不同，权限是同一套。**它们都会变成同一个
//! [`CommandSpec`]，后面的白名单、危险命令、联网、受保护路径、程序解析全都
//! 只认它。唯一的区别写在 [`CommandForm`] 上：`argv` 不做 shell 语法检测，
//! 因为它的参数不经过 shell，`|` 和换行就是数据。
//!
//! 两个都给会被拒——不去猜哪个算数，也不维护两套权限逻辑。

use serde_json::Value;

use super::policy::{PolicyError, PolicyReason};

/// 调用方是怎么给这条命令的。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandForm {
    /// `cmd`：一行文本，按 shell 词法拆开。仍然禁止未加引号的操作符——
    /// 写成这个形式的人以为它是 shell，而它不是，放行等于骗人。
    Line,
    /// `argv`：程序 + 参数，逐格给。参数里的引号、换行、`|` 都是数据。
    Argv,
}

/// 一条待判定、待执行的命令。
#[derive(Debug, Clone)]
pub struct CommandSpec {
    /// 拆好的 argv，`parts[0]` 是程序。一定非空。
    pub parts: Vec<String>,
    /// 给人看的一行文本：日志、结果的 `command` 字段、错误消息都用它。
    /// `argv` 形式下由 `shell_words::join` 合成，**不拿它回去执行**。
    pub display: String,
    pub form: CommandForm,
}

impl CommandSpec {
    /// 从工具参数里取命令。`cmd` 与 `argv` 二选一。
    pub fn from_args(args: &Value) -> Result<Self, PolicyError> {
        let cmd = args.get("cmd").filter(|value| !value.is_null());
        let argv = args.get("argv").filter(|value| !value.is_null());
        match (cmd, argv) {
            (Some(_), Some(_)) => Err(PolicyError::new(
                PolicyReason::ConflictingCommandForms,
                "cmd 和 argv 只能给一个",
            )),
            (Some(cmd), None) => {
                let cmd = cmd.as_str().ok_or_else(|| {
                    PolicyError::new(PolicyReason::MissingCommand, "cmd must be a string")
                })?;
                Self::from_line(cmd)
            }
            (None, Some(argv)) => Self::from_argv(argv),
            (None, None) => Err(PolicyError::new(
                PolicyReason::MissingCommand,
                "exec_command requires cmd or argv",
            )),
        }
    }

    fn from_line(cmd: &str) -> Result<Self, PolicyError> {
        if cmd.trim().is_empty() {
            return Err(PolicyError::new(
                PolicyReason::MissingCommand,
                "exec_command requires a non-empty cmd",
            ));
        }
        if cmd.len() > MAX_COMMAND_BYTES {
            return Err(PolicyError::new(
                PolicyReason::CommandTooLong,
                "Command is too long",
            ));
        }
        // 拆词放到后面（`validate` 里 shell 语法检测之后）才做：先报"有非法
        // 操作符"比先报"引号没配对"更接近真实原因。这里只留原文。
        Ok(Self {
            parts: Vec::new(),
            display: cmd.to_string(),
            form: CommandForm::Line,
        })
    }

    fn from_argv(argv: &Value) -> Result<Self, PolicyError> {
        let items = argv.as_array().ok_or_else(|| {
            PolicyError::new(
                PolicyReason::InvalidSyntax,
                "argv must be an array of strings",
            )
        })?;
        let mut parts = Vec::with_capacity(items.len());
        for (index, item) in items.iter().enumerate() {
            let text = item.as_str().ok_or_else(|| {
                PolicyError::new(
                    PolicyReason::InvalidSyntax,
                    format!("argv[{index}] must be a string"),
                )
            })?;
            parts.push(text.to_string());
        }
        match parts.first() {
            None => {
                return Err(PolicyError::new(
                    PolicyReason::MissingCommand,
                    "argv must not be empty",
                ))
            }
            Some(program) if program.trim().is_empty() => {
                return Err(PolicyError::new(
                    PolicyReason::MissingCommand,
                    "argv[0] must be the program to run",
                ))
            }
            Some(_) => {}
        }
        let display = shell_words::join(parts.iter().map(String::as_str));
        if display.len() > MAX_COMMAND_BYTES {
            return Err(PolicyError::new(
                PolicyReason::CommandTooLong,
                "Command is too long",
            ));
        }
        Ok(Self {
            parts,
            display,
            form: CommandForm::Argv,
        })
    }

    /// 服务端自己造的命令，比如 `exec_health_check` 的探针。
    ///
    /// 它不来自调用方，也不过策略（策略管的是"调用方能让我跑什么"）。用
    /// `argv` 形式是因为探针本来就要 `sh -c`，参数里的 `;` 和 `>&2` 是交给
    /// sh 的数据。
    pub fn internal(parts: Vec<&str>) -> Self {
        let parts: Vec<String> = parts.into_iter().map(str::to_string).collect();
        let display = shell_words::join(parts.iter().map(String::as_str));
        Self {
            parts,
            display,
            form: CommandForm::Argv,
        }
    }

    /// 把 `cmd` 那一行拆成 argv。`argv` 形式已经是拆好的，原样返回。
    ///
    /// 调用点必须在 shell 语法检测之后——见 [`from_line`](Self::from_line)。
    pub fn resolved_parts(&self) -> Result<Vec<String>, PolicyError> {
        if self.form == CommandForm::Argv {
            return Ok(self.parts.clone());
        }
        let parts = shell_words::split(&self.display)
            .map_err(|_| PolicyError::new(PolicyReason::InvalidSyntax, "Invalid command syntax"))?;
        if parts.is_empty() {
            return Err(PolicyError::new(
                PolicyReason::MissingCommand,
                "Empty command",
            ));
        }
        Ok(parts)
    }

    /// 要不要对它做 shell 操作符检测。
    pub fn needs_shell_syntax_check(&self) -> bool {
        self.form == CommandForm::Line
    }
}

/// 一条命令最长多少字节。两种形式同一个上限——`argv` 形式量的是合成之后的
/// 那一行，不然拆成一千格就能绕过去。
const MAX_COMMAND_BYTES: usize = 4_000;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_line_stays_a_line_until_it_is_split() {
        let spec = CommandSpec::from_args(&json!({"cmd": "cargo test --workspace"})).expect("spec");
        assert_eq!(spec.form, CommandForm::Line);
        assert!(spec.needs_shell_syntax_check());
        assert_eq!(
            spec.resolved_parts().expect("split"),
            vec!["cargo", "test", "--workspace"]
        );
    }

    #[test]
    fn argv_keeps_every_argument_exactly_as_given() {
        let spec =
            CommandSpec::from_args(&json!({"argv": ["rg", "foo|bar", "src"]})).expect("spec");
        assert_eq!(spec.form, CommandForm::Argv);
        assert!(!spec.needs_shell_syntax_check());
        assert_eq!(
            spec.resolved_parts().expect("parts"),
            vec!["rg", "foo|bar", "src"]
        );
        // 展示文本会把特殊字符引起来，所以它拿回去当 cmd 也不会变成管道。
        assert_eq!(spec.display, "rg 'foo|bar' src");
    }

    #[test]
    fn giving_both_forms_is_rejected_rather_than_guessed() {
        let error = CommandSpec::from_args(&json!({"cmd": "ls", "argv": ["ls"]}))
            .expect_err("两个都给必须拒");
        assert_eq!(error.reason, PolicyReason::ConflictingCommandForms);
    }

    #[test]
    fn an_empty_argv_is_a_missing_command() {
        let error = CommandSpec::from_args(&json!({"argv": []})).expect_err("空 argv");
        assert_eq!(error.reason, PolicyReason::MissingCommand);
        let error = CommandSpec::from_args(&json!({"argv": ["  "]})).expect_err("空程序名");
        assert_eq!(error.reason, PolicyReason::MissingCommand);
    }

    #[test]
    fn argv_items_must_be_strings() {
        let error = CommandSpec::from_args(&json!({"argv": ["ls", 7]})).expect_err("数字参数");
        assert_eq!(error.reason, PolicyReason::InvalidSyntax);
    }

    #[test]
    fn neither_form_is_a_missing_command() {
        let error = CommandSpec::from_args(&json!({"workdir": "."})).expect_err("什么都没给");
        assert_eq!(error.reason, PolicyReason::MissingCommand);
    }

    /// 拆成很多格也不能绕过长度上限。
    #[test]
    fn the_length_cap_applies_to_argv_too() {
        let parts: Vec<String> = (0..1000).map(|_| "0123456789".to_string()).collect();
        let error = CommandSpec::from_args(&json!({"argv": parts})).expect_err("超长 argv");
        assert_eq!(error.reason, PolicyReason::CommandTooLong);
    }
}
