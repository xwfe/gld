//! 直接调用工具内核：`gld tool list` / `gld tool call`。
//!
//! 用途是「不接 AI 客户端也能验证工具行为」——排查“Agent 说它读不到文件”
//! 这类问题时，直接在命令行跑一次同样的工具调用即可，看到的错误结构和
//! AI 看到的完全一样（同一个 [`crate::tools::build_tool_context`]、
//! 同一个 [`crate::tools::call_tool`]）。

use std::sync::Arc;

use serde_json::Value;

use super::App;
use crate::error::{AppError, AppResult};
use crate::tools::registry::{canonical_tool_name, exposed_tool_names};
use crate::tools::{call_tool, list_tools_for_profile, ToolContext};
use crate::usage::ServiceUsage;

impl App {
    /// 当前工作区按 tool-profile 暴露的工具定义（含 JSON Schema）。
    pub fn list_tools(&self, id: &str) -> AppResult<Vec<Value>> {
        let profile = self.profile_by_id(id)?;
        Ok(list_tools_for_profile(&profile.runtime.tool_profile))
    }

    /// 执行一次工具调用，返回结构化结果（含 `ok` 字段）。
    ///
    /// 未知工具名在这里就被拒绝，而不是交给内核返回一个含糊的错误；
    /// 别名（如 `grep` → `grep_text`）与 MCP 侧用同一张映射表。
    pub fn call_tool(&self, id: &str, name: &str, args: Value) -> AppResult<Value> {
        let profile = self.profile_by_id(id)?;
        let canonical = canonical_tool_name(name);
        let exposed = exposed_tool_names(&profile.runtime.tool_profile);
        if !exposed.contains(&canonical) {
            return Err(AppError::Message(format!(
                "当前工具集「{}」没有暴露工具「{name}」。`gld tool list` 查看可用工具，\
                 或用 `gld ws set mcp.tool-profile=advanced` 换一个工具集。",
                profile.runtime.tool_profile
            )));
        }
        if !args.is_object() {
            return Err(AppError::Message("工具参数必须是 JSON 对象".into()));
        }
        let ctx = self.tool_context(id)?;
        Ok(call_tool(&ctx, canonical, &args))
    }

    /// 取得（必要时构建并缓存）该工作区的工具上下文。
    fn tool_context(&self, id: &str) -> AppResult<Arc<ToolContext>> {
        if let Some(cached) = self.with_tool_contexts(|contexts| Ok(contexts.get(id).cloned()))? {
            return Ok(cached);
        }
        let profile = self.profile_by_id(id)?;
        let settings = self.settings()?;
        let context = crate::tools::build_tool_context(
            profile.path.clone().into(),
            profile.auth.clone(),
            &profile.runtime,
            &settings,
            Arc::new(ServiceUsage::default()),
        )
        .map_err(AppError::Message)?;
        let context = Arc::new(context);
        self.with_tool_contexts(|contexts| {
            // 并发下可能已经有人建好了，用先到的那个，避免同一工作区出现两份会话表。
            Ok(contexts.entry(id.to_string()).or_insert(context).clone())
        })
    }
}
