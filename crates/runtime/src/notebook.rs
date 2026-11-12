//! Jupyter notebook 按 cell 读、按 cell 改。
//!
//! # 为什么不改 `read_file`
//!
//! `read_file` 现在对 `.ipynb` 返回的是磁盘上那份 JSON，模型能照着那段文本
//! 用普通补丁改它。把它换成 cell 视图，那些补丁就全部对不上了——这是行为
//! 变更，不是加法。所以 cell 视图是**另一个工具**，`read_file` 不动。
//! ccnm 在 P40 做了同样的取舍。
//!
//! # 照抄原生的哪些语义
//!
//! Claude Code 2.1.273 的 Read / NotebookEdit（ccnm P40 从它的打包代码里读出来的）：
//!
//! ```text
//! Read           每个 cell 一段 <cell id="…">；代码 cell 的输出跟在后面
//! NotebookEdit   cell_id、new_source、cell_type、edit_mode
//!                先按 id 找 cell，找不到时 "cell-N" 当序号 N
//!                替换代码 cell 会清空 outputs 和 execution_count
//!                insert 插在 cell_id 之后，不给 cell_id 就插在最前面
//!                nbformat ≥ 4.5 给新 cell 一个 8 位 id
//! ```
//!
//! 两处有意不同（和 ccnm 一致）：换了类型的 cell 会丢掉新类型不该有的键
//! （nbformat 的 schema 不允许 markdown cell 带 `outputs`，Claude Code 留着），
//! source 按 nbformat 的写法存成一行一条，这样 `git diff` 能看出改了哪几行。
//!
//! # 和 ccnm 的一处差别：图片
//!
//! ccnm 把 PNG/JPEG 输出当 MCP 图片块发给模型。gld 的工具结果是**单块**的
//! （`wrap_mcp_tool_result` 只有 view_image 走图片块），要发多块得改所有工具的
//! 传输形状。所以这里只标注"有一张多大的图"，不发图片。写进文档，不装作有。
//!
//! # 写回去
//!
//! nbformat 用 `json.dumps(sort_keys=True, indent=1, ensure_ascii=False)` 外加
//! 一个末尾换行。gld 的 `serde_json` 没开 `preserve_order`，所以键本来就是
//! 排序的，也不转义非 ASCII；缩进宽度和末尾换行从原文件读出来照用。于是
//! **没动过的 notebook 写回去是逐字节一样的**。已知的一处不同：metadata 里的
//! 浮点数，Python 写 `1e-05`，这里写 `1e-5`。

use serde_json::{json, Map, Value};

use crate::workspace::{tool_ok, Workspace, WorkspaceError};

/// 读和改的文件大小上限，和 ccnm 的 `MAX_EDIT_BYTES` 一样。
/// 带着输出的 notebook 很容易几 MB，太小的上限会让这个工具在真实项目里没用。
pub(crate) const MAX_NOTEBOOK_BYTES: u64 = 16 * 1024 * 1024;

/// 一个输出最多给多少字节。训练循环的进度条是单个输出上兆的常见原因。
const MAX_OUTPUT_BYTES: usize = 4 * 1024;

/// 读出来的一份 notebook，外加它在磁盘上是怎么排版的。
#[derive(Debug)]
pub(crate) struct Notebook {
    root: Map<String, Value>,
    /// 缩进几个空格；0 表示整份 JSON 只有一行。
    indent: usize,
    final_newline: bool,
}

impl Notebook {
    pub(crate) fn parse(bytes: &[u8], rel: &str) -> Result<Self, WorkspaceError> {
        let text = std::str::from_utf8(bytes)
            .map_err(|_| invalid(format!("{rel} is not valid UTF-8, so it is not a notebook")))?;
        let value: Value = serde_json::from_str(text).map_err(|error| {
            invalid(format!(
                "{rel} is not valid JSON ({error}); it may be truncated or still being written"
            ))
        })?;
        let Value::Object(root) = value else {
            return Err(not_a_notebook(rel));
        };
        if !root.get("cells").is_some_and(Value::is_array) {
            return Err(not_a_notebook(rel));
        }
        match root.get("nbformat").and_then(Value::as_u64) {
            Some(major) if major >= 4 => {}
            Some(major) => {
                return Err(invalid(format!(
                    "{rel} is nbformat {major}; only nbformat 4 is supported. Convert it first: jupyter nbconvert --to notebook --nbformat 4 {rel}"
                )))
            }
            None => return Err(not_a_notebook(rel)),
        }
        // 第二行前面有几个空格就是缩进宽度；只有一行说明这份 JSON 没有缩进。
        let indent = text
            .split('\n')
            .nth(1)
            .map_or(0, |line| line.len() - line.trim_start_matches(' ').len());
        Ok(Self {
            root,
            indent,
            final_newline: text.ends_with('\n'),
        })
    }

    pub(crate) fn cells(&self) -> &Vec<Value> {
        self.root["cells"].as_array().expect("parse 已经查过")
    }

    fn cells_mut(&mut self) -> &mut Vec<Value> {
        self.root
            .get_mut("cells")
            .and_then(Value::as_array_mut)
            .expect("parse 已经查过")
    }

    /// cell 是从 nbformat 4.5 开始有 id 的。
    fn has_ids(&self) -> bool {
        let major = self
            .root
            .get("nbformat")
            .and_then(Value::as_u64)
            .unwrap_or(4);
        let minor = self
            .root
            .get("nbformat_minor")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        major > 4 || minor >= 5
    }

    pub(crate) fn format(&self) -> String {
        let major = self
            .root
            .get("nbformat")
            .and_then(Value::as_u64)
            .unwrap_or(4);
        let minor = self
            .root
            .get("nbformat_minor")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        format!("{major}.{minor}")
    }

    pub(crate) fn language(&self) -> String {
        let meta = self.root.get("metadata");
        meta.and_then(|value| value.pointer("/language_info/name"))
            .or_else(|| meta.and_then(|value| value.pointer("/kernelspec/language")))
            .and_then(Value::as_str)
            .unwrap_or("python")
            .to_string()
    }

    pub(crate) fn to_bytes(&self) -> Vec<u8> {
        let value = Value::Object(self.root.clone());
        let mut out = Vec::new();
        if self.indent == 0 {
            serde_json::to_writer(&mut out, &value).expect("Value 总能序列化");
        } else {
            let indent = vec![b' '; self.indent];
            let formatter = serde_json::ser::PrettyFormatter::with_indent(&indent);
            let mut serializer = serde_json::Serializer::with_formatter(&mut out, formatter);
            serde::Serialize::serialize(&value, &mut serializer).expect("Value 总能序列化");
        }
        if self.final_newline {
            out.push(b'\n');
        }
        out
    }
}

fn invalid(message: String) -> WorkspaceError {
    WorkspaceError::invalid_argument(message)
}

fn not_a_notebook(rel: &str) -> WorkspaceError {
    invalid(format!(
        "{rel} is not a Jupyter notebook (no cells array or nbformat number)"
    ))
}

/// 一个 cell 的地址：它自己的 id，或者没有 id 的老 notebook 里的 `cell-N`
/// ——和 Claude Code 的回退规则一样。
fn cell_id(cell: &Value, index: usize) -> String {
    cell.get("id")
        .and_then(Value::as_str)
        .map_or_else(|| format!("cell-{index}"), str::to_string)
}

/// source 和输出文本可能是一个字符串，也可能是一串字符串。
fn joined(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => parts.iter().filter_map(Value::as_str).collect(),
        _ => String::new(),
    }
}

/// 渲染的结果。
pub(crate) struct Rendered {
    pub(crate) text: String,
    /// 这一次显示了哪些 cell（闭区间）；notebook 是空的时候是 None。
    pub(crate) shown: Option<(usize, usize)>,
    /// 还没显示完时，下一次从哪个 cell 开始。
    pub(crate) next_start_cell: Option<usize>,
}

/// 从第 `start` 个 cell 开始渲染，文本不超过 `max_bytes`。
pub(crate) fn render(notebook: &Notebook, start: usize, max_bytes: usize) -> Rendered {
    let cells = notebook.cells();
    let total = cells.len();
    let language = notebook.language();
    if start >= total {
        return Rendered {
            text: String::new(),
            shown: None,
            next_start_cell: None,
        };
    }

    let mut text = String::new();
    let mut next = None;
    let mut last = start;
    for (index, cell) in cells.iter().enumerate().skip(start) {
        let one = render_cell(cell, index, &language);
        // 第一个 cell 一定显示（哪怕它自己就超了）：否则一个巨大的 cell 会让
        // 整份 notebook 卡在这里，翻页翻不过去。
        if index > start && text.len() + one.len() > max_bytes {
            next = Some(index);
            break;
        }
        text.push_str(&one);
        last = index;
    }
    Rendered {
        text,
        shown: Some((start, last)),
        next_start_cell: next,
    }
}

fn render_cell(cell: &Value, index: usize, language: &str) -> String {
    let id = cell_id(cell, index);
    let kind = cell
        .get("cell_type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let mut head = format!("<cell id=\"{id}\" index=\"{index}\" type=\"{kind}\"");
    if kind == "code" {
        if let Some(count) = cell.get("execution_count").and_then(Value::as_u64) {
            head.push_str(&format!(" execution_count=\"{count}\""));
        }
        if language != "python" {
            head.push_str(&format!(" language=\"{language}\""));
        }
    }
    let mut source = joined(cell.get("source"));
    if !source.is_empty() && !source.ends_with('\n') {
        source.push('\n');
    }
    let mut text = format!("{head}>\n{source}</cell>\n");

    for output in cell
        .get("outputs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        text.push_str(&render_output(output, &id));
    }
    text
}

fn render_output(output: &Value, id: &str) -> String {
    let kind = output
        .get("output_type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let (body, image) = match kind {
        "stream" => (joined(output.get("text")), None),
        "execute_result" | "display_data" => {
            let data = output.get("data");
            (
                joined(data.and_then(|data| data.get("text/plain"))),
                data.and_then(image_note),
            )
        }
        "error" => {
            let name = output.get("ename").and_then(Value::as_str).unwrap_or("");
            let value = output.get("evalue").and_then(Value::as_str).unwrap_or("");
            let trace = output
                .get("traceback")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default();
            (strip_ansi(&format!("{name}: {value}\n{trace}")), None)
        }
        other => (format!("[a {other} output is not shown]"), None),
    };

    let mut attrs = format!("type=\"{kind}\"");
    if let Some(name) = output.get("name").and_then(Value::as_str) {
        attrs.push_str(&format!(" name=\"{name}\""));
    }
    let mut text = format!("<output cell=\"{id}\" {attrs}>\n");
    let body = body.trim_end_matches('\n');
    if body.len() > MAX_OUTPUT_BYTES {
        text.push_str(truncate_on_char_boundary(body, MAX_OUTPUT_BYTES));
        text.push_str(&format!(
            "\n[output cut at {MAX_OUTPUT_BYTES} of {} bytes; all of it is in the notebook's JSON, which read_file shows]",
            body.len()
        ));
    } else {
        text.push_str(body);
    }
    if !body.is_empty() {
        text.push('\n');
    }
    if let Some(note) = image {
        text.push_str(&format!("{note}\n"));
    }
    text.push_str("</output>\n");
    text
}

/// 输出里的图片：只说有多大，不发内容。
///
/// gld 的工具结果是单块的，发不了图文交错（ccnm 能，那边的结果是块列表）。
/// 与其发一段没人看得懂的 base64，不如如实说这里有一张图。
fn image_note(data: &Value) -> Option<String> {
    let object = data.as_object()?;
    for (mime, value) in object {
        if !mime.starts_with("image/") {
            continue;
        }
        let encoded = joined(Some(value));
        // base64 每 4 个字符还原 3 个字节，末尾的 '=' 是补位。
        let padding = encoded.chars().rev().take_while(|c| *c == '=').count();
        let bytes = encoded.len() / 4 * 3 - padding.min(2);
        return Some(format!(
            "[{mime}, about {bytes} bytes; one block per tool result here, so the image itself is not included]"
        ));
    }
    None
}

fn truncate_on_char_boundary(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// 去掉 ANSI 转义序列。Python 的 traceback 里全是颜色码，留着只会占地方。
fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        if ch != '\u{1b}' {
            out.push(ch);
            continue;
        }
        // ESC [ … 字母：吃到终止字母为止。别的 ESC 序列直接丢掉这个字符。
        if chars.as_str().starts_with('[') {
            chars.next();
            for next in chars.by_ref() {
                if next.is_ascii_alphabetic() {
                    break;
                }
            }
        }
    }
    out
}

/// 一次 cell 编辑，字段名照 Claude Code 的 NotebookEdit。
#[derive(Debug, Clone, Default)]
pub(crate) struct CellEdit {
    pub(crate) cell_id: Option<String>,
    pub(crate) new_source: Option<String>,
    pub(crate) cell_type: Option<String>,
    pub(crate) edit_mode: String,
}

impl CellEdit {
    pub(crate) fn from_value(value: &Value, at: &str) -> Result<Self, WorkspaceError> {
        let object = value
            .as_object()
            .ok_or_else(|| invalid(format!("{at}: each cell edit must be an object")))?;
        let text = |key: &str| {
            object
                .get(key)
                .and_then(Value::as_str)
                .map(str::to_string)
                .filter(|value| !value.is_empty())
        };
        let edit_mode = object
            .get("edit_mode")
            .and_then(Value::as_str)
            .unwrap_or("replace")
            .to_string();
        if !matches!(edit_mode.as_str(), "replace" | "insert" | "delete") {
            return Err(invalid(format!(
                "{at}: edit_mode must be replace, insert or delete"
            )));
        }
        if let Some(kind) = text("cell_type") {
            if !matches!(kind.as_str(), "code" | "markdown") {
                return Err(invalid(format!("{at}: cell_type must be code or markdown")));
            }
        }
        Ok(Self {
            cell_id: text("cell_id"),
            // new_source 允许是空字符串（清空一个 cell 是正当操作），
            // 所以这一格不能走上面那个 filter。
            new_source: object
                .get("new_source")
                .and_then(Value::as_str)
                .map(str::to_string),
            cell_type: text("cell_type"),
            edit_mode,
        })
    }
}

/// 按顺序把 `edits` 应用到 notebook 上，返回新的文件内容。
pub(crate) fn edit(bytes: &[u8], rel: &str, edits: &[CellEdit]) -> Result<Vec<u8>, WorkspaceError> {
    if edits.is_empty() {
        return Err(invalid(format!("{rel}: needs at least one cell edit")));
    }
    let mut notebook = Notebook::parse(bytes, rel)?;
    for (index, one) in edits.iter().enumerate() {
        apply_one(&mut notebook, rel, index, one)?;
    }
    let out = notebook.to_bytes();
    if out.len() as u64 > MAX_NOTEBOOK_BYTES {
        return Err(invalid(format!(
            "{rel} would become {} bytes, over the {MAX_NOTEBOOK_BYTES} limit",
            out.len()
        )));
    }
    Ok(out)
}

fn apply_one(
    notebook: &mut Notebook,
    rel: &str,
    n: usize,
    one: &CellEdit,
) -> Result<(), WorkspaceError> {
    let at = |what: &str| invalid(format!("{rel}: cells[{n}]: {what}"));
    let index = match one.cell_id.as_deref() {
        Some(id) => Some(find(notebook.cells(), id).ok_or_else(|| {
            let known = notebook
                .cells()
                .iter()
                .enumerate()
                .map(|(index, cell)| cell_id(cell, index))
                .collect::<Vec<_>>();
            at(&format!(
                "no cell has id {id}; the cells are {}",
                if known.is_empty() {
                    "none".to_string()
                } else {
                    known.join(", ")
                }
            ))
        })?),
        None => None,
    };

    match one.edit_mode.as_str() {
        "delete" => {
            let index = index.ok_or_else(|| at("delete needs cell_id"))?;
            notebook.cells_mut().remove(index);
        }
        "replace" => {
            let index = index.ok_or_else(|| at("replace needs cell_id"))?;
            let source = one
                .new_source
                .as_deref()
                .ok_or_else(|| at("replace needs new_source"))?;
            let cell = notebook.cells_mut()[index]
                .as_object_mut()
                .ok_or_else(|| at("that cell is not a JSON object"))?;
            cell.insert("source".into(), lines(source));
            // 不给 cell_type 就保持原来的类型，raw 也一样。
            let kind = one.cell_type.clone().unwrap_or_else(|| {
                cell.get("cell_type")
                    .and_then(Value::as_str)
                    .unwrap_or("code")
                    .to_string()
            });
            set_type(cell, &kind);
        }
        _ => {
            let kind = one
                .cell_type
                .clone()
                .ok_or_else(|| at("insert needs cell_type"))?;
            let source = one
                .new_source
                .as_deref()
                .ok_or_else(|| at("insert needs new_source"))?;
            let mut cell = Map::new();
            cell.insert("cell_type".into(), Value::from(kind.clone()));
            if notebook.has_ids() {
                cell.insert("id".into(), Value::from(new_id(notebook.cells())));
            }
            cell.insert("metadata".into(), Value::Object(Map::new()));
            cell.insert("source".into(), lines(source));
            set_type(&mut cell, &kind);
            let position = index.map_or(0, |index| index + 1);
            notebook.cells_mut().insert(position, Value::Object(cell));
        }
    }
    Ok(())
}

/// 按 id 找 cell；没有哪个 cell 用这个 id 时，`cell-N` 当序号。
fn find(cells: &[Value], id: &str) -> Option<usize> {
    cells
        .iter()
        .position(|cell| cell.get("id").and_then(Value::as_str) == Some(id))
        .or_else(|| {
            id.strip_prefix("cell-")
                .and_then(|n| n.parse::<usize>().ok())
                .filter(|n| *n < cells.len())
        })
}

/// 把 cell 设成某个类型：代码 cell 得到空的 outputs 和空的 execution_count
/// （Claude Code 每次替换都这么做——旧输出配新代码是骗人的）；markdown cell
/// 则把这两个键去掉，nbformat 的 schema 不允许它带。
fn set_type(cell: &mut Map<String, Value>, kind: &str) {
    cell.insert("cell_type".into(), Value::from(kind));
    if kind == "code" {
        cell.insert("execution_count".into(), Value::Null);
        cell.insert("outputs".into(), Value::Array(Vec::new()));
    } else {
        cell.remove("execution_count");
        cell.remove("outputs");
    }
}

/// nbformat 存 source 的写法：一行一条，除了最后一行都带着换行符。
/// 这样 `git diff` 看得出改了哪几行，而不是整段 source 一条大字符串。
fn lines(source: &str) -> Value {
    Value::Array(source.split_inclusive('\n').map(Value::from).collect())
}

/// 八个十六进制字符，和 notebook 里已有的 id 都不重样。
fn new_id(cells: &[Value]) -> String {
    loop {
        let id = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
        if !cells
            .iter()
            .any(|cell| cell.get("id").and_then(Value::as_str) == Some(id.as_str()))
        {
            return id;
        }
    }
}

/// `read_notebook`：把一份 notebook 按 cell 读出来。
///
/// 和 `read_file` 并存，互不影响：那边给的是磁盘上的 JSON（已经有人照着它
/// 用普通补丁改 notebook），这边给的是 cell 视图。
pub fn read_notebook(ws: &Workspace, args: &Value) -> Result<Value, WorkspaceError> {
    let path = args
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| WorkspaceError::invalid_argument("path is required"))?;
    let start_cell = args.get("start_cell").and_then(Value::as_u64).unwrap_or(0) as usize;
    let max_bytes = gld_protocol::tool_args::bounded(args, "read_notebook", "max_bytes") as usize;

    let resolved = ws.resolve_read_path(path)?;
    let meta = std::fs::metadata(&resolved.path)
        .map_err(|_| WorkspaceError::not_found(format!("File not found: {path}")))?;
    if !meta.is_file() {
        return Err(WorkspaceError::invalid_argument(format!(
            "{} is not a regular file",
            resolved.display
        )));
    }
    if meta.len() > MAX_NOTEBOOK_BYTES {
        return Err(invalid(format!(
            "{} is {} bytes; notebooks up to {MAX_NOTEBOOK_BYTES} bytes are read as cells. Clear its outputs first (jupyter nbconvert --clear-output --inplace {}), or read the JSON with read_file",
            resolved.display,
            meta.len(),
            resolved.display
        )));
    }
    let version = crate::workspace::file_version(&meta);
    let bytes = std::fs::read(&resolved.path).map_err(|error| WorkspaceError::Tool {
        code: "IO_ERROR",
        message: format!("Failed to read notebook: {error}"),
        category: "runtime",
        retryable: false,
    })?;
    let notebook = Notebook::parse(&bytes, &resolved.display)?;
    let total = notebook.cells().len();
    let rendered = render(&notebook, start_cell, max_bytes);

    let mut warnings: Vec<String> = Vec::new();
    if rendered.shown.is_none() && total > 0 {
        warnings.push(format!(
            "start_cell {start_cell} is past the last cell ({})",
            total - 1
        ));
    }
    Ok(tool_ok(json!({
        "path": resolved.display,
        // 改它的时候把这个版本放进 apply_patch 的 expected_versions。
        "version": version,
        "nbformat": notebook.format(),
        "language": notebook.language(),
        "total_cells": total,
        "start_cell": rendered.shown.map(|(start, _)| start),
        "end_cell": rendered.shown.map(|(_, end)| end),
        "next_start_cell": rendered.next_start_cell,
        "truncated": rendered.next_start_cell.is_some(),
        "content": rendered.text,
        "warnings": warnings
    })))
}

/// `apply_patch` 的 `notebook_edits`：一份 notebook 的一批 cell 编辑。
pub(crate) struct NotebookEdits {
    pub(crate) path: String,
    pub(crate) cells: Vec<CellEdit>,
}

/// 读 `notebook_edits` 参数。没给就是 None——这个参数是加法，老调用一个字
/// 都不用改。
pub(crate) fn notebook_edits(args: &Value) -> Result<Option<Vec<NotebookEdits>>, WorkspaceError> {
    let Some(raw) = args.get("notebook_edits") else {
        return Ok(None);
    };
    if raw.is_null() {
        return Ok(None);
    }
    let items = raw.as_array().ok_or_else(|| {
        WorkspaceError::invalid_argument(
            "notebook_edits must be an array of {path, cells:[…]} objects",
        )
    })?;
    let mut out = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        let at = format!("notebook_edits[{index}]");
        let object = item
            .as_object()
            .ok_or_else(|| WorkspaceError::invalid_argument(format!("{at} must be an object")))?;
        let path = object
            .get("path")
            .and_then(Value::as_str)
            .filter(|path| !path.is_empty())
            .ok_or_else(|| WorkspaceError::invalid_argument(format!("{at}: path is required")))?
            .to_string();
        let cells = object
            .get("cells")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                WorkspaceError::invalid_argument(format!("{at}: cells must be an array"))
            })?
            .iter()
            .enumerate()
            .map(|(n, cell)| CellEdit::from_value(cell, &format!("{at}.cells[{n}]")))
            .collect::<Result<Vec<_>, _>>()?;
        if cells.is_empty() {
            return Err(WorkspaceError::invalid_argument(format!(
                "{at}: cells must have at least one edit"
            )));
        }
        out.push(NotebookEdits { path, cells });
    }
    if out.is_empty() {
        return Ok(None);
    }
    Ok(Some(out))
}

#[cfg(test)]
mod tests;
