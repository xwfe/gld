//! 一个路径能不能写、要不要先确认——全仓只在这里判一次。
//!
//! 以前这件事在两个地方各判一遍：`patch.rs` 判"是不是受保护资产"，
//! `workspace.rs::reject_protected_write_path` 又判一遍，两边都把
//! `.git` 和 `.github` 当成同一种东西一律拒绝。后果是**新增一个
//! `.github/workflows/ci.yml` 会报「禁止删除仓库保护资产」**——既不是删除，
//! 报的理由也和实际操作对不上，用户为了修 Actions 只能绕过工具用别的办法写
//! 文件（审查 C05、E02，方案 C4）。
//!
//! 现在按"改了会发生什么"分开：
//!
//! | 这一类 | 是什么 | 改 | 删 |
//! | --- | --- | --- | --- |
//! | `.git/**` | Git 自己的对象库和配置，不是仓库源文件 | 永远拒 | 永远拒 |
//! | `.github/workflows/**`、`.github/actions/**` | 改一行就能改变 CI 在 GitHub 上执行什么 | 要 confirm | 要 confirm |
//! | `.github/CODEOWNERS` | 决定谁有权批准 PR | 要 confirm | 要 confirm |
//! | `.github/**` 其他 | issue 模板、说明文档这类 | 放行 | 要 confirm |
//! | 关键项目文件 | `Cargo.toml`、`package.json`、`README*` … | 放行 | 要 confirm |
//! | 其他 | | 放行 | 放行 |
//!
//! **`.github` 不是无风险配置。**放行的是"改"，不是"随便改"：workflow 和
//! CODEOWNERS 仍然要调用方显式带 `confirm=true`，理由会写在拒绝消息里，让用户
//! 知道自己在批准什么。
//!
//! 这里只管**普通文件工具**（`apply_patch` / `patch_check`）的写入。子进程
//! 能干什么是另一层，在 `policy.rs`：那一层看的是命令文本，仍然把 `.git` 和
//! `.github` 一起当删除保护对象，因为从命令文本里分不出"改一行 workflow"和
//! "把 .github 递归删掉"。两层的宽严不同是有意的，不要为了"一致"把哪一边拉齐。

/// 一个写目标属于哪一类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteClass {
    /// `.git/**`：Git 的对象库、索引、配置。
    GitInternals,
    /// `.github/workflows/**`、`.github/actions/**`：改得动 CI 的执行行为。
    CiWorkflow,
    /// `.github/CODEOWNERS`：改得动"谁能批准这次合并"。
    CodeOwners,
    /// `.github/**` 的其余部分：issue / PR 模板、贡献说明等。
    GithubOther,
    /// 改坏了会让项目起不来或语义大变的文件，主要防的是**删除**。
    CriticalFile,
    /// 普通源文件。
    Ordinary,
}

/// 这次写入的判定结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    /// 调用方带 `confirm=true` 就能做；`&'static str` 是**为什么要确认**，
    /// 直接写进拒绝消息——"需要确认"本身不构成信息，用户要知道批的是什么。
    NeedsConfirm(&'static str),
    /// 这条路走不通，确认也没用。
    Deny(&'static str),
}

impl WriteClass {
    /// 按工作区相对路径分类。传进来的路径可以是 `a/b` 或 `a\b`。
    pub fn of(path: &str) -> Self {
        let normalized = path.replace('\\', "/");
        let normalized = normalized.strip_prefix("./").unwrap_or(&normalized);
        let mut parts = normalized.split('/');
        let first = parts.next().unwrap_or("");
        match first {
            ".git" => return Self::GitInternals,
            ".github" => {
                let second = parts.next().unwrap_or("");
                return match second {
                    // 目录本身（".github"）没有第二段，当成 GithubOther：
                    // 补丁的目标总是文件，这种形状只会来自畸形输入。
                    "workflows" | "actions" => Self::CiWorkflow,
                    "CODEOWNERS" => Self::CodeOwners,
                    _ => Self::GithubOther,
                };
            }
            _ => {}
        }
        if is_critical_file_name(normalized) {
            return Self::CriticalFile;
        }
        Self::Ordinary
    }

    /// 新建、更新、整文件覆盖。
    pub fn modify(self) -> Verdict {
        match self {
            Self::GitInternals => Verdict::Deny(
                "`.git/` 里是 Git 自己的对象库和配置，普通文件工具不写它；要动仓库状态请用 git 命令",
            ),
            Self::CiWorkflow => Verdict::NeedsConfirm(
                "这是 CI 工作流：改完之后 GitHub 上跑的就是新内容。确认要改就带 confirm=true 重试",
            ),
            Self::CodeOwners => Verdict::NeedsConfirm(
                "CODEOWNERS 决定谁能批准合并。确认要改就带 confirm=true 重试",
            ),
            Self::GithubOther | Self::CriticalFile | Self::Ordinary => Verdict::Allow,
        }
    }

    /// 删除整个文件。
    pub fn delete(self) -> Verdict {
        match self {
            Self::GitInternals => self.modify(),
            Self::CiWorkflow => Verdict::NeedsConfirm(
                "删掉这个工作流，它负责的那道 CI 检查就没有了。确认要删就带 confirm=true 重试",
            ),
            Self::CodeOwners => Verdict::NeedsConfirm(
                "删掉 CODEOWNERS 等于取消所有代码所有者的审批要求。确认要删就带 confirm=true 重试",
            ),
            Self::GithubOther => {
                Verdict::NeedsConfirm("这是仓库的 `.github/` 配置文件，删除要带 confirm=true")
            }
            Self::CriticalFile => Verdict::NeedsConfirm("删除关键项目文件要带 confirm=true"),
            Self::Ordinary => Verdict::Allow,
        }
    }

    /// 给结果里的标注用：这次碰的文件里有没有敏感的。
    pub fn is_sensitive(self) -> bool {
        matches!(self, Self::CiWorkflow | Self::CodeOwners)
    }

    /// 放进响应和日志的短名字。
    pub fn slug(self) -> &'static str {
        match self {
            Self::GitInternals => "git_internals",
            Self::CiWorkflow => "ci_workflow",
            Self::CodeOwners => "code_owners",
            Self::GithubOther => "github_config",
            Self::CriticalFile => "critical_file",
            Self::Ordinary => "ordinary",
        }
    }
}

/// 删了会让项目起不来、或者让"这是什么项目"这个问题失去答案的文件。
///
/// 只影响**删除**：改它们是日常工作（加一个依赖就要改 `Cargo.toml`）。
fn is_critical_file_name(normalized_path: &str) -> bool {
    let name = normalized_path
        .rsplit('/')
        .next()
        .unwrap_or(normalized_path);
    name == ".gitignore"
        || name == "Cargo.toml"
        || name == "Cargo.lock"
        || name == "package.json"
        || name == "package-lock.json"
        || name == "pnpm-lock.yaml"
        || name == "tauri.conf.json"
        || name.starts_with("README")
        || name.starts_with("LICENSE")
        || name.starts_with("vite.config.")
        || name == "pyproject.toml"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_internals_are_never_writable() {
        for path in [".git/config", ".git/objects/ab/cdef", "./.git/HEAD"] {
            assert_eq!(WriteClass::of(path), WriteClass::GitInternals, "{path}");
            assert!(matches!(WriteClass::of(path).modify(), Verdict::Deny(_)));
            assert!(matches!(WriteClass::of(path).delete(), Verdict::Deny(_)));
        }
    }

    /// 这条是 E02 的回归：新建一个 workflow 以前报"禁止删除仓库保护资产"。
    #[test]
    fn a_new_workflow_is_a_confirmable_change_not_a_deletion() {
        let class = WriteClass::of(".github/workflows/ci.yml");
        assert_eq!(class, WriteClass::CiWorkflow);
        assert!(matches!(class.modify(), Verdict::NeedsConfirm(_)));
        assert!(class.is_sensitive());
    }

    #[test]
    fn github_templates_are_ordinary_to_edit_but_confirmed_to_delete() {
        let class = WriteClass::of(".github/ISSUE_TEMPLATE/bug.md");
        assert_eq!(class, WriteClass::GithubOther);
        assert_eq!(class.modify(), Verdict::Allow);
        assert!(matches!(class.delete(), Verdict::NeedsConfirm(_)));
        assert!(!class.is_sensitive());
    }

    #[test]
    fn code_owners_needs_confirmation_both_ways() {
        let class = WriteClass::of(".github/CODEOWNERS");
        assert_eq!(class, WriteClass::CodeOwners);
        assert!(matches!(class.modify(), Verdict::NeedsConfirm(_)));
        assert!(matches!(class.delete(), Verdict::NeedsConfirm(_)));
    }

    #[test]
    fn critical_files_are_editable_and_only_guarded_on_delete() {
        let class = WriteClass::of("Cargo.toml");
        assert_eq!(class, WriteClass::CriticalFile);
        assert_eq!(class.modify(), Verdict::Allow);
        assert!(matches!(class.delete(), Verdict::NeedsConfirm(_)));
    }

    #[test]
    fn ordinary_files_stay_out_of_the_way() {
        let class = WriteClass::of("src/main.rs");
        assert_eq!(class, WriteClass::Ordinary);
        assert_eq!(class.modify(), Verdict::Allow);
        assert_eq!(class.delete(), Verdict::Allow);
    }

    /// Windows 的反斜杠和 `./` 前缀不能把分类绕过去。
    #[test]
    fn separators_and_prefixes_do_not_change_the_class() {
        assert_eq!(
            WriteClass::of(r".github\workflows\release.yml"),
            WriteClass::CiWorkflow
        );
        assert_eq!(
            WriteClass::of("./.github/CODEOWNERS"),
            WriteClass::CodeOwners
        );
        assert_eq!(WriteClass::of(r".git\config"), WriteClass::GitInternals);
    }

    /// 名字里带 `.github` 的普通文件不该被误判。
    #[test]
    fn a_file_merely_named_like_github_is_ordinary() {
        assert_eq!(
            WriteClass::of("docs/.github-notes.md"),
            WriteClass::Ordinary
        );
        assert_eq!(WriteClass::of("tools/github/api.rs"), WriteClass::Ordinary);
    }
}
