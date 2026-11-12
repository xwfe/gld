//! 本机 IPC 的用例输入与清理报告；解析项目、发凭据和执行清理由应用侧负责。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub const DEFAULT_BROWSER_GRANT_TTL_SECONDS: u64 = 8 * 60 * 60;

/// 调用方对“哪个工作区”的描述。
///
/// - `selector`：用户显式给出的 id / id 前缀 / 名称 / 路径；
/// - `cwd`：调用方所在目录，用于在没有 selector 时按目录归属推断。
///
/// 由应用侧解析为实际工作区。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkspaceTarget {
    #[serde(default)]
    pub selector: Option<String>,
    #[serde(default)]
    pub cwd: Option<PathBuf>,
}

impl WorkspaceTarget {
    pub fn selector(selector: impl Into<String>) -> Self {
        Self {
            selector: Some(selector.into()),
            cwd: None,
        }
    }

    pub fn new(selector: Option<String>, cwd: Option<PathBuf>) -> Self {
        Self { selector, cwd }
    }
}

/// 创建工作区时可选的覆盖项。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkspaceCreateOptions {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub mcp_port: Option<u16>,
    #[serde(default)]
    pub actions_port: Option<u16>,
}

/// `gld grant add` 收到的参数。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrantSpec {
    pub name: String,
    /// 服务里的项目：id、名称或 id 前缀（≥4 位），本地和远端都行。
    pub workspaces: Vec<String>,
    /// 能写能跑命令。默认 false（只读）：能跑命令就能以你的身份读到数据目录里的服务
    /// 口令，拿到全权，所以要显式要（`gld grant add --write`）。
    #[serde(default)]
    pub writable: bool,
}

/// 浏览器连接单独发短期钥匙；旧 AddGrant IPC 不接受这项操作。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserGrantSpec {
    pub name: String,
    pub workspaces: Vec<String>,
    #[serde(default)]
    pub writable: bool,
    #[serde(default = "default_browser_ttl")]
    pub ttl_seconds: u64,
    #[serde(default)]
    pub allowed_origins: Vec<String>,
}

fn default_browser_ttl() -> u64 {
    DEFAULT_BROWSER_GRANT_TTL_SECONDS
}

/// `gld hub remote add` 收到的参数。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CcnmMemberSpec {
    /// 给人看的名字，也是调用时 `workspace` 参数可以用的值；空串表示和 ccnm 里的项目名一样。
    pub name: String,
    /// ccnm 配置里的 node 别名（一台机器的名字），不是 host 也不是 user；空串表示交给 ccnm 选。
    pub node: String,
    /// ccnm 里的项目（workspace）名，不是路径。
    pub workspace: String,
    /// 本机 `ccnm` 可执行程序；空串表示用 PATH 里的 `ccnm`。
    #[serde(default)]
    pub ccnm_bin: String,
    /// 访问上限：`coding`（默认）或 `read`。
    #[serde(default)]
    pub mode: String,
}

/// 全局运行时设置（影响所有工作区）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GlobalRuntimeSettingsDto {
    #[serde(default)]
    pub executable_paths: String,
    #[serde(default)]
    pub ai_instructions: String,
    #[serde(default)]
    pub instruction_sources: Vec<String>,
    #[serde(default)]
    pub skill_sources: Vec<String>,
    #[serde(default)]
    pub custom_instruction_paths: String,
    #[serde(default)]
    pub custom_skill_paths: String,
    #[serde(default)]
    pub hidden_skills: Vec<String>,
    #[serde(default)]
    pub allow_lan_access: bool,
    #[serde(default)]
    pub restore_runtime_state_on_launch: bool,
}

/// `gld prune` 看到的（只列清单时）或者做了的（真删时）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PruneReport {
    /// 登记了、目录却已经不在的项目。
    pub missing_projects: Vec<MissingProject>,
    /// 对不上任何留下的项目的数据：已删项目的，和上面那些项目的。
    pub leftovers: Vec<Leftover>,
    /// 也对不上，但里面还有命令或 Agent 在跑，这次不动。
    #[serde(default)]
    pub busy: Vec<Leftover>,
    /// 真删了是 true；只列清单是 false。
    #[serde(default)]
    pub applied: bool,
    /// 删的时候出了错的。
    #[serde(default)]
    pub failures: Vec<PruneFailure>,
}

impl PruneReport {
    pub fn is_empty(&self) -> bool {
        self.missing_projects.is_empty()
            && self.leftovers.is_empty()
            && self.busy.is_empty()
            && self.failures.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MissingProject {
    pub id: String,
    pub name: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Leftover {
    /// logs | oauth | harness | runs | agents
    pub kind: String,
    pub path: String,
    pub bytes: u64,
    /// 属于上面哪个目录不在了的项目（它的 id）；已经删掉的项目留下的是空串。
    #[serde(default)]
    pub project: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PruneFailure {
    /// 项目 id 或路径。
    pub target: String,
    pub error: String,
}
