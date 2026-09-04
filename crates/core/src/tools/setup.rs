//! 构建 [`ToolContext`] 的唯一入口。
//!
//! MCP 监听器和命令行的 `gld tool call` 都从这里拿上下文，所以命令行里试出来的
//! 行为就是 AI 客户端看到的行为。如果两边各自拼一套（早期就是这样），
//! 改了工具集或可执行路径的合并规则，很容易只改到一边。
//!
//! 工作区配置和全局设置的合并规则：可执行路径、Agent 说明、说明 / Skill 来源
//! 都是「全局在前、工作区在后」，工作区的值不覆盖全局，而是追加。

use std::path::PathBuf;
use std::sync::Arc;

use crate::agent_context::{merge_source_lists, AgentContextRuntimeConfig};
use crate::settings::AppSettings;
use crate::tools::context::{merge_ai_instructions, merge_executable_paths};
use crate::tools::policy::PolicySettings;
use crate::tools::{ToolContext, Workspace};
use crate::usage::ServiceUsage;
use crate::workspace::{AuthConfig, RuntimeConfig};

/// 按工作区运行时配置 + 全局设置构建工具上下文。
///
/// `auth` 单独传入而不是从 profile 取，是因为启用共享密钥池时
/// OAuth Client ID 会被换成共享池里的值。
pub fn build_tool_context(
    workspace_path: PathBuf,
    auth: AuthConfig,
    runtime: &RuntimeConfig,
    settings: &AppSettings,
    usage: Arc<ServiceUsage>,
) -> Result<ToolContext, String> {
    let workspace = Workspace::new(workspace_path).map_err(|error| error.message())?;
    let policy = PolicySettings::from_runtime(runtime);
    let agent_context = AgentContextRuntimeConfig {
        instruction_sources: merge_source_lists(
            &settings.global_instruction_sources,
            &runtime.instruction_sources,
        ),
        skill_sources: merge_source_lists(&settings.global_skill_sources, &runtime.skill_sources),
        custom_instruction_paths: merge_lines(
            &settings.global_custom_instruction_paths,
            &runtime.custom_instruction_paths,
        ),
        custom_skill_paths: merge_lines(
            &settings.global_custom_skill_paths,
            &runtime.custom_skill_paths,
        ),
    };

    Ok(ToolContext::from_workspace(
        workspace,
        auth,
        policy,
        runtime.tool_profile.clone(),
        runtime.permission_mode.clone(),
    )
    .with_agent_runtime(
        merge_executable_paths(&runtime.executable_paths, &settings.global_executable_paths),
        merge_ai_instructions(&settings.global_ai_instructions, &runtime.ai_instructions),
    )
    .with_agent_context(agent_context)
    .with_history_config(
        runtime.history_recording,
        runtime.history_context_sessions.clone(),
    )
    .with_usage(usage))
}

fn merge_lines(global: &str, workspace: &str) -> String {
    [global.trim(), workspace.trim()]
        .into_iter()
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_global_and_workspace_config_without_overriding() {
        let temp = tempfile::tempdir().expect("workspace");
        let settings = AppSettings {
            global_ai_instructions: "全局规则".into(),
            global_instruction_sources: vec!["claude".into()],
            global_custom_skill_paths: "~/skills".into(),
            ..AppSettings::default()
        };
        let runtime = RuntimeConfig {
            ai_instructions: "工作区规则".into(),
            instruction_sources: vec!["cursor".into()],
            custom_skill_paths: "./skills".into(),
            tool_profile: "compact".into(),
            ..RuntimeConfig::default()
        };

        let ctx = build_tool_context(
            temp.path().to_path_buf(),
            AuthConfig::default(),
            &runtime,
            &settings,
            Arc::new(ServiceUsage::default()),
        )
        .expect("context");

        assert_eq!(ctx.tool_profile, "compact");
        let instructions = ctx.current_ai_instructions();
        assert!(instructions.contains("全局规则"));
        assert!(instructions.contains("工作区规则"));
        let agent = ctx.agent_context.as_ref().expect("agent context");
        assert_eq!(agent.instruction_sources, vec!["claude", "cursor"]);
        assert_eq!(agent.custom_skill_paths, "~/skills\n./skills");
    }
}
