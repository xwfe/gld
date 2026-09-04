use std::path::Path;

use serde_json::Value;

use super::App;
use crate::agent_context::{
    discover, merge_source_lists, scan_global_agent_context, AgentContextSnapshot,
    GlobalAgentContextScan,
};
use crate::error::{AppError, AppResult};
use crate::health::{run_health_checks, HealthItem};
use crate::runtime::ServiceKind;
use crate::tools::{history, Workspace};
use crate::usage::ServiceUsageStats;

impl App {
    /// 逐项检查本地 / 公网端点与 OAuth 元数据是否可达。
    pub async fn health_checks(&self, id: &str) -> AppResult<Vec<HealthItem>> {
        let profile = self.profile_by_id(id)?;
        Ok(run_health_checks(&profile).await)
    }

    /// 项目内 `docs/history-session/` 的会话目录（有界，不含正文）。
    pub fn history_sessions(&self, id: &str) -> AppResult<Value> {
        let profile = self.profile_by_id(id)?;
        let workspace = Workspace::new(profile.path.into())
            .map_err(|error| AppError::Message(error.message()))?;
        history::list_sessions_for_workspace(&workspace)
            .map_err(|error| AppError::Message(error.message()))
    }

    /// 本次守护进程生命周期内的请求 / Token 估算统计。
    pub fn usage_stats(&self, id: &str) -> AppResult<Vec<ServiceUsageStats>> {
        self.ensure_workspace_exists(id)?;
        self.with_runtime(|runtime| {
            Ok(vec![
                runtime.usage_stats(id, ServiceKind::Mcp),
                runtime.usage_stats(id, ServiceKind::Actions),
            ])
        })
    }

    /// 扫描工作区里会被注入给 Agent 的说明文件与 Skill。
    pub fn agent_context(&self, id: &str) -> AppResult<AgentContextSnapshot> {
        let profile = self.profile_by_id(id)?;
        let settings = self.settings()?;
        let instruction_sources = merge_source_lists(
            &settings.global_instruction_sources,
            &profile.runtime.instruction_sources,
        );
        let skill_sources = merge_source_lists(
            &settings.global_skill_sources,
            &profile.runtime.skill_sources,
        );
        let instruction_paths = merge_text(
            &settings.global_custom_instruction_paths,
            &profile.runtime.custom_instruction_paths,
        );
        let skill_paths = merge_text(
            &settings.global_custom_skill_paths,
            &profile.runtime.custom_skill_paths,
        );
        Ok(discover(
            Path::new(&profile.path),
            &instruction_sources,
            &skill_sources,
            &instruction_paths,
            &skill_paths,
            &profile.runtime.tool_profile,
        ))
    }

    /// 探测用户主目录下各 IDE / Agent 的全局说明与 Skill 来源。
    pub fn global_agent_context(&self) -> GlobalAgentContextScan {
        scan_global_agent_context()
    }
}

fn merge_text(global: &str, workspace: &str) -> String {
    [global.trim(), workspace.trim()]
        .into_iter()
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}
