//! `~/.agents/mcp.json`：用户要 gld 再关掉哪些工具、skill 给到哪一档
//! （toexec RFC-0001，文件的读法在 `toexec-agents`，ccnm 读同一个文件）。
//!
//! 读的是跑守护进程那个账号的 HOME。**只收窄**：tool-profile 先定上限，
//! 这里在上限里再关掉一些；skill 自己的 `disable-model-invocation` 这里也放
//! 不开。
//!
//! 每次用到都重读，不缓存：文件只有几百字节，改完下一次请求就生效，也没有
//! "同一秒里改了两次、修改时间没变"这种缓存漏读的情况。
//!
//! 文件写坏了不能当成没有：那等于把用户想关的工具又打开了。所以
//! [`exposed`] 在这种时候一个工具都不给，能说话的地方（hub 的 `tools/list`、
//! `tools/call`，`gld doctor`）用 [`current`] 拿到原因报出来。

use std::path::PathBuf;

use toexec_agents::{Policy, Rules, SkillLevel, ToolRule};

use crate::agent_context::{SkillScan, SkippedSkill};

/// gld 在文件里的条目名：`mcpServers.gld`。
pub const KEY: &str = "gld";

/// 这个账号的 `~/.agents/mcp.json`。
pub fn path() -> Option<PathBuf> {
    home_dir().map(|home| toexec_agents::path_in(&home))
}

/// 单元测试里不读跑测试那台机器的真实 HOME：开发者自己写了这个文件，
/// 和被测的规则毫无关系的测试就会红。要测它的地方用 [`tests::with_home`]。
fn home_dir() -> Option<PathBuf> {
    #[cfg(test)]
    return tests::home();
    #[cfg(not(test))]
    dirs::home_dir()
}

/// 现在的规则。文件不存在是空规则；读不了、写坏了是 `Err`，里面是给人看的原因。
pub fn current() -> Result<Policy, String> {
    let Some(path) = path() else {
        return Ok(Policy::default());
    };
    Policy::load(&path).map_err(|error| {
        format!("{error}; fix it or move it away (without it nothing is turned off)")
    })
}

fn rules(policy: &Policy) -> Rules<'_> {
    policy.for_server(KEY)
}

/// 从 `names` 里去掉文件关掉的。文件写坏时一个都不留。
pub fn exposed(names: Vec<&'static str>) -> Vec<&'static str> {
    match current() {
        Ok(policy) => {
            let rules = rules(&policy);
            names
                .into_iter()
                .filter(|name| rules.tool_allowed(name))
                .collect()
        }
        Err(_) => Vec::new(),
    }
}

/// 这个工具能不能调；不能的话给模型看的原因（英文，和别的工具错误一致）。
pub fn gate(tool: &str) -> Result<(), String> {
    let policy = current()?;
    match rules(&policy).tool_rule(tool) {
        None => Ok(()),
        Some(rule) => Err(refusal(tool, rule)),
    }
}

fn refusal(tool: &str, rule: ToolRule) -> String {
    let (how, key) = match rule {
        ToolRule::NotEnabled => ("is not listed in", "enabledTools"),
        ToolRule::Disabled => ("is listed in", "disabledTools"),
    };
    format!("{tool} is turned off here: it {how} mcpServers.{KEY}.{key} in ~/.agents/mcp.json")
}

/// gld 可能对外给的全部工具名：写在文件里却不在这里面的，多半是写错了。
pub fn known_tools() -> Vec<String> {
    let mut names: Vec<String> = crate::tools::registry::P0_TOOLS
        .iter()
        .map(|(name, ..)| (*name).to_string())
        .collect();
    names.push(crate::hub::LIST_WORKSPACES.into());
    names.push(crate::hub::WORKSPACE_CONTEXT.into());
    names.extend(
        crate::bridge::tools::definitions(true)
            .iter()
            .filter_map(|tool| tool["name"].as_str().map(str::to_string)),
    );
    names.sort();
    names.dedup();
    names
}

/// 文件里提到、gld 却没有的工具名。写错在 `disabledTools` 里意味着想关的
/// 那个**还开着**，所以 doctor 当失败报。
pub fn unknown_tools(policy: &Policy) -> Vec<String> {
    let known = known_tools();
    let known: Vec<&str> = known.iter().map(String::as_str).collect();
    rules(policy).unknown_tools(&known)
}

/// 文件关掉的工具名。
pub fn turned_off(policy: &Policy) -> Vec<String> {
    let rules = rules(policy);
    known_tools()
        .into_iter()
        .filter(|name| !rules.tool_allowed(name))
        .collect()
}

/// 这个账号的文件对 gld 写了几条 skill 覆盖（doctor 用）。
pub fn skill_overrides(policy: &Policy) -> usize {
    rules(policy).skill_overrides().len()
}

/// 按文件给每个 skill 收档：`off` 拿掉；`user-invocable-only` 等于
/// frontmatter 写了 `disable-model-invocation`（不进目录，点名照样能
/// `get_skill`）；`name-only` 清掉描述，目录和 `list_skills` 都只剩名字。
///
/// 文件写坏时一个 skill 都不给，原因记在 `skipped` 里，`list_skills` 会带出来。
pub fn narrow_skills(scan: &mut SkillScan) {
    let policy = match current() {
        Ok(policy) => policy,
        Err(reason) => {
            scan.skills.clear();
            scan.skipped.push(SkippedSkill {
                provider: "agents".into(),
                path: "~/.agents/mcp.json".into(),
                scope: "global".into(),
                reason,
            });
            return;
        }
    };
    let rules = rules(&policy);
    scan.skills.retain_mut(|skill| {
        let level = rules.skill_level(&skill.descriptor.name);
        if !level.model_invocable() {
            skill.descriptor.disable_model_invocation = true;
        }
        if !level.described() {
            skill.descriptor.description.clear();
        }
        level != SkillLevel::Off
    });
}

#[cfg(test)]
pub(crate) mod tests {
    use std::cell::RefCell;
    use std::path::{Path, PathBuf};

    use super::*;

    thread_local! {
        static HOME: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
    }

    pub(super) fn home() -> Option<PathBuf> {
        HOME.with(|home| home.borrow().clone())
    }

    /// 在这个线程里把 HOME 换成 `home` 跑 `f`。
    pub(crate) fn with_home<T>(home: &Path, f: impl FnOnce() -> T) -> T {
        HOME.with(|slot| *slot.borrow_mut() = Some(home.to_path_buf()));
        let out = f();
        HOME.with(|slot| *slot.borrow_mut() = None);
        out
    }

    pub(crate) fn write_agents_file(home: &Path, text: &str) {
        let path = toexec_agents::path_in(home);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn only_the_gld_entry_narrows_and_a_broken_file_gives_nothing() {
        let home = tempfile::tempdir().unwrap();
        write_agents_file(
            home.path(),
            r#"{"mcpServers": {"gld": {"disabledTools": ["exec_command", "exec_comand"]},
                                "ccnm": {"disabledTools": ["read_file"]}}}"#,
        );
        with_home(home.path(), || {
            assert_eq!(
                exposed(vec!["read_file", "exec_command"]),
                vec!["read_file"]
            );
            let err = gate("exec_command").unwrap_err();
            assert!(err.contains("mcpServers.gld.disabledTools"), "{err}");
            assert!(gate("read_file").is_ok());
            let policy = current().unwrap();
            assert_eq!(unknown_tools(&policy), vec!["exec_comand"]);
            assert_eq!(turned_off(&policy), vec!["exec_command"]);
        });

        write_agents_file(
            home.path(),
            r#"{"mcpServers": {"gld": {"disabledTools": 1}}}"#,
        );
        with_home(home.path(), || {
            assert!(
                exposed(vec!["read_file"]).is_empty(),
                "broken means nothing, not everything"
            );
            let err = gate("read_file").unwrap_err();
            assert!(err.contains("mcpServers.gld.disabledTools"), "{err}");
        });
    }

    /// 四档在 gld 里的样子：off 连 list_skills 都没有；user-invocable-only
    /// 和 frontmatter 的 disable-model-invocation 一样只报个数、点名照给；
    /// name-only 目录里只剩名字；写 on 放不开作者关掉的。
    #[test]
    fn skill_levels_narrow_the_scan_everything_reads_from() {
        let root = tempfile::tempdir().unwrap();
        for (name, extra) in [
            ("deploy", ""),
            ("notes", ""),
            ("plain", ""),
            ("release", ""),
            ("manual", "disable-model-invocation: true\n"),
        ] {
            let dir = root.path().join(".agents/skills").join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join("SKILL.md"),
                format!("---\nname: {name}\ndescription: The {name} flow\n{extra}---\nSteps.\n"),
            )
            .unwrap();
        }
        let home = tempfile::tempdir().unwrap();
        write_agents_file(
            home.path(),
            r#"{"mcpServers": {"gld": {"skillOverrides": {"release": "user-invocable-only", "manual": "on"}},
                                "ccnm": {"skillOverrides": {"plain": "off"}}},
                "skillOverrides": {"deploy": "off", "notes": "name-only"}}"#,
        );
        let mut scan =
            crate::agent_context::scan_skills(root.path(), &["custom".into()], ".agents/skills");
        with_home(home.path(), || narrow_skills(&mut scan));
        let names: Vec<&str> = scan
            .skills
            .iter()
            .map(|s| s.descriptor.name.as_str())
            .collect();
        assert_eq!(names.len(), 4, "{names:?}");
        assert!(!names.contains(&"deploy"), "{names:?}");
        let by = |name: &str| {
            &scan
                .skills
                .iter()
                .find(|s| s.descriptor.name == name)
                .unwrap()
                .descriptor
        };
        assert!(by("release").disable_model_invocation);
        assert!(
            by("manual").disable_model_invocation,
            "on cannot lift the author's switch"
        );
        assert!(
            !by("plain").disable_model_invocation,
            "ccnm's entry is not gld's"
        );
        assert!(by("notes").description.is_empty());

        let catalog =
            crate::agent_context::render_skill_catalog_for_profile(&scan.skills, "advanced").text;
        assert!(catalog.contains("- plain: The plain flow"), "{catalog}");
        assert!(catalog.contains("- notes (provider:"), "{catalog}");
        assert!(!catalog.contains("- release"), "{catalog}");
        assert!(!catalog.contains("deploy"), "{catalog}");

        // 写坏了：一个都不给，原因进 skipped。
        write_agents_file(home.path(), r#"{"skillOverrides": {"x": "hidden"}}"#);
        let mut scan =
            crate::agent_context::scan_skills(root.path(), &["custom".into()], ".agents/skills");
        with_home(home.path(), || narrow_skills(&mut scan));
        assert!(scan.skills.is_empty());
        assert!(scan
            .skipped
            .iter()
            .any(|s| s.reason.contains("skillOverrides.x")));
    }

    #[test]
    fn the_hub_and_remote_tools_count_as_known() {
        let known = known_tools();
        for name in [
            "read_file",
            "list_workspaces",
            "workspace_context",
            "remote_read_file",
        ] {
            assert!(known.iter().any(|k| k == name), "{name}: {known:?}");
        }
    }
}
