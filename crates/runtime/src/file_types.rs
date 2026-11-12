//! `search_text` 的 `type` 过滤：类型名 → 认哪些文件。
//!
//! 名字照 ripgrep 的 `--type`（`rg --type-list`），因为模型是照着 rg 的习惯
//! 写参数的；但**这里不是 rg 的全集**——gld 自己遍历文件，没有 rg 那份 100 多
//! 条的表，只列常用的。给了不认识的类型不是悄悄不过滤，而是报错并把支持的
//! 类型列出来：悄悄不过滤会让模型以为"这个类型里没有匹配"。
//!
//! 匹配看两样东西：扩展名（不分大小写），以及少数没有扩展名的文件名
//! （`Dockerfile`、`Makefile`）。

/// `(类型名, 扩展名, 整个文件名)`。同一类型的别名各占一行，查表按名字找，
/// 多一行比多一层间接便宜。
const TYPES: &[(&str, &[&str], &[&str])] = &[
    ("rust", &["rs"], &[]),
    ("py", &["py", "pyi"], &[]),
    ("python", &["py", "pyi"], &[]),
    ("js", &["js", "jsx", "mjs", "cjs"], &[]),
    ("ts", &["ts", "tsx", "mts", "cts"], &[]),
    ("go", &["go"], &[]),
    ("java", &["java"], &[]),
    ("kotlin", &["kt", "kts"], &[]),
    ("c", &["c", "h"], &[]),
    ("cpp", &["cpp", "cc", "cxx", "hpp", "hh", "hxx"], &[]),
    ("cs", &["cs"], &[]),
    ("rb", &["rb"], &[]),
    ("ruby", &["rb"], &[]),
    ("php", &["php"], &[]),
    ("swift", &["swift"], &[]),
    ("sh", &["sh", "bash", "zsh", "ksh"], &[]),
    ("md", &["md", "markdown"], &[]),
    ("markdown", &["md", "markdown"], &[]),
    ("json", &["json"], &[]),
    ("yaml", &["yaml", "yml"], &[]),
    ("yml", &["yaml", "yml"], &[]),
    ("toml", &["toml"], &[]),
    ("xml", &["xml"], &[]),
    ("html", &["html", "htm"], &[]),
    ("css", &["css", "scss", "sass", "less"], &[]),
    ("sql", &["sql"], &[]),
    ("vue", &["vue"], &[]),
    ("svelte", &["svelte"], &[]),
    ("lua", &["lua"], &[]),
    ("proto", &["proto"], &[]),
    ("docker", &[], &["Dockerfile"]),
    ("make", &["mk"], &["Makefile", "GNUmakefile"]),
];

/// 一个类型认哪些文件。不认识的类型返回 `None`。
pub(crate) fn lookup(name: &str) -> Option<FileType> {
    let name = name.trim().to_ascii_lowercase();
    TYPES
        .iter()
        .find(|(type_name, ..)| *type_name == name)
        .map(|(_, extensions, file_names)| FileType {
            extensions,
            file_names,
        })
}

/// 支持的类型名，排好序，用在报错里。
pub(crate) fn known_names() -> Vec<&'static str> {
    let mut names = TYPES
        .iter()
        .map(|(name, ..)| *name)
        .collect::<Vec<&'static str>>();
    names.sort_unstable();
    names
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct FileType {
    extensions: &'static [&'static str],
    file_names: &'static [&'static str],
}

impl FileType {
    /// 这个**工作区相对路径**算不算这个类型。
    pub(crate) fn matches(&self, relative_path: &str) -> bool {
        let name = relative_path
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(relative_path);
        if self
            .file_names
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(name))
        {
            return true;
        }
        // `.gitignore` 这种以点开头、没有扩展名的文件不算任何类型：
        // `rsplit_once('.')` 会把它的"扩展名"读成 `gitignore`。
        let Some((stem, extension)) = name.rsplit_once('.') else {
            return false;
        };
        if stem.is_empty() {
            return false;
        }
        self.extensions
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(extension))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_type_matches_by_extension_ignoring_case() {
        let rust = lookup("rust").expect("rust");
        assert!(rust.matches("crates/core/src/lib.rs"));
        assert!(rust.matches("SRC/MAIN.RS"));
        assert!(!rust.matches("README.md"));
    }

    #[test]
    fn a_type_can_match_a_whole_file_name() {
        let docker = lookup("docker").expect("docker");
        assert!(docker.matches("deploy/Dockerfile"));
        assert!(!docker.matches("deploy/docker-compose.yml"));
    }

    /// `.gitignore` 的"扩展名"是 `gitignore`，别让它变成某个类型的成员。
    #[test]
    fn a_dotfile_without_an_extension_is_not_typed() {
        let md = lookup("md").expect("md");
        assert!(!md.matches(".md"));
        assert!(md.matches("docs/plan.md"));
    }

    #[test]
    fn aliases_point_at_the_same_extensions() {
        assert!(lookup("python").expect("python").matches("a/b.py"));
        assert!(lookup("py").expect("py").matches("a/b.py"));
        assert!(lookup("nope").is_none());
    }
}
