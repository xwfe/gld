pub mod context;
pub mod dispatch;
pub mod exec;
pub mod file;
pub mod git;
pub mod history;
mod image_tool;
pub mod manage;
pub mod patch;
pub mod planning;
pub mod policy;
pub mod registry;
pub mod session;
pub mod setup;
pub mod skill;
pub mod workspace;

pub use context::{SharedToolContext, ToolContext};
/// 唯一工具执行入口；MCP 与 Actions 必须调用此函数，不得分叉实现。
pub use dispatch::call_tool;
pub use policy::{validate_actions_exposure, PolicySettings};
pub use registry::{
    exposed_tool_names, is_allowed_tool, list_tools, list_tools_for_profile, MUTATING_TOOLS,
};
/// 唯一工具上下文构建入口；MCP 监听器与命令行共用，避免两边行为漂移。
pub use setup::build_tool_context;
pub use workspace::{wrap_mcp_tool_result, wrap_tool_result, Workspace};
