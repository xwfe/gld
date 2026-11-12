//! 工作区构建适配与 MCP 结果包装；路径和文件能力由 gld-runtime 提供。

use std::path::PathBuf;

use serde_json::{json, Value};

pub use gld_runtime::workspace::{
    current_file_version, file_state, file_version, relative_display, tool_err, tool_err_code,
    tool_ok, FileState, ResolvedPath, Workspace, WorkspaceError, WorkspaceResult,
    DEFAULT_EXCLUDED_NAMES,
};

/// 在应用层解析 GLD_HOME，再显式交给文件边界保护。
pub fn new_workspace(root: PathBuf) -> WorkspaceResult<Workspace> {
    let protected_read_roots = crate::home::data_home().ok().into_iter().collect();
    Workspace::new(root, protected_read_roots)
}

pub fn wrap_tool_result(structured: Value) -> Value {
    wrap_mcp_tool_result("", &serde_json::json!({}), structured)
}

pub fn wrap_mcp_tool_result(tool_name: &str, args: &Value, structured: Value) -> Value {
    let is_error = structured.get("ok").and_then(Value::as_bool) == Some(false);
    let content = if tool_name == "view_image"
        && args
            .get("output")
            .and_then(Value::as_str)
            .unwrap_or("mcp_image")
            == "mcp_image"
        && !is_error
    {
        vec![json!({
            "type": "image",
            "data": structured.get("base64").and_then(Value::as_str).unwrap_or(""),
            "mimeType": structured
                .get("mime_type")
                .and_then(Value::as_str)
                .unwrap_or("application/octet-stream")
        })]
    } else {
        vec![json!({
            "type": "text",
            "text": structured.to_string()
        })]
    };
    json!({
        "content": content,
        "structuredContent": structured,
        "isError": is_error
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_factory_protects_the_configured_data_home() {
        crate::home::isolate_for_tests();
        let home = crate::home::data_home().expect("data home");
        std::fs::create_dir_all(&home).expect("data home directory");
        let protected = tempfile::tempdir_in(home).expect("protected fixture");
        let secret = protected.path().join("secret.json");
        std::fs::write(&secret, "{}").expect("fixture");
        let project = tempfile::tempdir().expect("workspace");
        let workspace = new_workspace(project.path().to_path_buf())
            .expect("workspace")
            .with_confined_reads(false);
        assert_eq!(
            workspace
                .resolve_read_path(&secret.to_string_lossy())
                .unwrap_err()
                .code(),
            "GLD_DATA_HOME_DENIED"
        );
        assert!(workspace.is_ignored_path(&secret, true, true));
    }
}
