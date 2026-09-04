use std::path::{Component, Path, PathBuf};

use serde_json::{json, Value};
use thiserror::Error;

pub const DEFAULT_EXCLUDED_NAMES: &[&str] = &[
    ".git",
    ".reference",
    "node_modules",
    "target",
    "dist",
    "build",
    ".venv",
    "venv",
    ".tox",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
    "__pycache__",
];

#[derive(Debug, Clone)]
pub struct ResolvedPath {
    pub display: String,
    pub path: PathBuf,
    pub existed: bool,
}

#[derive(Debug, Error)]
pub enum WorkspaceError {
    #[error("{message}")]
    Tool {
        code: &'static str,
        message: String,
        category: &'static str,
        retryable: bool,
    },
    #[error("{message}")]
    ToolDetails {
        code: &'static str,
        message: String,
        category: &'static str,
        retryable: bool,
        details: Value,
    },
}

impl WorkspaceError {
    pub fn message(&self) -> String {
        match self {
            Self::Tool { message, .. } | Self::ToolDetails { message, .. } => message.clone(),
        }
    }

    pub fn invalid_argument(message: impl Into<String>) -> Self {
        Self::Tool {
            code: "INVALID_ARGUMENT",
            message: message.into(),
            category: "validation",
            retryable: false,
        }
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::Tool {
            code: "NOT_FOUND",
            message: message.into(),
            category: "not_found",
            retryable: false,
        }
    }

    pub fn absolute_path_denied() -> Self {
        Self::Tool {
            code: "ABSOLUTE_PATH_DENIED",
            message: "Absolute paths are denied.".into(),
            category: "security",
            retryable: false,
        }
    }

    pub fn path_outside_workspace() -> Self {
        Self::Tool {
            code: "PATH_OUTSIDE_WORKSPACE",
            message: "Path escapes the configured workspace.".into(),
            category: "security",
            retryable: false,
        }
    }

    pub fn symlink_escape() -> Self {
        Self::Tool {
            code: "SYMLINK_ESCAPE",
            message: "Path escapes the configured workspace.".into(),
            category: "security",
            retryable: false,
        }
    }

    pub fn not_a_directory(message: impl Into<String>) -> Self {
        Self::Tool {
            code: "NOT_A_DIRECTORY",
            message: message.into(),
            category: "validation",
            retryable: false,
        }
    }

    pub fn to_error_value(&self) -> Value {
        match self {
            Self::Tool {
                code,
                message,
                category,
                retryable,
            } => json!({
                "code": code,
                "message": message,
                "category": category,
                "retryable": retryable,
                "details": {}
            }),
            Self::ToolDetails {
                code,
                message,
                category,
                retryable,
                details,
            } => json!({
                "code": code,
                "message": message,
                "category": category,
                "retryable": retryable,
                "details": details
            }),
        }
    }
}

pub type WorkspaceResult<T> = Result<T, WorkspaceError>;

#[derive(Debug, Clone)]
pub struct Workspace {
    root: PathBuf,
    /// gld 数据目录的路径前缀，用来挡住对密钥库的读取。
    ///
    /// 建 Workspace 时算一次而不是每次现算：遍历类工具会对每个文件问一遍
    /// "这个要不要跳过"，在那里做 canonicalize 等于给每个文件加一次系统调用。
    ///
    /// 存两份是因为 macOS 上 `/var` 是指向 `/private/var` 的软链：遍历拿到的
    /// 路径可能是任意一侧，只比其中一份会漏。
    data_home_prefixes: Vec<PathBuf>,
    /// 读工具是否只许读 Workspace 里面。
    ///
    /// 默认 `true`。桌面版和 0.3.0 之前是不限制的——`read_file` 给个绝对路径就能读
    /// `~/.ssh/id_rsa`。本机自用时这只是方便，但服务是可以挂到公网给 ChatGPT 用的，
    /// 那时候仓库里任何一段文字都可能是提示词注入，"能读整台机器"就成了实打实的风险。
    ///
    /// 所以默认收紧，需要读隔壁仓库 / 系统头文件的人自己打开
    /// （`gld ws set mcp.confine-reads=false`）。
    confine_reads: bool,
}

impl Workspace {
    pub fn new(root: PathBuf) -> WorkspaceResult<Self> {
        let root = root
            .canonicalize()
            .map_err(|_| WorkspaceError::invalid_argument("Workspace root must exist"))?;
        if !root.is_dir() {
            return Err(WorkspaceError::invalid_argument(
                "Workspace root must be a directory",
            ));
        }
        Ok(Self {
            root,
            data_home_prefixes: data_home_prefixes(),
            // 默认收紧：新加的调用点忘了设也是安全的那一侧。
            confine_reads: true,
        })
    }

    /// 放开 / 收紧"只读 Workspace 内"。见 [`Workspace::confine_reads`] 字段说明。
    pub fn with_confined_reads(mut self, confine: bool) -> Self {
        self.confine_reads = confine;
        self
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn root_display(&self) -> String {
        self.root.to_string_lossy().into_owned()
    }

    pub fn reject_unsafe_text(&self, raw_path: &str) -> WorkspaceResult<()> {
        if raw_path.is_empty() {
            return Err(WorkspaceError::invalid_argument(
                "Path must be a non-empty string",
            ));
        }
        if raw_path.contains('\0') {
            return Err(WorkspaceError::invalid_argument("Path contains a NUL byte"));
        }
        if raw_path.starts_with('/') || raw_path.starts_with('\\') {
            return Err(WorkspaceError::absolute_path_denied());
        }
        if raw_path.len() >= 2 {
            let bytes = raw_path.as_bytes();
            if bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
                return Err(WorkspaceError::absolute_path_denied());
            }
        }
        for part in Path::new(raw_path).components() {
            if matches!(part, Component::ParentDir) {
                return Err(WorkspaceError::path_outside_workspace());
            }
        }
        Ok(())
    }

    pub fn resolve_existing(&self, raw_path: &str) -> WorkspaceResult<ResolvedPath> {
        self.resolve_existing_at(&self.root, raw_path)
    }

    /// 解析只读路径。
    ///
    /// `confine_reads`（默认开）为真时，读也限制在 Workspace 内，
    /// 和写入工具一致。关掉之后，显式的绝对路径和 `..` 路径可以指向外部，
    /// 但仍然不会被任何写入工具复用——那是桌面版的行为。
    ///
    /// gld 自己的数据目录任何情况下都读不到，见 [`Self::reject_data_home_read`]。
    pub fn resolve_read_path(&self, raw_path: &str) -> WorkspaceResult<ResolvedPath> {
        let raw = if raw_path.is_empty() { "." } else { raw_path };
        self.validate_read_text(raw)?;
        let input = Path::new(raw);
        let candidate = if input.is_absolute() {
            input.to_path_buf()
        } else {
            self.root
                .join(raw.replace('/', std::path::MAIN_SEPARATOR_STR))
        };
        let resolved = candidate
            .canonicalize()
            .map_err(|_| WorkspaceError::not_found(format!("Path not found: {raw}")))?;
        let explicit_external = input.is_absolute()
            || input
                .components()
                .any(|part| matches!(part, Component::ParentDir));
        if self.confine_reads {
            // 注意这里比的是 canonicalize 之后的路径：不然 Workspace 里一个
            // 指向外面的软链就能绕过去。
            if !resolved.starts_with(&self.root) {
                return Err(self.read_outside_workspace(raw));
            }
        } else if !explicit_external && candidate.starts_with(&self.root) {
            self.ensure_inside_workspace(&candidate, &resolved)?;
        }
        self.reject_data_home_read(&resolved)?;
        Ok(ResolvedPath {
            display: relative_display(&self.root, &resolved),
            path: resolved,
            existed: true,
        })
    }

    /// `confine_reads` 打开时，读到 Workspace 外面的报错。
    ///
    /// 报错里必须带上关掉的办法：读隔壁仓库、读系统头文件是正当需求，
    /// 只说一句"越界了"会让人以为这是死规则，然后去改代码或者放弃。
    fn read_outside_workspace(&self, raw_path: &str) -> WorkspaceError {
        WorkspaceError::ToolDetails {
            code: "READS_CONFINED_TO_WORKSPACE",
            message: format!(
                "只允许读 Workspace 内的文件，{raw_path} 在外面。\
                 确实需要读外部路径就关掉这个限制：gld ws set confine-reads=false（\
                 Actions 侧写全 actions.confine-reads）；改完服务会自动重启。"
            ),
            category: "security",
            retryable: false,
            details: json!({
                "workspace_root": self.root.to_string_lossy(),
                "setting": "mcp.confine-reads",
            }),
        }
    }

    /// 挡住对 gld 自己数据目录（`GLD_HOME`，默认 `~/.gld`）的读取。
    ///
    /// 读工具本来就允许读 Workspace 外面的东西——这是从桌面版继承的行为，
    /// 方便读隔壁仓库、读系统头文件。但 `~/.gld/data/profiles.json` 是个特例：
    /// 它以明文存着**每个**工作区的 bearer_token、oauth_password、
    /// oauth_token_secret、actions_api_key。也就是说，谁能读一个工作区的文件，
    /// 谁就拿到了所有工作区的钥匙——一次提示词注入就能横向打穿全部连接器。
    ///
    /// 而且 Plan 模式明确禁掉了 exec_command、只放行读工具，那时候读工具
    /// 就是唯一的边界，这个洞会让 Plan 模式的承诺不成立。
    ///
    /// 没有开关：任何正常的写代码任务都不需要读 gld 的数据目录。
    /// 唯一的例外是用户自己把 Workspace 指到了数据目录里，那是他自己的选择。
    ///
    /// 注意这**不是**沙箱：trusted 模式下 exec_command 仍能 `cat` 这个文件。
    /// 这里堵的是默认就在、且模型最容易走的那条路。
    fn reject_data_home_read(&self, resolved: &Path) -> WorkspaceResult<()> {
        if !self.is_gld_data_home_path(resolved) {
            return Ok(());
        }
        Err(WorkspaceError::Tool {
            code: "GLD_DATA_HOME_DENIED",
            message: "拒绝读取 gld 自己的数据目录：里面是所有工作区的密钥明文。".into(),
            category: "security",
            retryable: false,
        })
    }

    /// 这个路径是否落在 gld 的数据目录里（Workspace 自己的内容除外）。
    fn is_gld_data_home_path(&self, path: &Path) -> bool {
        if path.starts_with(&self.root) {
            return false;
        }
        self.data_home_prefixes
            .iter()
            .any(|prefix| path.starts_with(prefix))
    }

    pub fn resolve_existing_at(
        &self,
        base: &Path,
        raw_path: &str,
    ) -> WorkspaceResult<ResolvedPath> {
        let raw = if raw_path.is_empty() { "." } else { raw_path };
        self.reject_unsafe_text(raw)?;
        let base = self.validate_base(base)?;
        let candidate = base.join(raw.replace('/', std::path::MAIN_SEPARATOR_STR));
        let resolved = candidate
            .canonicalize()
            .map_err(|_| WorkspaceError::not_found(format!("Path not found: {raw}")))?;
        self.ensure_inside_workspace(&candidate, &resolved)?;
        Ok(ResolvedPath {
            display: relative_display(&self.root, &resolved),
            path: resolved,
            existed: true,
        })
    }

    pub fn resolve_for_write(&self, raw_path: &str) -> WorkspaceResult<ResolvedPath> {
        self.reject_unsafe_text(raw_path)?;
        self.reject_protected_write_path(raw_path)?;
        let pure = Path::new(raw_path);
        if pure.file_name().is_none() || raw_path == "." || raw_path == ".." {
            return Err(WorkspaceError::invalid_argument("Invalid write target"));
        }
        let candidate = self
            .root
            .join(raw_path.replace('/', std::path::MAIN_SEPARATOR_STR));
        if candidate.exists() || candidate.is_symlink() {
            let resolved = candidate
                .canonicalize()
                .map_err(|_| WorkspaceError::not_found(format!("Path not found: {raw_path}")))?;
            self.ensure_inside_workspace(&candidate, &resolved)?;
            return Ok(ResolvedPath {
                display: relative_display(&self.root, &resolved),
                path: resolved,
                existed: true,
            });
        }
        let parent = candidate.parent().unwrap_or(&self.root);
        let resolved_parent = if parent.exists() {
            parent
                .canonicalize()
                .map_err(|_| WorkspaceError::not_found("Parent directory not found"))?
        } else {
            self.ensure_parent_chain(parent)?;
            parent.to_path_buf()
        };
        if !resolved_parent.starts_with(&self.root) {
            return Err(WorkspaceError::path_outside_workspace());
        }
        Ok(ResolvedPath {
            display: raw_path.replace('\\', "/"),
            path: candidate,
            existed: false,
        })
    }

    fn ensure_parent_chain(&self, parent: &Path) -> WorkspaceResult<()> {
        let mut cursor = parent;
        while !cursor.exists() {
            if cursor == self.root || cursor.parent() == Some(cursor) {
                break;
            }
            cursor = cursor.parent().unwrap_or(cursor);
        }
        if cursor.exists() {
            let resolved = cursor
                .canonicalize()
                .map_err(|_| WorkspaceError::not_found("Parent directory not found"))?;
            if !resolved.starts_with(&self.root) {
                return Err(WorkspaceError::path_outside_workspace());
            }
        }
        Ok(())
    }

    fn validate_base(&self, base: &Path) -> WorkspaceResult<PathBuf> {
        let resolved = base
            .canonicalize()
            .map_err(|_| WorkspaceError::not_found("Base path not found"))?;
        if !resolved.is_dir() {
            return Err(WorkspaceError::not_a_directory("Base is not a directory"));
        }
        if !resolved.starts_with(&self.root) {
            return Err(WorkspaceError::path_outside_workspace());
        }
        Ok(resolved)
    }

    fn ensure_inside_workspace(&self, candidate: &Path, resolved: &Path) -> WorkspaceResult<()> {
        if !resolved.starts_with(&self.root) {
            if candidate.is_symlink() {
                return Err(WorkspaceError::symlink_escape());
            }
            return Err(WorkspaceError::path_outside_workspace());
        }
        Ok(())
    }

    pub fn reject_write_symlink(&self, raw_path: &str) -> WorkspaceResult<()> {
        self.reject_unsafe_text(raw_path)?;
        let candidate = self
            .root
            .join(raw_path.replace('/', std::path::MAIN_SEPARATOR_STR));
        if candidate.is_symlink() {
            return Err(WorkspaceError::symlink_escape());
        }
        Ok(())
    }

    pub fn reject_protected_write_path(&self, raw_path: &str) -> WorkspaceResult<()> {
        let normalized = raw_path.replace('\\', "/");
        let first = normalized.split('/').next().unwrap_or("");
        if matches!(first, ".git" | ".github") {
            return Err(WorkspaceError::Tool {
                code: "PROTECTED_PATH",
                message: format!("禁止普通文件操作写入受保护目录: {raw_path}"),
                category: "security",
                retryable: false,
            });
        }
        Ok(())
    }

    fn validate_read_text(&self, raw_path: &str) -> WorkspaceResult<()> {
        if raw_path.contains('\0') {
            return Err(WorkspaceError::invalid_argument("Path contains a NUL byte"));
        }
        Ok(())
    }

    pub fn is_ignored_path(
        &self,
        path: &Path,
        include_hidden: bool,
        include_ignored: bool,
    ) -> bool {
        // 遍历类工具（list_dir / list_files / search_text）的统一出口。
        // 单靠 resolve_read_path 挡不住这条路：那里只校验用户给的起点，
        // 之后 WalkDir 会自己走下去。`search_text path=~ include_hidden=true`
        // 就能直接从 `~/.gld/data/profiles.json` 里搜出所有工作区的密钥。
        if self.is_gld_data_home_path(path) {
            return true;
        }
        let Ok(scan_path) = path.strip_prefix(&self.root) else {
            // Workspace 外的读取路径不套用 Workspace 内部的隐藏/构建目录过滤，
            // 否则 Windows 临时目录等路径会被误判为隐藏目录而无法读取。
            return false;
        };
        let parts: Vec<String> = scan_path
            .components()
            .filter_map(|part| match part {
                Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
                _ => None,
            })
            .collect();
        if !include_hidden {
            for part in &parts {
                if part.starts_with('.') && part != "." {
                    return true;
                }
            }
        }
        if !include_ignored {
            for part in &parts {
                if DEFAULT_EXCLUDED_NAMES.contains(&part.as_str()) {
                    return true;
                }
            }
        }
        false
    }

    pub fn is_safe_existing_path(&self, path: &Path) -> bool {
        path.canonicalize()
            .map(|p| p.starts_with(&self.root))
            .unwrap_or(false)
    }

    pub fn is_safe_read_path(&self, path: &Path) -> bool {
        path.exists() || path.is_symlink()
    }
}

/// gld 数据目录的所有等价写法（原始路径 + canonicalize 之后的路径）。
fn data_home_prefixes() -> Vec<PathBuf> {
    let Ok(home) = crate::home::data_home() else {
        return Vec::new();
    };
    // 目录可能还没建出来，那时 canonicalize 会失败，只留原始路径。
    match home.canonicalize() {
        Ok(real) if real != home => vec![home, real],
        _ => vec![home],
    }
}

pub fn relative_display(root: &Path, path: &Path) -> String {
    let display = path
        .strip_prefix(root)
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| path.to_string_lossy().replace('\\', "/"));
    #[cfg(windows)]
    {
        if let Some(unc) = display.strip_prefix("//?/UNC/") {
            return format!("//{unc}");
        }
        if let Some(normal) = display.strip_prefix("//?/") {
            return normal.to_string();
        }
    }
    display
}

pub fn tool_ok(mut value: Value) -> Value {
    if value.get("ok").is_none() {
        value
            .as_object_mut()
            .expect("tool result object")
            .insert("ok".into(), Value::Bool(true));
    }
    value
}

pub fn tool_err(error: WorkspaceError) -> Value {
    json!({
        "ok": false,
        "status": "error",
        "summary": error.message(),
        "error": error.to_error_value()
    })
}

pub fn tool_err_code(
    code: &'static str,
    message: impl Into<String>,
    category: &'static str,
) -> Value {
    let message = message.into();
    json!({
        "ok": false,
        "status": "error",
        "summary": message.clone(),
        "error": {
            "code": code,
            "message": message,
            "category": category,
            "retryable": false,
            "details": {}
        }
    })
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

    /// 建一个工作区，并在隔离的 GLD_HOME 里放一份假的密钥文件。
    fn workspace_with_fake_secret_store() -> (tempfile::TempDir, Workspace, PathBuf) {
        crate::home::isolate_for_tests();
        let home = crate::home::data_home().expect("data home");
        std::fs::create_dir_all(home.join("data")).expect("mkdir");
        let secrets = home.join("data").join("profiles.json");
        std::fs::write(&secrets, r#"{"workspace_secrets":{}}"#).expect("write");

        let dir = tempfile::tempdir().expect("workspace");
        std::fs::write(dir.path().join("main.rs"), "fn main() {}\n").expect("file");
        let workspace = Workspace::new(dir.path().to_path_buf()).expect("workspace");
        (dir, workspace, secrets)
    }

    /// gld 自己的数据目录必须挡住，**哪怕越界读已经被放开**：
    /// 那里是所有工作区的 bearer_token / oauth_password 明文。
    ///
    /// 特意关掉 confine_reads 才是这条的意义所在——开着的话它被外层拦掉，
    /// 测不到数据目录这道独立的门。
    #[test]
    fn reading_the_gld_data_home_is_denied_even_with_confinement_off() {
        let (_dir, workspace, secrets) = workspace_with_fake_secret_store();
        let workspace = workspace.with_confined_reads(false);

        let error = workspace
            .resolve_read_path(&secrets.display().to_string())
            .expect_err("必须拒绝");

        assert!(
            matches!(error, WorkspaceError::Tool { code, .. } if code == "GLD_DATA_HOME_DENIED"),
            "错误码不对: {error:?}"
        );
    }

    /// 默认（confine_reads=true）下，读 Workspace 外面就被挡在门口。
    #[test]
    fn confined_reads_are_the_default_and_block_absolute_paths() {
        let (_dir, workspace, _) = workspace_with_fake_secret_store();
        let outside = tempfile::tempdir().expect("outside");
        let file = outside.path().join("neighbour.txt");
        std::fs::write(&file, "ok\n").expect("write");

        let error = workspace
            .resolve_read_path(&file.display().to_string())
            .expect_err("默认就该拒绝");
        let message = error.message();

        assert!(message.contains("只允许读 Workspace 内的文件"), "{message}");
        // 报错要给出关掉的办法：读隔壁仓库是正当需求。
        assert!(message.contains("confine-reads=false"), "{message}");
    }

    /// Workspace 里指向外面的软链也算越界——比的是 canonicalize 之后的路径。
    #[cfg(unix)]
    #[test]
    fn a_symlink_pointing_outside_does_not_slip_past_confinement() {
        let (dir, workspace, _) = workspace_with_fake_secret_store();
        let outside = tempfile::tempdir().expect("outside");
        let target = outside.path().join("secret.txt");
        std::fs::write(&target, "outside\n").expect("write");
        std::os::unix::fs::symlink(&target, dir.path().join("link.txt")).expect("symlink");

        assert!(workspace.resolve_read_path("link.txt").is_err());
    }

    /// 遍历类工具从外面走进数据目录也要挡住——
    /// resolve_read_path 只校验起点，WalkDir 之后会自己走下去。
    #[test]
    fn walking_into_the_gld_data_home_is_skipped() {
        let (_dir, workspace, secrets) = workspace_with_fake_secret_store();

        assert!(
            workspace.is_ignored_path(&secrets, true, true),
            "include_hidden=true 也不能把数据目录搜出来"
        );
    }

    /// 关掉 confine_reads 之后，Workspace 外的普通路径要读得到——
    /// 这个开关得真的有用，不能只是把"一律拒绝"换个错误码。
    #[test]
    fn turning_confinement_off_restores_reads_outside_the_workspace() {
        let (_dir, workspace, _) = workspace_with_fake_secret_store();
        let workspace = workspace.with_confined_reads(false);
        let outside = tempfile::tempdir().expect("outside");
        let file = outside.path().join("neighbour.txt");
        std::fs::write(&file, "ok\n").expect("write");

        assert!(workspace
            .resolve_read_path(&file.display().to_string())
            .is_ok());
        assert!(!workspace.is_ignored_path(&file, false, false));
    }
}
