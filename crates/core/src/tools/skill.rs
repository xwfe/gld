use std::path::Path;

use serde_json::{json, Value};
use toexec_skill::dir;

use crate::agent_context::SkillEntry;
use crate::tools::context::ToolContext;
use crate::tools::workspace::{tool_ok, WorkspaceError};

/// 附件一次最多读多少字节。skill 的脚本和参考文件是给模型读的文本，比这大的
/// 多半不是给模型读的。`files` 最多列多少个、往下看几层，在 `toexec_skill::dir`。
const MAX_ATTACHMENT_BYTES: u64 = 256 * 1024;

const USER_ONLY_NOTE: &str = "this skill is marked disable-model-invocation: its author wants a person to start it. Follow it only if the user asked for this skill by name; if you picked it yourself, stop and ask the user first";

pub fn list_skills(ctx: &ToolContext, _args: &Value) -> Result<Value, WorkspaceError> {
    let scan = ctx.current_skill_scan();
    Ok(tool_ok(json!({
        "skills": scan.skills.iter().map(|skill| &skill.descriptor).collect::<Vec<_>>(),
        "count": scan.skills.len(),
        // 扫到了却没收进来的，各带原因。不列的话，作者只会看到"AI 不用我的
        // skill"，而不知道是 frontmatter 写坏了。
        "skipped": scan.skipped,
    })))
}

pub fn get_skill(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    let text = |key: &str| {
        args.get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
    };
    let (id, name, file) = (text("id"), text("name"), text("file"));
    if id.is_none() && name.is_none() {
        return Err(WorkspaceError::invalid_argument("id or name is required"));
    }
    let skills = ctx.current_skills();
    let matches = skills
        .iter()
        .filter(|skill| {
            id.is_some_and(|value| skill.descriptor.id == value)
                || name.is_some_and(|value| skill.descriptor.name.eq_ignore_ascii_case(value))
        })
        .collect::<Vec<_>>();
    let skill = match matches.as_slice() {
        [] => {
            return Err(WorkspaceError::Tool {
                code: "SKILL_NOT_FOUND",
                message: "Skill not found in discovered agent sources".into(),
                category: "runtime",
                retryable: false,
            })
        }
        [skill] => *skill,
        _ => {
            return Err(WorkspaceError::ToolDetails {
                code: "AMBIGUOUS_SKILL",
                message: "Multiple skills use this name; call get_skill with id instead".into(),
                category: "runtime",
                retryable: false,
                details: json!({
                    "matches": matches.iter().map(|skill| &skill.descriptor).collect::<Vec<_>>()
                }),
            })
        }
    };
    let readable = files_readable(ctx, skill);
    if let Some(file) = file {
        let dir = readable.map_err(WorkspaceError::invalid_argument)?;
        let content = read_attachment(ctx, dir, file)?;
        return Ok(tool_ok(json!({
            "skill": skill.descriptor,
            "file": file,
            "content": content,
        })));
    }
    let mut result = json!({
        "skill": skill.descriptor,
        "content": skill.body,
    });
    let mut notes = Vec::new();
    if skill.descriptor.disable_model_invocation {
        // 不拒：gld 没有斜杠命令，用户在对话里点名是唯一的启动方式，拒了就
        // 谁都用不了。能做的是把作者的意思原样交给模型。
        notes.push(USER_ONLY_NOTE.to_string());
    }
    notes.extend(skill.notes.iter().cloned());
    if !notes.is_empty() {
        result["notes"] = json!(notes);
    }
    match readable {
        Ok(dir) => {
            let listing = dir::list(dir);
            result["files"] = json!(listing.files);
            if listing.more {
                result["moreFiles"] = json!(true);
            }
        }
        // 读不了的原因跟着正文走：正文里写着"跑 scripts/x.sh"，模型该知道为什么
        // 拿不到，而不是反复试。
        Err(why) if skill.dir.is_some() => result["filesUnavailable"] = json!(why),
        Err(_) => {}
    }
    Ok(tool_ok(result))
}

/// 这个 skill 目录里的文件能不能读，能读就是那个目录。
///
/// 工作区里的直接能读（`read_file` 本来就读得到）。工作区外的（主目录里的
/// 用户级 skill、自定义路径）要么来源是明确配置的，要么已经关掉了
/// confine-reads——默认的 auto 扫描不算"明确允许"（跨仓评审 X08），不然一个挂在
/// 公网上的服务会因为这次改动默认多读出一批主目录里的文件。
fn files_readable<'a>(ctx: &ToolContext, skill: &'a SkillEntry) -> Result<&'a Path, String> {
    let Some(dir) = &skill.dir else {
        return Err("this SKILL.md sits directly in a skills root, so it has no directory of its own to read files from".into());
    };
    let in_workspace = ctx
        .workspace
        .root()
        .canonicalize()
        .is_ok_and(|root| dir.starts_with(root));
    if in_workspace || skill.explicit_source || !ctx.policy.confine_reads {
        return Ok(dir);
    }
    Err(format!(
        "this skill is outside the workspace and was found by automatic discovery; its files are read only once its source is configured explicitly (gld cfg runtime --skill-sources {}) or reads are unconfined (gld set <project> confine-reads=false)",
        skill.descriptor.provider
    ))
}

/// skill 目录里的一个文件。只许在这个 skill 自己的目录下面：`..`、绝对路径、
/// 指到目录外面的符号链接、点开头的都拒绝（规则在 `toexec_skill::dir`，ccnm
/// 的 `load_skill` 用的是同一份）。
///
/// 为什么要有：用户级 skill 在主目录里，它的正文常写"跑 scripts/fill.py"、
/// "参考 reference.md"，而 read_file 默认只读工作区，模型就只能看着正文干瞪眼。
/// 反过来，为了这个把 read_file 放开到整个主目录又太宽（跨仓评审 X08）。
fn read_attachment(ctx: &ToolContext, dir: &Path, file: &str) -> Result<String, WorkspaceError> {
    let resolved = dir::resolve(dir, file).map_err(|error| refused(file, error))?;
    ctx.workspace.reject_data_home_read(&resolved)?;
    dir::read_text(&resolved, MAX_ATTACHMENT_BYTES).map_err(|error| refused(file, error))
}

/// 共享库只说是哪一种，错误码和措辞是 gld 自己的对外契约。
fn refused(file: &str, error: dir::ReadError) -> WorkspaceError {
    match error {
        dir::ReadError::NotRelative => outside(format!(
            "file must be a relative path inside the skill's directory, like scripts/run.sh; got {file}"
        )),
        dir::ReadError::Outside => outside(format!(
            "{file} leads outside the skill's directory (through a symlink); it is not read"
        )),
        // 点开头的不读：skill 目录里的 `.env` 多半是给脚本用的密钥，不是给模型
        // 看的。`files` 也不列它们。
        dir::ReadError::Hidden => outside(format!(
            "{file} is a hidden file; get_skill does not read those (they often hold a script's secrets)"
        )),
        dir::ReadError::NotFound => WorkspaceError::not_found(format!(
            "{file} does not exist in this skill's directory; get_skill without file lists what is there"
        )),
        dir::ReadError::NotAFile => WorkspaceError::invalid_argument(format!(
            "{file} is not a file; get_skill without file lists what is there"
        )),
        dir::ReadError::TooLarge { size, max } => WorkspaceError::invalid_argument(format!(
            "{file} is {size} bytes; get_skill returns files up to {max} bytes"
        )),
        dir::ReadError::NotText => WorkspaceError::invalid_argument(format!(
            "{file} is not text (not UTF-8); get_skill only returns text"
        )),
        dir::ReadError::Io(error) => io_error(error),
    }
}

fn outside(message: String) -> WorkspaceError {
    WorkspaceError::Tool {
        code: "SKILL_FILE_OUTSIDE",
        message,
        category: "security",
        retryable: false,
    }
}

fn io_error(error: std::io::Error) -> WorkspaceError {
    WorkspaceError::Tool {
        code: "IO_ERROR",
        message: format!("Failed to read the skill file: {error}"),
        category: "runtime",
        retryable: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_context::{AgentContextRuntimeConfig, SkillDescriptor, SkillEntry};
    use crate::tools::ToolContext;

    #[test]
    fn get_skill_loads_body_on_demand() {
        let workspace = tempfile::tempdir().expect("workspace");
        let harness = tempfile::tempdir().expect("harness");
        let ctx =
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("context")
                .with_skills(vec![SkillEntry {
                    descriptor: SkillDescriptor {
                        id: "abc".into(),
                        name: "release".into(),
                        description: "Release safely".into(),
                        provider: "codex".into(),
                        path: ".agents/skills/release/SKILL.md".into(),
                        scope: "workspace".into(),
                        ..Default::default()
                    },
                    body: "Run tests first.".into(),
                    ..Default::default()
                }]);
        let result = get_skill(&ctx, &json!({"name":"release"})).expect("skill");
        assert_eq!(result["content"], "Run tests first.");
    }

    #[test]
    fn list_skills_refreshes_files_created_after_context_initialization() {
        let workspace = tempfile::tempdir().expect("workspace");
        let harness = tempfile::tempdir().expect("harness");
        let ctx =
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("context")
                .with_agent_context(AgentContextRuntimeConfig {
                    instruction_sources: vec!["codex".into()],
                    skill_sources: vec!["custom".into()],
                    custom_instruction_paths: String::new(),
                    custom_skill_paths: ".agents/skills".into(),
                    hidden_skills: Vec::new(),
                });

        let empty = list_skills(&ctx, &json!({})).expect("empty skills");
        assert_eq!(empty["count"], 0);

        let skill_dir = workspace.path().join(".agents/skills/live-refresh");
        std::fs::create_dir_all(&skill_dir).expect("skill dir");
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: live-refresh\ndescription: Refresh after startup\n---\nFresh body.",
        )
        .expect("skill");

        let refreshed = list_skills(&ctx, &json!({})).expect("refreshed skills");
        assert_eq!(refreshed["count"], 1);
        assert_eq!(refreshed["skills"][0]["name"], "live-refresh");
    }

    /// 一个工作区之外的 skill 根（像主目录里的 `~/.claude/skills`）：工作区、
    /// harness、skill 根三个临时目录，外加一个 skill 根外面的"秘密"目录。
    struct Outside {
        _dirs: [tempfile::TempDir; 3],
        root: std::path::PathBuf,
        secret: tempfile::TempDir,
        ctx: ToolContext,
    }

    fn outside_skill_root() -> Outside {
        let workspace = tempfile::tempdir().expect("workspace");
        let harness = tempfile::tempdir().expect("harness");
        let skills = tempfile::tempdir().expect("skills root");
        let secret = tempfile::tempdir().expect("secret");
        std::fs::write(secret.path().join("token.txt"), "do not read").expect("secret");
        let root = skills.path().canonicalize().expect("canonical root");
        let ctx =
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("context")
                .with_agent_context(AgentContextRuntimeConfig {
                    instruction_sources: vec![],
                    skill_sources: vec!["custom".into()],
                    custom_instruction_paths: String::new(),
                    custom_skill_paths: root.to_string_lossy().into_owned(),
                    hidden_skills: Vec::new(),
                });
        Outside {
            _dirs: [workspace, harness, skills],
            root,
            secret,
            ctx,
        }
    }

    fn write(path: &std::path::Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).expect("dir");
        std::fs::write(path, text).expect("write");
    }

    /// 用户级 skill 的附件：read_file 默认只读工作区，读不到；get_skill 只在
    /// 这个 skill 自己的目录里读。
    #[test]
    fn a_skill_outside_the_workspace_can_have_its_own_files_read() {
        let o = outside_skill_root();
        let dir = o.root.join("fill-pdf");
        write(
            &dir.join("SKILL.md"),
            "---\ndescription: Fill PDF forms\n---\nRun scripts/fill.py, see reference.md.\n",
        );
        write(&dir.join("scripts/fill.py"), "print('fill')\n");
        write(&dir.join("reference.md"), "# Fields\n");
        write(&dir.join(".cache/x"), "hidden");

        let loaded = get_skill(&o.ctx, &json!({"name": "fill-pdf"})).expect("skill");
        assert_eq!(loaded["files"], json!(["reference.md", "scripts/fill.py"]));
        assert_eq!(loaded["skill"]["scope"], "custom");
        assert_eq!(
            loaded["skill"]["contentSha256"].as_str().map(str::len),
            Some(64)
        );

        let file = get_skill(
            &o.ctx,
            &json!({"name": "fill-pdf", "file": "scripts/fill.py"}),
        )
        .expect("attachment");
        assert_eq!(file["content"], "print('fill')\n");
        assert_eq!(file["file"], "scripts/fill.py");
    }

    #[test]
    fn nothing_outside_the_skills_own_directory_is_read() {
        let o = outside_skill_root();
        let dir = o.root.join("tool");
        write(
            &dir.join("SKILL.md"),
            "---\ndescription: A tool\n---\nbody\n",
        );
        write(
            &o.root.join("other/SKILL.md"),
            "---\ndescription: Other\n---\nbody\n",
        );
        write(&o.root.join("other/notes.md"), "other skill's file");
        #[cfg(unix)]
        std::os::unix::fs::symlink(o.secret.path().join("token.txt"), dir.join("link.txt"))
            .expect("symlink");
        std::fs::create_dir_all(dir.join("sub")).expect("sub");
        std::fs::write(dir.join("big.txt"), vec![b'a'; 300 * 1024]).expect("big");
        std::fs::write(dir.join("blob.bin"), [0xff, 0xfe, 0x00]).expect("blob");

        let secret = o.secret.path().join("token.txt");
        let secret = secret.to_string_lossy();
        for (file, code) in [
            ("../other/notes.md", "SKILL_FILE_OUTSIDE"),
            (secret.as_ref(), "SKILL_FILE_OUTSIDE"),
            #[cfg(unix)]
            ("link.txt", "SKILL_FILE_OUTSIDE"),
            ("missing.md", "NOT_FOUND"),
            ("sub", "INVALID_ARGUMENT"),
            ("big.txt", "INVALID_ARGUMENT"),
            ("blob.bin", "INVALID_ARGUMENT"),
        ] {
            let err = get_skill(&o.ctx, &json!({"name": "tool", "file": file})).expect_err(file);
            assert_eq!(err.code(), code, "{file}: {}", err.message());
        }
    }

    /// 默认的 auto 扫描扫到的、工作区外的 skill：文件默认不读，说清怎么打开。
    /// 这条挡的是"挂在公网上的服务因为这次改动默认多读出一批主目录文件"。
    #[test]
    fn files_of_an_automatically_found_skill_outside_need_an_opt_in() {
        let workspace = tempfile::tempdir().expect("workspace");
        let harness = tempfile::tempdir().expect("harness");
        let home_skill = tempfile::tempdir().expect("home skill");
        let dir = home_skill.path().canonicalize().expect("dir");
        write(&dir.join("notes.md"), "reference");
        let entry = SkillEntry {
            descriptor: SkillDescriptor {
                id: "abc".into(),
                name: "machine-wide".into(),
                provider: "claude".into(),
                scope: "global".into(),
                ..Default::default()
            },
            body: "see notes.md".into(),
            dir: Some(dir.clone()),
            explicit_source: false,
            ..Default::default()
        };
        let mut ctx =
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("context")
                .with_skills(vec![entry.clone()]);

        let loaded = get_skill(&ctx, &json!({"name": "machine-wide"})).expect("body still served");
        assert!(loaded.get("files").is_none());
        let why = loaded["filesUnavailable"].as_str().expect("says why");
        assert!(
            why.contains("--skill-sources claude") && why.contains("confine-reads=false"),
            "{why}"
        );
        let err = get_skill(&ctx, &json!({"name": "machine-wide", "file": "notes.md"}))
            .expect_err("not read by default");
        assert_eq!(err.code(), "INVALID_ARGUMENT");

        // 关掉 confine-reads 的人本来就能读整台机器，这里不再多挡。
        ctx.policy.confine_reads = false;
        let file = get_skill(&ctx, &json!({"name": "machine-wide", "file": "notes.md"}))
            .expect("unconfined");
        assert_eq!(file["content"], "reference");

        // 明确配置的来源也行。
        ctx.policy.confine_reads = true;
        let ctx = ctx.with_skills(vec![SkillEntry {
            explicit_source: true,
            ..entry
        }]);
        let loaded = get_skill(&ctx, &json!({"name": "machine-wide"})).expect("skill");
        assert_eq!(loaded["files"], json!(["notes.md"]));
    }

    /// 点开头的文件不读也不列：skill 目录里的 `.env` 多半是脚本的密钥。
    #[test]
    fn hidden_files_are_neither_listed_nor_read() {
        let o = outside_skill_root();
        let dir = o.root.join("deploy");
        write(
            &dir.join("SKILL.md"),
            "---\ndescription: Deploy\n---\nbody\n",
        );
        write(&dir.join(".env"), "TOKEN=secret");
        write(&dir.join(".config/key"), "secret");
        write(&dir.join("run.sh"), "./go");
        let loaded = get_skill(&o.ctx, &json!({"name": "deploy"})).expect("skill");
        assert_eq!(loaded["files"], json!(["run.sh"]));
        for file in [".env", ".config/key", "./.env"] {
            let err = get_skill(&o.ctx, &json!({"name": "deploy", "file": file})).expect_err(file);
            assert_eq!(err.code(), "SKILL_FILE_OUTSIDE", "{file}");
        }
    }

    /// SKILL.md 直接放在 skill 根上：它的"目录"是整个根，不给读。
    #[test]
    fn a_skill_md_at_the_root_itself_has_no_directory_to_read() {
        let o = outside_skill_root();
        write(
            &o.root.join("SKILL.md"),
            "---\nname: flat\ndescription: Flat\n---\nbody\n",
        );
        write(&o.root.join("other/notes.md"), "not this skill's");
        let loaded = get_skill(&o.ctx, &json!({"name": "flat"})).expect("skill");
        assert!(loaded.get("files").is_none(), "{loaded}");
        let err = get_skill(&o.ctx, &json!({"name": "flat", "file": "other/notes.md"}))
            .expect_err("no directory");
        assert_eq!(err.code(), "INVALID_ARGUMENT");
    }

    /// 读不了的不是静默消失，而是列在 skipped 里、说出原因。
    #[test]
    fn what_cannot_be_taken_in_is_listed_with_the_reason() {
        let o = outside_skill_root();
        write(
            &o.root.join("ok/SKILL.md"),
            "---\ndescription: Fine\n---\nbody\n",
        );
        write(
            &o.root.join("open-quote/SKILL.md"),
            "---\ndescription: \"never closed\n---\nbody\n",
        );
        write(
            &o.root.join("no-description/SKILL.md"),
            "---\nname: x\n---\nbody\n",
        );
        write(
            &o.root.join("too-long/SKILL.md"),
            &format!("---\ndescription: {}\n---\nbody\n", "a".repeat(1025)),
        );
        write(
            &o.root.join("copy/SKILL.md"),
            "---\ndescription: Fine\n---\nbody\n",
        );

        let listed = list_skills(&o.ctx, &json!({})).expect("list");
        assert_eq!(listed["count"], 1, "{listed}");
        let reasons: Vec<(String, String)> = listed["skipped"]
            .as_array()
            .expect("skipped")
            .iter()
            .map(|s| {
                (
                    s["path"].as_str().unwrap().to_string(),
                    s["reason"].as_str().unwrap().to_string(),
                )
            })
            .collect();
        let reason = |dir: &str| {
            reasons
                .iter()
                .find(|(path, _)| path.contains(&format!("/{dir}/")))
                .map(|(_, reason)| reason.as_str())
                .unwrap_or_else(|| panic!("{dir} not in {reasons:?}"))
        };
        assert!(reason("open-quote").contains("frontmatter line 1"));
        assert!(reason("no-description").contains("no description"));
        assert!(reason("too-long").contains("1025 characters"));
        // 两份内容一模一样：收一份，另一份说明是谁的副本。
        let copy = reasons
            .iter()
            .find(|(_, reason)| reason.starts_with("same content as"))
            .expect("the copy is named");
        assert!(
            copy.0.contains("/copy/") || copy.0.contains("/ok/"),
            "{copy:?}"
        );
    }

    /// 原生客户端会整段丢弃的 frontmatter、写了几遍的键：get_skill 写明。
    #[test]
    fn how_the_frontmatter_reads_natively_comes_with_the_skill() {
        let o = outside_skill_root();
        write(
            &o.root.join("loose/SKILL.md"),
            "---\ndescription: Review.\n  Use when: asked.\nname: loose\nname: loose\n---\nbody\n",
        );
        let loaded = get_skill(&o.ctx, &json!({"name": "loose"})).expect("skill");
        let notes = loaded["notes"].to_string();
        assert!(notes.contains("not valid YAML"), "{notes}");
        assert!(
            notes.contains("\\\"name\\\" more than once (lines 3, 4)"),
            "{notes}"
        );
    }

    /// 标了 disable-model-invocation 的 skill：list_skills 照列并带上标记，
    /// get_skill 照给正文，但第一条说明就是"用户点名才照做"。拒掉的话，没有
    /// 斜杠命令的客户端（ChatGPT、Codex）里谁都用不了它。
    #[test]
    fn a_skill_for_the_user_to_start_is_listed_and_loaded_with_a_warning() {
        let o = outside_skill_root();
        write(
            &o.root.join("deploy/SKILL.md"),
            "---\nname: deploy\ndescription: Ship it\ndisable-model-invocation: true\n---\nPush to prod.\n",
        );
        write(
            &o.root.join("review/SKILL.md"),
            "---\nname: review\ndescription: Review it\n---\nRead the diff.\n",
        );
        let listed = list_skills(&o.ctx, &json!({})).expect("list");
        let flag = |name: &str| {
            listed["skills"]
                .as_array()
                .expect("skills")
                .iter()
                .find(|skill| skill["name"] == name)
                .expect(name)
                .get("disableModelInvocation")
                .cloned()
        };
        assert_eq!(flag("deploy"), Some(json!(true)));
        // 默认值不写出来：每条 skill 多一个 false 只是噪音。
        assert_eq!(flag("review"), None);

        let loaded = get_skill(&o.ctx, &json!({"name": "deploy"})).expect("skill");
        assert_eq!(loaded["content"], "Push to prod.");
        let first = loaded["notes"][0].as_str().expect("note");
        assert!(first.contains("disable-model-invocation"), "{first}");
        assert!(first.contains("asked for this skill by name"), "{first}");

        let plain = get_skill(&o.ctx, &json!({"name": "review"})).expect("skill");
        assert!(plain.get("notes").is_none(), "{plain}");
    }

    /// 按名字藏掉的本机 skill：列表里没有、点名也拿不到、skipped 里也不提——
    /// 和没装一样。
    #[test]
    fn a_hidden_installed_skill_is_as_if_it_were_not_there() {
        let mut o = outside_skill_root();
        write(
            &o.root.join("pdf/SKILL.md"),
            "---\ndescription: Fill PDFs\n---\nbody\n",
        );
        write(
            &o.root.join("noise/SKILL.md"),
            "---\ndescription: Noise\n---\nbody\n",
        );
        let config = AgentContextRuntimeConfig {
            instruction_sources: vec![],
            skill_sources: vec!["custom".into()],
            custom_instruction_paths: String::new(),
            custom_skill_paths: o.root.to_string_lossy().into_owned(),
            hidden_skills: vec!["NOISE".into()],
        };
        o.ctx = o.ctx.with_agent_context(config);
        let listed = list_skills(&o.ctx, &json!({})).expect("list");
        assert_eq!(listed["count"], 1, "{listed}");
        assert_eq!(listed["skills"][0]["name"], "pdf");
        assert!(!listed.to_string().contains("noise"), "{listed}");
        let err = get_skill(&o.ctx, &json!({"name": "noise"})).expect_err("hidden");
        assert_eq!(err.code(), "SKILL_NOT_FOUND");
    }
}
