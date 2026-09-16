//! 一个「远端 workspace」成员：它在哪台机器上、用哪个 ccnm、最多能做什么。
//!
//! 跟本地成员（`WorkspaceProfile`）是**两个类型**，不共用序列化，也不拿 fake
//! path 去迁就本地那套（RFC-0002 5.1）。本地成员有 root 路径、隧道、Planning、
//! Harness；远端成员一样都没有——远端的 root 由 ccnm Runtime 自己解析，gld
//! 这边连它是哪个目录都不该知道。
//!
//! **这个模块最要紧的一件事**：启动 bridge 的那条命令行完全由操作员配置决定，
//! 模型一个字都插不进去。见 [`CcnmMember::bridge_argv`]。

use serde::{Deserialize, Serialize};

/// 对远端 workspace 的访问级别。跟 ccnm `mcp bridge --mode` 的取值一一对应。
///
/// 顺序有意义：`Read < Coding`，取交集时直接比大小。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// 四个只读工具。
    Read,
    /// 七个工具，并且在远端持有整棵工作树的写入互斥锁。
    Coding,
}

impl Mode {
    /// 传给 `ccnm mcp bridge --mode` 的值。
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Read => "read",
            Mode::Coding => "coding",
        }
    }

    /// 取两个级别里较低的那个。
    ///
    /// hub 权限、成员配置的上限、这次调用要求的级别，三者取交集就是连着
    /// 用两次它。**只会往下降**，所以没有哪条路径能把 read-only 的成员
    /// 提成 coding。
    pub fn min(self, other: Mode) -> Mode {
        if self <= other {
            self
        } else {
            other
        }
    }
}

/// 一个远端 ccnm workspace 成员。
///
/// 字段全部来自操作员在 gld 这边的配置。**没有一个是模型能填的**——尤其
/// 没有远端 root、没有 SSH 用户、没有私钥路径：那些是 ccnm Runtime 那边的
/// 事，gld 不解析也不转发。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CcnmMember {
    /// 稳定的成员 ID，hub 路由用它。
    pub id: String,
    /// 给人看的名字。
    pub name: String,
    /// 本机 `ccnm` 可执行程序。默认就是 PATH 里的 `ccnm`。
    #[serde(default = "default_ccnm_bin")]
    pub ccnm_bin: String,
    /// ccnm 配置里的 node 别名（一台机器的名字），不是 host 也不是 user。
    pub node: String,
    /// ccnm 配置里的 workspace 名字，不是路径。
    pub workspace: String,
    /// 这个成员最多允许到哪一级。默认只读。
    #[serde(default = "default_mode")]
    pub max_mode: Mode,
}

fn default_ccnm_bin() -> String {
    "ccnm".into()
}

fn default_mode() -> Mode {
    Mode::Read
}

impl CcnmMember {
    /// 启动 bridge 的 argv。
    ///
    /// **只有 `mode` 是调用时决定的，而且它已经被 [`Mode::min`] 压到成员上限
    /// 以内。**其余每一项都来自这个结构里的配置。返回的是 argv 数组，调用方
    /// 直接 spawn，不经过 shell——所以配置里哪怕有空格、引号、`;`，也只是一个
    /// 普通的参数值，拼不出第二条命令。
    ///
    /// 只用公开子命令 `ccnm mcp bridge`。gld 不生成 ccnm 的内部 `mcp-serve`
    /// payload，不碰 SSH 凭据，也不绕过官方 bridge（RFC-0002 5.1）。
    pub fn bridge_argv(&self, mode: Mode) -> (String, Vec<String>) {
        let mode = mode.min(self.max_mode);
        (
            self.ccnm_bin.clone(),
            vec![
                "mcp".into(),
                "bridge".into(),
                self.workspace.clone(),
                "--node".into(),
                self.node.clone(),
                "--mode".into(),
                mode.as_str().into(),
            ],
        )
    }

    /// 这次调用实际能拿到的级别：要求的和配置上限取低。
    pub fn effective_mode(&self, wanted: Mode) -> Mode {
        wanted.min(self.max_mode)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member() -> CcnmMember {
        CcnmMember {
            id: "m1".into(),
            name: "远端项目".into(),
            ccnm_bin: "ccnm".into(),
            node: "work".into(),
            workspace: "myproject".into(),
            max_mode: Mode::Coding,
        }
    }

    #[test]
    fn the_argv_is_the_public_bridge_command() {
        let (program, args) = member().bridge_argv(Mode::Read);
        assert_eq!(program, "ccnm");
        assert_eq!(
            args,
            vec![
                "mcp",
                "bridge",
                "myproject",
                "--node",
                "work",
                "--mode",
                "read"
            ]
        );
    }

    /// 成员配的是只读，要 coding 也只能拿到只读——不是报错，是降级到配置
    /// 允许的范围。想放开得操作员改配置。
    #[test]
    fn a_read_only_member_cannot_be_asked_into_coding() {
        let mut m = member();
        m.max_mode = Mode::Read;
        assert_eq!(m.effective_mode(Mode::Coding), Mode::Read);
        let (_, args) = m.bridge_argv(Mode::Coding);
        assert!(
            args.ends_with(&["--mode".to_string(), "read".to_string()]),
            "{args:?}"
        );
    }

    #[test]
    fn taking_the_lower_of_two_levels_never_goes_up() {
        assert_eq!(Mode::Read.min(Mode::Coding), Mode::Read);
        assert_eq!(Mode::Coding.min(Mode::Read), Mode::Read);
        assert_eq!(Mode::Coding.min(Mode::Coding), Mode::Coding);
        assert_eq!(Mode::Read.min(Mode::Read), Mode::Read);
    }

    /// 配置里带古怪字符也只是一个参数值——因为是 argv 不是 shell 字符串。
    /// 这条钉住的是「不拼 shell」这个决定，不是说这种配置合理。
    #[test]
    fn odd_characters_in_the_config_stay_one_argument() {
        let mut m = member();
        m.workspace = "a b; rm -rf /".into();
        let (_, args) = m.bridge_argv(Mode::Read);
        assert_eq!(args[2], "a b; rm -rf /", "必须原样是一个参数");
        assert_eq!(args.len(), 7, "不该多出参数来：{args:?}");
    }

    /// 旧数据文件里没有这些字段，反序列化要能补上默认值，而且默认是**只读**。
    #[test]
    fn the_defaults_are_ccnm_on_path_and_read_only() {
        let m: CcnmMember = serde_json::from_str(
            r#"{ "id": "m1", "name": "n", "node": "work", "workspace": "p" }"#,
        )
        .expect("反序列化");
        assert_eq!(m.ccnm_bin, "ccnm");
        assert_eq!(m.max_mode, Mode::Read, "默认必须是只读");
    }

    #[test]
    fn a_mode_round_trips_through_json_as_a_lowercase_word() {
        assert_eq!(
            serde_json::to_string(&Mode::Coding).expect("序列化"),
            "\"coding\""
        );
        let back: Mode = serde_json::from_str("\"read\"").expect("反序列化");
        assert_eq!(back, Mode::Read);
    }
}
