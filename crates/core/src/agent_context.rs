use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use sha2::{Digest, Sha256};
use walkdir::WalkDir;

pub const AUTO_SOURCE: &str = "auto";
const DISABLED_SOURCE: &str = "disabled";
const DISCOVERABLE_SOURCES: &[&str] = &[
    "codex", "claude", "cursor", "copilot", "opencode", "zcode", "reasonix",
];

#[derive(Debug, Clone, Default)]
pub struct AgentContextRuntimeConfig {
    pub instruction_sources: Vec<String>,
    pub skill_sources: Vec<String>,
    pub custom_instruction_paths: String,
    pub custom_skill_paths: String,
}

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstructionDocument {
    pub provider: String,
    pub path: String,
    pub scope: String,
    pub content: String,
}

#[derive(Debug, Clone, Default, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillDescriptor {
    pub id: String,
    pub name: String,
    pub description: String,
    pub provider: String,
    pub path: String,
    pub scope: String,
    /// SKILL.md 整个文件的 sha256。`id` 只由路径决定，同一个路径下内容换了
    /// id 不变——拿这个才分得清"模型读到的是不是现在这一版"（跨仓评审 X08
    /// 要的"内容版本"）。
    #[serde(default)]
    pub content_sha256: String,
}

#[derive(Debug, Clone, Default)]
pub struct SkillEntry {
    pub descriptor: SkillDescriptor,
    pub body: String,
    /// skill 自己的目录（规范化过的绝对路径），`get_skill` 读附件只许在它下面。
    ///
    /// `None`：SKILL.md 直接放在扫描根上（或者经符号链接指到了根外面）——它的
    /// "目录"就是整个根，放开等于放开整个根，所以不给。
    pub dir: Option<PathBuf>,
    /// 读这份文件时模型该知道的事：被截断了、原生客户端会整段丢弃这份
    /// frontmatter、有键写了几遍。
    pub notes: Vec<String>,
    /// 来源是用户明确配置的（`--skill-sources claude`、自定义路径），不是
    /// auto 默认扫到的。工作区外的附件只在这时（或关掉 confine-reads 时）才读。
    pub explicit_source: bool,
}

/// 看起来是 skill、但没进目录的文件，和原因。
///
/// 以前这些是静默跳过的：作者写了 skill、AI 看不见、也没有任何地方说为什么。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkippedSkill {
    pub provider: String,
    pub path: String,
    pub scope: String,
    pub reason: String,
}

/// 一次扫描的结果：收进来的，和没收进来的。
#[derive(Debug, Clone, Default)]
pub struct SkillScan {
    pub skills: Vec<SkillEntry>,
    pub skipped: Vec<SkippedSkill>,
}

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentContextSnapshot {
    pub instructions: Vec<InstructionDocument>,
    pub skills: Vec<SkillDescriptor>,
    pub rendered_instructions: String,
    /// 这些说明里，真正会注入给 AI 的那几份的路径。
    ///
    /// 扫描到 ≠ 会注入：compact 工具集（默认值）为了省 token，只留工作区里的
    /// AGENTS.md 一份。不分开报的话，`gld context` 会把 `.cursorrules`
    /// 列得像是生效了——人照着改了半天没反应，也想不到是这里。
    pub injected_instruction_paths: Vec<String>,
    /// Skill 目录会不会被写进给 AI 的说明里。
    pub skills_injected: bool,
    /// 目录里**实际列了几条**。
    ///
    /// compact 档的目录有字符预算，扫到 12 条可能只列了 8 条。没列进去的不是
    /// 失效了——模型调 `list_skills` 照样拿得到全部——但说明里看不见，模型
    /// 主动想起它们的机会就小。`gld context` 照这个数打勾。
    #[serde(default)]
    pub skills_listed: usize,
    /// 扫到了但没收进来的 skill，各带原因。
    #[serde(default)]
    pub skills_skipped: Vec<SkippedSkill>,
}

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSourceDetection {
    pub provider: String,
    pub instruction_paths: Vec<String>,
    pub skill_paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GlobalAgentContextScan {
    pub sources: Vec<AgentSourceDetection>,
    pub detected_instruction_sources: Vec<String>,
    pub detected_skill_sources: Vec<String>,
}

pub fn merge_source_lists(global: &[String], workspace: &[String]) -> Vec<String> {
    let global = normalized_source_list(global);
    let workspace = normalized_source_list(workspace);

    if !workspace.is_empty() {
        if workspace.iter().any(|source| source == AUTO_SOURCE) {
            return vec![AUTO_SOURCE.to_string()];
        }
        if workspace.iter().any(|source| source == DISABLED_SOURCE) {
            return vec![DISABLED_SOURCE.to_string()];
        }
    }

    let mut result = Vec::new();
    if workspace.is_empty() {
        result.extend(global);
        return result;
    }

    for source in global
        .iter()
        .filter(|source| *source != AUTO_SOURCE && *source != DISABLED_SOURCE)
        .chain(workspace.iter())
    {
        if !result.iter().any(|item| item == source) {
            result.push(source.clone());
        }
    }
    result
}

/// 在这个工具集下，扫描到的说明里哪几份真的会注入。
///
/// compact 是默认工具集，它为了省 token 只保留工作区里的 AGENTS.md 一份。
/// 这条规则以前只写在 `ToolContext::current_ai_instructions` 里，
/// 而 `gld context` 自己扫一遍全报出来——两边口径不一致，命令就在撒谎。
/// 现在两处都调这个函数。
pub fn effective_instructions(
    documents: &[InstructionDocument],
    tool_profile: &str,
) -> Vec<InstructionDocument> {
    if tool_profile != "compact" {
        return documents.to_vec();
    }
    documents
        .iter()
        .filter(|document| {
            document.scope == "workspace"
                && (document.path.ends_with("AGENTS.md")
                    || document.path.ends_with("AGENTS.override.md"))
        })
        .take(1)
        .cloned()
        .collect()
}

/// 这个工具集会不会把 Skill 目录写进给 AI 的说明。
///
/// 现在所有档都会（2026-09-19 起）。compact 以前一条都不给，`list_skills` /
/// `get_skill` 也不暴露，等于 Skill 整体不可用；现在改成"给一段有上限的目录"，
/// 上限见 [`COMPACT_SKILL_CATALOG_CHARS`]。
///
/// 函数留着，是因为"有没有 skill 进说明"这件事 `gld context` 和 MCP 握手两边
/// 都要问，将来再出现某个档不带目录时，改这一处就够。
pub fn skills_are_injected(_tool_profile: &str) -> bool {
    true
}

pub fn discover(
    workspace_root: &Path,
    instruction_sources: &[String],
    skill_sources: &[String],
    custom_instruction_paths: &str,
    custom_skill_paths: &str,
    tool_profile: &str,
) -> AgentContextSnapshot {
    let instructions = discover_instructions(
        workspace_root,
        instruction_sources,
        custom_instruction_paths,
    );
    let SkillScan {
        skills: skill_entries,
        skipped: skills_skipped,
    } = scan_skills(workspace_root, skill_sources, custom_skill_paths);
    let skills = skill_entries
        .iter()
        .map(|entry| entry.descriptor.clone())
        .collect::<Vec<_>>();
    let rendered_instructions = render_instruction_documents(&instructions);
    let injected_instruction_paths = effective_instructions(&instructions, tool_profile)
        .into_iter()
        .map(|document| document.path)
        .collect();
    // 目录里实际列了几条，用的是握手时同一个渲染函数——分开算，`gld context`
    // 迟早会报一个和模型看到的不一样的数。
    let skills_listed = render_skill_catalog_for_profile(&skill_entries, tool_profile).listed;
    AgentContextSnapshot {
        instructions,
        skills,
        rendered_instructions,
        injected_instruction_paths,
        skills_injected: skills_are_injected(tool_profile),
        skills_listed,
        skills_skipped,
    }
}

pub fn scan_global_agent_context() -> GlobalAgentContextScan {
    let Some(home) = home_dir() else {
        return GlobalAgentContextScan {
            sources: Vec::new(),
            detected_instruction_sources: Vec::new(),
            detected_skill_sources: Vec::new(),
        };
    };

    scan_global_agent_context_at(&home)
}

fn scan_global_agent_context_at(home: &Path) -> GlobalAgentContextScan {
    let mut sources = Vec::new();
    let mut detected_instruction_sources = Vec::new();
    let mut detected_skill_sources = Vec::new();

    for provider in DISCOVERABLE_SOURCES {
        let instruction_paths = discover_global_instruction_paths(home, provider);
        let skill_paths = discover_global_skill_paths(home, provider);

        if !instruction_paths.is_empty() {
            detected_instruction_sources.push((*provider).to_string());
        }
        if !skill_paths.is_empty() {
            detected_skill_sources.push((*provider).to_string());
        }
        if !instruction_paths.is_empty() || !skill_paths.is_empty() {
            sources.push(AgentSourceDetection {
                provider: (*provider).to_string(),
                instruction_paths,
                skill_paths,
            });
        }
    }

    GlobalAgentContextScan {
        sources,
        detected_instruction_sources,
        detected_skill_sources,
    }
}

pub fn discover_instructions(
    workspace_root: &Path,
    sources: &[String],
    custom_paths: &str,
) -> Vec<InstructionDocument> {
    let (sources, auto_enabled) = effective_sources(sources);
    let mut candidates = Vec::<(String, PathBuf, &'static str)>::new();
    for raw_source in &sources {
        let provider = normalize_provider(raw_source);
        match provider.as_str() {
            "codex" => {
                add_home_candidate(&mut candidates, &provider, ".codex/AGENTS.md");
                let override_path = workspace_root.join("AGENTS.override.md");
                if override_path.is_file() {
                    candidates.push((provider.clone(), override_path, "workspace"));
                } else {
                    candidates.push((
                        provider.clone(),
                        workspace_root.join("AGENTS.md"),
                        "workspace",
                    ));
                }
            }
            "claude" => {
                add_home_candidate(&mut candidates, &provider, ".claude/CLAUDE.md");
                candidates.push((
                    provider.clone(),
                    workspace_root.join("CLAUDE.md"),
                    "workspace",
                ));
            }
            "cursor" => {
                let global_rules = home_dir().map(|home| home.join(".cursor/rules"));
                if let Some(rules) = global_rules.filter(|rules| rules.is_dir()) {
                    for entry in WalkDir::new(rules)
                        .max_depth(6)
                        .into_iter()
                        .filter_map(Result::ok)
                    {
                        let path = entry.path();
                        if path.is_file()
                            && matches!(
                                path.extension().and_then(|value| value.to_str()),
                                Some("mdc") | Some("md")
                            )
                            && cursor_rule_is_always_apply(path)
                        {
                            candidates.push((provider.clone(), path.to_path_buf(), "global"));
                        }
                    }
                }
                candidates.push((
                    provider.clone(),
                    workspace_root.join("AGENTS.md"),
                    "workspace",
                ));
                candidates.push((
                    provider.clone(),
                    workspace_root.join(".cursorrules"),
                    "workspace",
                ));
                let rules = workspace_root.join(".cursor/rules");
                if rules.is_dir() {
                    for entry in WalkDir::new(rules)
                        .max_depth(6)
                        .into_iter()
                        .filter_map(Result::ok)
                    {
                        let path = entry.path();
                        if path.is_file()
                            && matches!(
                                path.extension().and_then(|value| value.to_str()),
                                Some("mdc") | Some("md")
                            )
                            && cursor_rule_is_always_apply(path)
                        {
                            candidates.push((provider.clone(), path.to_path_buf(), "workspace"));
                        }
                    }
                }
            }
            "copilot" => candidates.push((
                provider.clone(),
                workspace_root.join(".github/copilot-instructions.md"),
                "workspace",
            )),
            "opencode" => {
                add_home_candidate(&mut candidates, &provider, ".config/opencode/AGENTS.md");
                candidates.push((
                    provider.clone(),
                    workspace_root.join("AGENTS.md"),
                    "workspace",
                ));
                if !workspace_root.join("AGENTS.md").is_file() {
                    candidates.push((
                        provider.clone(),
                        workspace_root.join("CLAUDE.md"),
                        "workspace",
                    ));
                }
            }
            "zcode" => {
                add_home_candidate(&mut candidates, &provider, ".zcode/AGENTS.md");
                candidates.push((
                    provider.clone(),
                    workspace_root.join("AGENTS.md"),
                    "workspace",
                ));
            }
            "reasonix" => {
                for name in ["REASONIX.md", "AGENTS.md", "CLAUDE.md"] {
                    add_home_candidate(&mut candidates, &provider, &format!(".reasonix/{name}"));
                    let global_local = name.trim_end_matches(".md").to_string() + ".local.md";
                    add_home_candidate(
                        &mut candidates,
                        &provider,
                        &format!(".reasonix/{global_local}"),
                    );
                    candidates.push((provider.clone(), workspace_root.join(name), "workspace"));
                    let local = name.trim_end_matches(".md").to_string() + ".local.md";
                    candidates.push((provider.clone(), workspace_root.join(local), "workspace"));
                }
            }
            "custom" => {}
            _ => {}
        }
    }

    if auto_enabled {
        add_auto_instruction_candidates(&mut candidates, workspace_root);
    }

    if sources
        .iter()
        .any(|source| normalize_provider(source) == "custom")
        || (auto_enabled && !custom_paths.trim().is_empty())
    {
        for path in split_config_paths(custom_paths) {
            candidates.push((
                "custom".into(),
                resolve_config_path(workspace_root, &path),
                "custom",
            ));
        }
    }

    let mut seen_paths = HashSet::new();
    let mut seen_content = HashSet::new();
    let mut result = Vec::new();
    for (provider, path, scope) in candidates {
        if !path.is_file() {
            continue;
        }
        let canonical = path.canonicalize().unwrap_or_else(|_| path.clone());
        let path_key = canonical.to_string_lossy().to_string();
        if !seen_paths.insert(path_key) {
            continue;
        }
        let Ok(content) = fs::read_to_string(&canonical) else {
            continue;
        };
        let content = content.trim().to_string();
        if content.is_empty() || !seen_content.insert(hex_hash(content.as_bytes())) {
            continue;
        }
        result.push(InstructionDocument {
            provider,
            path: display_path(workspace_root, &canonical),
            scope: scope.into(),
            content,
        });
    }
    result
}

pub fn discover_skills(
    workspace_root: &Path,
    sources: &[String],
    custom_paths: &str,
) -> Vec<SkillEntry> {
    scan_skills(workspace_root, sources, custom_paths).skills
}

/// SKILL.md 超过这么多字符，正文只留这么多（整份文件 `get_skill` 的附件读法
/// 还拿得到，上限更大）。
const MAX_SKILL_CHARS: usize = 100 * 1024;

/// 扫描 skill，收不进来的也记下原因。
pub fn scan_skills(workspace_root: &Path, sources: &[String], custom_paths: &str) -> SkillScan {
    let (sources, auto_enabled) = effective_sources(sources);
    let mut roots = Vec::<(String, PathBuf, &'static str)>::new();
    for raw_source in &sources {
        let provider = normalize_provider(raw_source);
        match provider.as_str() {
            "codex" => {
                for root in [".agents/skills", ".codex/skills"] {
                    add_home_skill_root(&mut roots, &provider, root);
                    add_skill_root(
                        &mut roots,
                        &provider,
                        workspace_root.join(root),
                        "workspace",
                    );
                }
            }
            "claude" => {
                for root in [".claude/skills", ".agents/skills"] {
                    add_home_skill_root(&mut roots, &provider, root);
                    add_skill_root(
                        &mut roots,
                        &provider,
                        workspace_root.join(root),
                        "workspace",
                    );
                }
            }
            "cursor" => {
                for root in [".cursor/skills", ".agents/skills"] {
                    add_home_skill_root(&mut roots, &provider, root);
                    add_skill_root(
                        &mut roots,
                        &provider,
                        workspace_root.join(root),
                        "workspace",
                    );
                }
            }
            "copilot" => {
                for root in [".github/skills", ".claude/skills", ".agents/skills"] {
                    add_home_skill_root(&mut roots, &provider, root);
                    add_skill_root(
                        &mut roots,
                        &provider,
                        workspace_root.join(root),
                        "workspace",
                    );
                }
            }
            "opencode" => {
                for root in [".opencode/skills", ".claude/skills", ".agents/skills"] {
                    add_home_skill_root(&mut roots, &provider, root);
                    add_skill_root(
                        &mut roots,
                        &provider,
                        workspace_root.join(root),
                        "workspace",
                    );
                }
                add_home_skill_root(&mut roots, &provider, ".config/opencode/skills");
            }
            "zcode" => {
                add_home_skill_root(&mut roots, &provider, ".zcode/skills");
                add_skill_root(
                    &mut roots,
                    &provider,
                    workspace_root.join(".zcode/skills"),
                    "workspace",
                );
            }
            "reasonix" => {
                for root in [
                    ".reasonix/skills",
                    ".agents/skills",
                    ".agent/skills",
                    ".claude/skills",
                ] {
                    add_home_skill_root(&mut roots, &provider, root);
                    add_skill_root(
                        &mut roots,
                        &provider,
                        workspace_root.join(root),
                        "workspace",
                    );
                }
            }
            "custom" => {}
            _ => {}
        }
    }

    if sources
        .iter()
        .any(|source| normalize_provider(source) == "custom")
        || (auto_enabled && !custom_paths.trim().is_empty())
    {
        for path in split_config_paths(custom_paths) {
            add_skill_root(
                &mut roots,
                "custom",
                resolve_config_path(workspace_root, &path),
                "custom",
            );
        }
    }

    let mut candidates = Vec::<Candidate>::new();
    for (provider, root, scope) in roots {
        collect_skill_candidates(&mut candidates, &provider, &root, scope, 4);
    }
    if auto_enabled {
        collect_auto_workspace_skills(&mut candidates, workspace_root);
    }

    let mut seen_paths = HashSet::new();
    // 内容哈希 → 已经收进来的那一份的路径。
    let mut seen_content = HashMap::<String, String>::new();
    let mut result = Vec::new();
    let mut skipped = Vec::new();
    for candidate in candidates {
        let canonical = candidate
            .path
            .canonicalize()
            .unwrap_or_else(|_| candidate.path.clone());
        let path_key = canonical.to_string_lossy().to_string();
        // 同一个文件从两个来源各扫到一次（`.agents/skills` 同时属于好几家），
        // 那是同一个 skill，不是被跳过的第二个。
        if !seen_paths.insert(path_key.clone()) {
            continue;
        }
        let path = display_path(workspace_root, &canonical);
        let mut skip = |reason: String| {
            skipped.push(SkippedSkill {
                provider: candidate.provider.clone(),
                path: path.clone(),
                scope: candidate.scope.into(),
                reason,
            })
        };
        let raw = match fs::read_to_string(&canonical) {
            Ok(raw) => raw,
            Err(error) => {
                skip(format!("cannot read it: {error}"));
                continue;
            }
        };
        let content_sha256 = hex_hash(raw.as_bytes());
        let mut notes = Vec::new();
        let served = if raw.chars().count() > MAX_SKILL_CHARS {
            notes.push(format!(
                "SKILL.md is {} bytes; only its first {MAX_SKILL_CHARS} characters are in this content",
                raw.len()
            ));
            raw.chars().take(MAX_SKILL_CHARS).collect()
        } else {
            raw
        };
        let parsed = match parse_skill(&served, &canonical) {
            Ok(parsed) => parsed,
            Err(reason) => {
                skip(reason);
                continue;
            }
        };
        if let Some(first) = seen_content.get(&content_sha256) {
            skip(format!("same content as {first}, which is listed"));
            continue;
        }
        seen_content.insert(content_sha256.clone(), path.clone());
        notes.extend(parsed.notes);
        let id = hex_hash(path_key.as_bytes())[..16].to_string();
        result.push(SkillEntry {
            descriptor: SkillDescriptor {
                id,
                name: parsed.name,
                description: parsed.description,
                provider: candidate.provider,
                path,
                scope: candidate.scope.into(),
                content_sha256,
            },
            body: parsed.body,
            dir: skill_dir(&canonical, &candidate.root),
            notes,
            explicit_source: !auto_enabled,
        });
    }
    // 项目自己的 skill 排前面。发现顺序是按来源来的，主目录那批（`global`）
    // 先加，于是项目里的排在后面——compact 档的目录有字符预算，那样一来
    // 这个项目专有的 skill 会被机器上装的通用 skill 挤掉。稳定排序，同一档
    // 内部的顺序不变。
    result.sort_by_key(|entry| u8::from(entry.descriptor.scope == "global"));
    SkillScan {
        skills: result,
        skipped,
    }
}

/// 一个待读的 SKILL.md，以及它是从哪个根下面找到的。
struct Candidate {
    provider: String,
    path: PathBuf,
    scope: &'static str,
    root: PathBuf,
}

/// 附件能读的目录：SKILL.md 所在的目录，但必须在发现它的根**里面**、且不是
/// 根本身。
///
/// 为什么不直接用 SKILL.md 的上一级：`~/.claude/skills/SKILL.md` 的上一级是整个
/// skills 根，自定义根配成主目录时就是整个主目录；一个指向别处的符号链接
/// SKILL.md 的上一级可以是任何地方。跨仓评审 X08：附件只授权已发现的 skill
/// 目录，不能为了让用户级附件可读就放开整个 HOME。
fn skill_dir(skill_md: &Path, root: &Path) -> Option<PathBuf> {
    let dir = skill_md.parent()?;
    let root = root.canonicalize().ok()?;
    (dir != root && dir.starts_with(&root)).then(|| dir.to_path_buf())
}

pub fn render_instruction_documents(documents: &[InstructionDocument]) -> String {
    if documents.is_empty() {
        return String::new();
    }
    let mut out = String::from("Repository / IDE instructions:\n");
    for document in documents {
        out.push_str(&format!(
            "\n## [{}] {}\n{}\n",
            document.provider, document.path, document.content
        ));
    }
    out.trim().to_string()
}

/// compact 档能给 skill 目录的字符数。
///
/// compact 是默认档，存在的理由就是省 token；但"一个都不给"的结果是模型根本
/// 不知道这个项目有 skill（v3 方案 3.3 节）。折中是给一段有上限的目录，放不下
/// 的明说还有几个、去哪儿看——模型照样能用 `list_skills` 拿到全部。
///
/// 1200 字符大约是 8–12 条（每条被截到 120 字符）。
const COMPACT_SKILL_CATALOG_CHARS: usize = 1_200;

/// compact 档下每条描述截到多少字符。
const COMPACT_SKILL_DESCRIPTION_CHARS: usize = 120;

/// 渲染好的 skill 目录，外加"目录里实际列了几条"。
///
/// 条数要带出来，是因为 `gld context` 得如实报：说明里列了 8 条、扫到 12 条，
/// 剩下 4 条调 `list_skills` 能拿到。两边各算一遍迟早对不上。
pub struct SkillCatalog {
    pub text: String,
    pub listed: usize,
}

pub fn render_skill_catalog_for_profile(skills: &[SkillEntry], tool_profile: &str) -> SkillCatalog {
    if skills.is_empty() {
        return SkillCatalog {
            text: String::new(),
            listed: 0,
        };
    }
    let compact = tool_profile == "compact";
    let description_chars = if compact {
        COMPACT_SKILL_DESCRIPTION_CHARS
    } else {
        250
    };
    let budget = compact.then_some(COMPACT_SKILL_CATALOG_CHARS);

    let mut out = String::from("Available skills are loaded on demand. Use list_skills to inspect them and get_skill with a skill id to load the full SKILL.md.\n");
    let mut listed = 0;
    for skill in skills.iter().take(50) {
        let description = skill
            .descriptor
            .description
            .chars()
            .take(description_chars)
            .collect::<String>();
        let line = format!(
            "- {}: {} (provider: {}, id: {})\n",
            skill.descriptor.name, description, skill.descriptor.provider, skill.descriptor.id
        );
        // 预算用完就停，但至少给一条：一条都不给等于没有目录。
        if let Some(budget) = budget {
            if listed > 0 && out.chars().count() + line.chars().count() > budget {
                break;
            }
        }
        out.push_str(&line);
        listed += 1;
    }
    if listed < skills.len() {
        out.push_str(&format!(
            "({} more not listed here; call list_skills to see all {}.)\n",
            skills.len() - listed,
            skills.len()
        ));
    }
    SkillCatalog {
        text: out.trim().to_string(),
        listed,
    }
}

/// 读一份 SKILL.md 的 name、description 和正文。
///
/// frontmatter 交给 `toexec-skill`（和 ccnm 共用的那一份）。gld 原来是逐行找
/// `key:` 前缀，于是官方 skill 里常见的
///
/// ```text
/// description: >
///   Use this when …
/// ```
///
/// 读出来的描述就是一个 `>`——模型看到的目录里，这条 skill 等于没有描述，
/// 也就永远不会被选中（v3 方案 3.3 节点名的缺陷）。
///
/// frontmatter 读不下去（引号没闭合、缩进里有 tab……）时**整条不收**：没有
/// 描述的 skill 放进目录也没有用，而且我们不猜作者想写什么。不收的原因交给
/// 调用方记进 `skipped`，不再静默。
fn parse_skill(raw: &str, path: &Path) -> Result<ParsedSkill, String> {
    let (frontmatter, body) = toexec_skill::frontmatter::split(raw);
    let parsed = toexec_skill::frontmatter::parse(frontmatter.unwrap_or_default())
        .map_err(|error| error.to_string())?;
    let name = parsed
        .text("name")
        .map(str::to_string)
        .or_else(|| path.parent()?.file_name()?.to_str().map(str::to_string))
        .filter(|name| !name.trim().is_empty())
        .ok_or("no name in the frontmatter and no directory to take one from")?;
    let description = parsed
        .text("description")
        .ok_or("no description in the frontmatter")?;
    // Agent Skills 规范的上限（agentskills.io/specification），Codex 超了也不收。
    let chars = description.chars().count();
    if chars > 1024 {
        return Err(format!(
            "description is {chars} characters; the limit is 1024"
        ));
    }
    let mut notes = Vec::new();
    if parsed.reading() == toexec_skill::Reading::Lenient {
        notes.push("this frontmatter is not valid YAML: native Claude Code ignores all of it (name, description, switches); it was read leniently here".into());
    }
    for group in parsed.duplicates() {
        let lines: Vec<String> = group.iter().map(|(_, line)| line.to_string()).collect();
        notes.push(format!(
            "the frontmatter sets \"{}\" more than once (lines {}); the last one counts",
            group[group.len() - 1].0,
            lines.join(", ")
        ));
    }
    Ok(ParsedSkill {
        name: name.trim().to_string(),
        description: description.to_string(),
        body: body.trim().to_string(),
        notes,
    })
}

struct ParsedSkill {
    name: String,
    description: String,
    body: String,
    notes: Vec<String>,
}

fn cursor_rule_is_always_apply(path: &Path) -> bool {
    let Ok(raw) = fs::read_to_string(path) else {
        return false;
    };
    let (frontmatter, _) = toexec_skill::frontmatter::split(&raw);
    let Some(frontmatter) = frontmatter else {
        return false;
    };
    // `alwaysApply` / `always_apply` / `alwaysapply` 都认（共享库的键匹配
    // 不分大小写和连字符），`"true"` 这种带引号的也认。
    toexec_skill::frontmatter::parse(frontmatter)
        .ok()
        .and_then(|fm| fm.flag("alwaysApply"))
        .unwrap_or(false)
}

fn effective_sources(sources: &[String]) -> (Vec<String>, bool) {
    let normalized = normalized_source_list(sources);

    let auto_enabled =
        normalized.is_empty() || normalized.iter().any(|source| source == AUTO_SOURCE);
    if auto_enabled {
        return (
            DISCOVERABLE_SOURCES
                .iter()
                .map(|source| (*source).to_string())
                .collect(),
            true,
        );
    }
    (normalized, false)
}

fn normalized_source_list(sources: &[String]) -> Vec<String> {
    let mut normalized = Vec::new();
    for source in sources {
        let source = normalize_provider(source);
        if !source.is_empty() && !normalized.iter().any(|item| item == &source) {
            normalized.push(source);
        }
    }
    normalized
}

fn add_auto_instruction_candidates(
    candidates: &mut Vec<(String, PathBuf, &'static str)>,
    workspace_root: &Path,
) {
    for (provider, relative) in [("auto", "GEMINI.md"), ("auto", ".windsurfrules")] {
        candidates.push((provider.into(), workspace_root.join(relative), "workspace"));
    }
}

fn collect_skill_candidates(
    candidates: &mut Vec<Candidate>,
    provider: &str,
    root: &Path,
    scope: &'static str,
    max_depth: usize,
) {
    if !root.is_dir() {
        return;
    }
    for entry in WalkDir::new(root)
        .min_depth(1)
        .max_depth(max_depth)
        .into_iter()
        .filter_map(Result::ok)
    {
        let path = entry.path();
        if path.is_file() && path.file_name().and_then(|value| value.to_str()) == Some("SKILL.md") {
            candidates.push(Candidate {
                provider: provider.to_string(),
                path: path.to_path_buf(),
                scope,
                root: root.to_path_buf(),
            });
        }
    }
}

fn collect_auto_workspace_skills(candidates: &mut Vec<Candidate>, workspace_root: &Path) {
    let walker = WalkDir::new(workspace_root)
        .min_depth(1)
        .max_depth(8)
        .into_iter()
        .filter_entry(auto_scan_entry_allowed);
    for entry in walker.filter_map(Result::ok) {
        let path = entry.path();
        if path.is_file() && path.file_name().and_then(|value| value.to_str()) == Some("SKILL.md") {
            candidates.push(Candidate {
                provider: infer_skill_provider(workspace_root, path),
                path: path.to_path_buf(),
                scope: "workspace",
                root: workspace_root.to_path_buf(),
            });
        }
    }
}

fn auto_scan_entry_allowed(entry: &walkdir::DirEntry) -> bool {
    if !entry.file_type().is_dir() {
        return true;
    }
    let Some(name) = entry.file_name().to_str() else {
        return true;
    };
    !matches!(
        name,
        ".git"
            | ".gld"
            | "node_modules"
            | "target"
            | "build"
            | "dist"
            | ".next"
            | ".svelte-kit"
            | "coverage"
            | "vendor"
            | ".venv"
            | "venv"
    )
}

fn infer_skill_provider(workspace_root: &Path, path: &Path) -> String {
    let relative = path
        .strip_prefix(workspace_root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/");
    if relative.starts_with(".codex/") {
        return "codex".into();
    }
    if relative.starts_with(".claude/") {
        return "claude".into();
    }
    if relative.starts_with(".cursor/") {
        return "cursor".into();
    }
    if relative.starts_with(".github/") {
        return "copilot".into();
    }
    if relative.starts_with(".opencode/") {
        return "opencode".into();
    }
    if relative.starts_with(".zcode/") {
        return "zcode".into();
    }
    if relative.starts_with(".reasonix/") {
        return "reasonix".into();
    }
    if relative.starts_with(".agents/") || relative.starts_with(".agent/") {
        return "shared".into();
    }
    "auto".into()
}

/// 用户主目录。全局说明和 Skill 都是从这里往下找的。
///
/// 单独包一层是为了让单元测试能换掉它：扫描全局说明时，跑测试那台机器上
/// 有没有 `~/.codex/AGENTS.md` 会直接改变结果——同一份代码，装过 Codex 的人
/// 那里红、CI 的干净镜像里绿，而这跟被测的规则毫无关系。
fn home_dir() -> Option<PathBuf> {
    #[cfg(test)]
    if let Some(path) = tests::home_override() {
        return Some(path);
    }
    dirs::home_dir()
}

fn normalize_provider(value: &str) -> String {
    match value.trim().to_ascii_lowercase().as_str() {
        "github" | "github-copilot" | "github_copilot" => "copilot".into(),
        "open-code" | "open_code" => "opencode".into(),
        "z-code" | "z_code" => "zcode".into(),
        value => value.to_string(),
    }
}

fn add_home_candidate(
    candidates: &mut Vec<(String, PathBuf, &'static str)>,
    provider: &str,
    path: &str,
) {
    if let Some(home) = home_dir() {
        candidates.push((provider.to_string(), home.join(path), "global"));
    }
}

fn add_skill_root(
    roots: &mut Vec<(String, PathBuf, &'static str)>,
    provider: &str,
    path: PathBuf,
    scope: &'static str,
) {
    roots.push((provider.to_string(), path, scope));
}

fn add_home_skill_root(
    roots: &mut Vec<(String, PathBuf, &'static str)>,
    provider: &str,
    path: &str,
) {
    if let Some(home) = home_dir() {
        roots.push((provider.to_string(), home.join(path), "global"));
    }
}

fn discover_global_instruction_paths(home: &Path, provider: &str) -> Vec<String> {
    let mut candidates = Vec::<PathBuf>::new();
    match provider {
        "codex" => candidates.push(home.join(".codex/AGENTS.md")),
        "claude" => candidates.push(home.join(".claude/CLAUDE.md")),
        "cursor" => {
            let rules = home.join(".cursor/rules");
            if rules.is_dir() {
                for entry in WalkDir::new(rules)
                    .max_depth(6)
                    .into_iter()
                    .filter_map(Result::ok)
                {
                    let path = entry.path();
                    if path.is_file()
                        && matches!(
                            path.extension().and_then(|value| value.to_str()),
                            Some("mdc") | Some("md")
                        )
                        && cursor_rule_is_always_apply(path)
                    {
                        candidates.push(path.to_path_buf());
                    }
                }
            }
        }
        "opencode" => candidates.push(home.join(".config/opencode/AGENTS.md")),
        "zcode" => candidates.push(home.join(".zcode/AGENTS.md")),
        "reasonix" => {
            for name in ["REASONIX.md", "AGENTS.md", "CLAUDE.md"] {
                candidates.push(home.join(".reasonix").join(name));
                candidates.push(
                    home.join(".reasonix")
                        .join(name.trim_end_matches(".md").to_string() + ".local.md"),
                );
            }
        }
        _ => {}
    }

    candidates
        .into_iter()
        .filter(|path| path.is_file())
        .map(|path| display_home_path(home, &path))
        .collect()
}

fn discover_global_skill_paths(home: &Path, provider: &str) -> Vec<String> {
    let roots: &[&str] = match provider {
        "codex" => &[".agents/skills", ".codex/skills"],
        "claude" => &[".claude/skills"],
        "cursor" => &[".cursor/skills"],
        "copilot" => &[".github/skills"],
        "opencode" => &[".opencode/skills", ".config/opencode/skills"],
        "zcode" => &[".zcode/skills"],
        "reasonix" => &[".reasonix/skills", ".agent/skills"],
        _ => &[],
    };

    let mut result = Vec::new();
    let mut seen = HashSet::new();
    for root in roots {
        let root = home.join(root);
        if !root.is_dir() {
            continue;
        }
        for entry in WalkDir::new(&root)
            .min_depth(1)
            .max_depth(4)
            .into_iter()
            .filter_map(Result::ok)
        {
            let path = entry.path();
            if !path.is_file()
                || path.file_name().and_then(|value| value.to_str()) != Some("SKILL.md")
            {
                continue;
            }
            let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
            let key = canonical.to_string_lossy().to_string();
            if seen.insert(key) {
                result.push(display_home_path(home, &canonical));
            }
        }
    }
    result
}

fn display_home_path(home: &Path, path: &Path) -> String {
    let canonical_home = home.canonicalize().unwrap_or_else(|_| home.to_path_buf());
    let canonical_path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    canonical_path
        .strip_prefix(&canonical_home)
        .map(|relative| format!("~/{}", relative.to_string_lossy().replace('\\', "/")))
        .unwrap_or_else(|_| canonical_path.to_string_lossy().into_owned())
}

fn split_config_paths(value: &str) -> Vec<String> {
    value
        .split(['\n', ','])
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_string)
        .collect()
}

fn resolve_config_path(workspace_root: &Path, value: &str) -> PathBuf {
    if value == "~" {
        return home_dir().unwrap_or_else(|| workspace_root.to_path_buf());
    }
    if let Some(rest) = value
        .strip_prefix("~/")
        .or_else(|| value.strip_prefix("~\\"))
    {
        if let Some(home) = home_dir() {
            return home.join(rest);
        }
    }
    let path = PathBuf::from(value);
    if path.is_absolute() {
        path
    } else {
        workspace_root.join(path)
    }
}

fn display_path(workspace_root: &Path, path: &Path) -> String {
    path.strip_prefix(workspace_root)
        .map(|relative| relative.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| path.to_string_lossy().into_owned())
}

fn hex_hash(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    thread_local! {
        static HOME_OVERRIDE: std::cell::RefCell<Option<PathBuf>> =
            const { std::cell::RefCell::new(None) };
    }

    /// [`super::home_dir`] 用它替换真实主目录。thread_local 而不是全局变量：
    /// 测试是并行跑的，一个测试改掉全局主目录会连累另一个。
    pub(super) fn home_override() -> Option<PathBuf> {
        HOME_OVERRIDE.with(|cell| cell.borrow().clone())
    }

    /// 在一个空的临时主目录里跑一段代码。
    ///
    /// 凡是会扫全局说明 / Skill 的测试都得套上它：开发机上 `~/.codex/AGENTS.md`、
    /// `~/.claude/CLAUDE.md` 很可能真的存在，扫到了就多出几份文档，
    /// 而断言写的是"只有工作区里那一份"。
    fn with_empty_home<T>(run: impl FnOnce() -> T) -> T {
        let home = tempfile::tempdir().expect("empty home");
        HOME_OVERRIDE.with(|cell| *cell.borrow_mut() = Some(home.path().to_path_buf()));
        let result = run();
        HOME_OVERRIDE.with(|cell| *cell.borrow_mut() = None);
        result
    }

    #[test]
    fn source_lists_are_normalized_and_deduplicated() {
        assert_eq!(
            merge_source_lists(
                &["codex".into(), "github-copilot".into()],
                &["copilot".into(), "Open-Code".into()]
            ),
            vec!["codex", "copilot", "opencode"]
        );
    }

    #[test]
    fn workspace_auto_mode_overrides_global_provider_limits() {
        assert_eq!(
            merge_source_lists(&["codex".into()], &[AUTO_SOURCE.into()]),
            vec![AUTO_SOURCE]
        );
    }

    #[test]
    fn workspace_manual_sources_override_global_auto_mode() {
        assert_eq!(
            merge_source_lists(&[AUTO_SOURCE.into()], &["claude".into()]),
            vec!["claude"]
        );
    }

    #[test]
    fn workspace_disabled_mode_turns_off_inherited_scanning() {
        assert_eq!(
            merge_source_lists(&["codex".into()], &[DISABLED_SOURCE.into()]),
            vec![DISABLED_SOURCE]
        );
    }

    /// codex / opencode / zcode 都指向工作区里的同一份 AGENTS.md，只能注入一次。
    #[test]
    fn duplicate_instruction_files_are_injected_once() {
        let root = tempfile::tempdir().expect("root");
        fs::write(root.path().join("AGENTS.md"), "shared instructions").expect("agents");
        let docs = with_empty_home(|| {
            discover_instructions(
                root.path(),
                &["codex".into(), "opencode".into(), "zcode".into()],
                "",
            )
        });
        assert_eq!(docs.len(), 1, "同一份文件被注入了多次：{docs:?}");
        assert_eq!(docs[0].content, "shared instructions");
    }

    /// 主目录里的全局说明和工作区里的各算一份，两份都要在。
    ///
    /// 上面那条测试把主目录清空了，只验了"去重"；这条反过来验"该扫的确实在扫"——
    /// 只有前一条的话，把全局扫描整个删掉它也照样绿。
    #[test]
    fn a_global_instruction_file_is_picked_up_alongside_the_workspace_one() {
        let root = tempfile::tempdir().expect("root");
        fs::write(root.path().join("AGENTS.md"), "workspace instructions").expect("agents");
        let home = tempfile::tempdir().expect("home");
        fs::create_dir_all(home.path().join(".codex")).expect("codex dir");
        fs::write(home.path().join(".codex/AGENTS.md"), "global instructions").expect("global");

        HOME_OVERRIDE.with(|cell| *cell.borrow_mut() = Some(home.path().to_path_buf()));
        let docs = discover_instructions(root.path(), &["codex".into()], "");
        HOME_OVERRIDE.with(|cell| *cell.borrow_mut() = None);

        let scopes: Vec<&str> = docs.iter().map(|doc| doc.scope.as_str()).collect();
        assert_eq!(scopes, vec!["global", "workspace"], "{docs:?}");
    }

    #[test]
    fn skills_are_progressively_disclosed_from_skill_md() {
        let root = tempfile::tempdir().expect("root");
        let skill_dir = root.path().join(".agents/skills/release");
        fs::create_dir_all(&skill_dir).expect("skill dir");
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: release\ndescription: Prepare a release safely\n---\n\n# Steps\nRun tests.",
        )
        .expect("skill");
        let skills = discover_skills(root.path(), &["custom".into()], ".agents/skills");
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].descriptor.name, "release");
        assert_eq!(skills[0].body, "# Steps\nRun tests.");
    }

    /// 超过上限的 SKILL.md 以前被悄悄截断，模型拿到半份流程还以为是全部。
    #[test]
    fn a_skill_md_that_is_cut_says_so() {
        let root = tempfile::tempdir().expect("root");
        let dir = root.path().join(".agents/skills/huge");
        fs::create_dir_all(&dir).expect("skill dir");
        let text = format!(
            "---\ndescription: Huge\n---\n{}",
            "line of steps\n".repeat(10_000)
        );
        fs::write(dir.join("SKILL.md"), &text).expect("skill");
        let scan = scan_skills(root.path(), &["custom".into()], ".agents/skills");
        let skill = &scan.skills[0];
        assert!(skill.body.chars().count() < text.chars().count());
        assert!(
            skill
                .notes
                .iter()
                .any(|note| note.contains(&format!("{} bytes", text.len()))),
            "{:?}",
            skill.notes
        );
        assert_eq!(skill.descriptor.content_sha256, hex_hash(text.as_bytes()));
        assert!(skill.dir.as_ref().is_some_and(|dir| dir.ends_with("huge")));
    }

    #[test]
    fn global_scan_detects_user_level_instruction_and_skill_sources() {
        let home = tempfile::tempdir().expect("home");
        let codex = home.path().join(".codex");
        fs::create_dir_all(&codex).expect("codex dir");
        fs::write(codex.join("AGENTS.md"), "Global Codex instructions")
            .expect("codex instructions");

        let claude_skill = home.path().join(".claude/skills/review");
        fs::create_dir_all(&claude_skill).expect("claude skill dir");
        fs::write(
            claude_skill.join("SKILL.md"),
            "---\nname: review\ndescription: Review code\n---\nReview carefully.",
        )
        .expect("claude skill");

        let scan = scan_global_agent_context_at(home.path());

        assert!(scan
            .detected_instruction_sources
            .iter()
            .any(|source| source == "codex"));
        assert!(scan
            .detected_skill_sources
            .iter()
            .any(|source| source == "claude"));
        let codex = scan
            .sources
            .iter()
            .find(|source| source.provider == "codex")
            .expect("codex detection");
        assert_eq!(codex.instruction_paths, vec!["~/.codex/AGENTS.md"]);
        let claude = scan
            .sources
            .iter()
            .find(|source| source.provider == "claude")
            .expect("claude detection");
        assert_eq!(claude.skill_paths, vec!["~/.claude/skills/review/SKILL.md"]);
    }

    #[test]
    fn cursor_only_loads_always_apply_rules_in_static_phase() {
        let root = tempfile::tempdir().expect("root");
        let rules = root.path().join(".cursor/rules");
        fs::create_dir_all(&rules).expect("rules");
        fs::write(
            rules.join("always.mdc"),
            "---\nalwaysApply: true\n---\nAlways use tests.",
        )
        .expect("always");
        fs::write(
            rules.join("scoped.mdc"),
            "---\nalwaysApply: false\nglobs: src/**\n---\nScoped rule.",
        )
        .expect("scoped");
        let docs = discover_instructions(root.path(), &["cursor".into()], "");
        assert_eq!(docs.len(), 1);
        assert!(docs[0].content.contains("Always use tests"));
    }

    #[test]
    fn empty_sources_enable_auto_instruction_discovery() {
        let root = tempfile::tempdir().expect("root");
        fs::write(root.path().join("AGENTS.md"), "Use repository rules.").expect("agents");
        fs::write(
            root.path().join("GEMINI.md"),
            "Use Gemini repository rules.",
        )
        .expect("gemini");

        let docs = discover_instructions(root.path(), &[], "");

        assert!(docs.iter().any(|doc| doc.path.ends_with("AGENTS.md")));
        assert!(docs.iter().any(|doc| doc.path.ends_with("GEMINI.md")));
    }

    #[test]
    fn auto_skill_discovery_finds_unknown_skill_roots_and_skips_dependencies() {
        let root = tempfile::tempdir().expect("root");
        let custom_skill = root.path().join(".custom-agent/workflows/release");
        fs::create_dir_all(&custom_skill).expect("custom skill dir");
        fs::write(
            custom_skill.join("SKILL.md"),
            "---\nname: auto-release\ndescription: Automatically discovered release workflow\n---\nRun release checks.",
        )
        .expect("custom skill");

        let dependency_skill = root.path().join("node_modules/dependency/skills/ignored");
        fs::create_dir_all(&dependency_skill).expect("dependency skill dir");
        fs::write(
            dependency_skill.join("SKILL.md"),
            "---\nname: ignored-dependency\ndescription: Must not be scanned\n---\nIgnore me.",
        )
        .expect("dependency skill");

        let skills = discover_skills(root.path(), &[AUTO_SOURCE.into()], "");

        assert!(skills
            .iter()
            .any(|skill| skill.descriptor.name == "auto-release"));
        assert!(!skills
            .iter()
            .any(|skill| skill.descriptor.name == "ignored-dependency"));
    }

    /// `description: >` 是官方 skill 里最常见的写法之一。gld 原来逐行找
    /// `key:` 前缀，读出来的描述就是一个 `>`——目录里那条 skill 等于没有描述，
    /// 模型永远不会选它（v3 方案 3.3 节点名的缺陷）。
    #[test]
    fn a_folded_description_is_read_as_text_not_as_a_greater_than_sign() {
        let root = tempfile::tempdir().expect("workspace");
        let dir = root.path().join(".claude/skills/release");
        fs::create_dir_all(&dir).expect("skill dir");
        fs::write(
            dir.join("SKILL.md"),
            "---\nname: release\ndescription: >\n  Use this when cutting a release.\n  Runs the checks first.\n---\nBody.\n",
        )
        .expect("skill");

        let skills = with_empty_home(|| discover_skills(root.path(), &[AUTO_SOURCE.into()], ""));
        let release = skills
            .iter()
            .find(|skill| skill.descriptor.name == "release")
            .expect("release skill");
        assert!(
            release
                .descriptor
                .description
                .contains("Use this when cutting a release."),
            "{}",
            release.descriptor.description
        );
        assert!(!release.descriptor.description.starts_with('>'));
    }

    /// compact 档的目录有字符预算：放不下的不是丢掉不提，而是明说还有几个、
    /// 去哪儿看。模型据此知道"这里还有东西"，调一次 list_skills 就拿得到。
    #[test]
    fn the_compact_catalog_says_how_many_it_left_out() {
        let skills = (0..40)
            .map(|index| SkillEntry {
                descriptor: SkillDescriptor {
                    id: format!("id{index:02}"),
                    name: format!("skill-{index:02}"),
                    description: "A".repeat(200),
                    provider: "claude".into(),
                    path: format!(".claude/skills/skill-{index:02}/SKILL.md"),
                    scope: "workspace".into(),
                    ..Default::default()
                },
                ..Default::default()
            })
            .collect::<Vec<_>>();

        let compact = render_skill_catalog_for_profile(&skills, "compact");
        assert!(
            compact.listed > 0 && compact.listed < skills.len(),
            "{}",
            compact.listed
        );
        assert!(
            compact.text.chars().count() <= COMPACT_SKILL_CATALOG_CHARS + 200,
            "{}",
            compact.text
        );
        assert!(
            compact.text.contains(&format!(
                "{} more not listed here",
                skills.len() - compact.listed
            )),
            "{}",
            compact.text
        );
        assert!(compact.text.contains("call list_skills"));

        // 别的档沿用原来的 50 条上限，不受这个预算影响。
        let advanced = render_skill_catalog_for_profile(&skills, "advanced");
        assert_eq!(advanced.listed, 40);
    }

    /// 项目自己的 skill 排在主目录那批前面：compact 的预算有限，挤掉的应该是
    /// 通用的那些，不是这个项目专有的。
    #[test]
    fn workspace_skills_come_before_the_ones_installed_on_this_machine() {
        let root = tempfile::tempdir().expect("workspace");
        let dir = root.path().join(".claude/skills/project-only");
        fs::create_dir_all(&dir).expect("skill dir");
        fs::write(
            dir.join("SKILL.md"),
            "---\nname: project-only\ndescription: Project specific\n---\nBody.\n",
        )
        .expect("skill");

        let home = tempfile::tempdir().expect("home");
        let home_dir = home.path().join(".claude/skills/machine-wide");
        fs::create_dir_all(&home_dir).expect("home skill dir");
        fs::write(
            home_dir.join("SKILL.md"),
            "---\nname: machine-wide\ndescription: Installed for every project\n---\nBody.\n",
        )
        .expect("home skill");

        HOME_OVERRIDE.with(|cell| *cell.borrow_mut() = Some(home.path().to_path_buf()));
        let skills = discover_skills(root.path(), &[AUTO_SOURCE.into()], "");
        HOME_OVERRIDE.with(|cell| *cell.borrow_mut() = None);

        let names = skills
            .iter()
            .map(|skill| skill.descriptor.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["project-only", "machine-wide"], "{names:?}");
    }
}
