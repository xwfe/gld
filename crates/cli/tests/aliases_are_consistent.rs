//! 同一个动作，在哪一层都得能用同一个简写。
//!
//! `gld ws ls` 能用而 `gld frp ls` 不能——这种不一致不会报错，只会让人每次
//! 都得先想一下"这条命令有没有简写"，想错了就吃一句 `unrecognized subcommand`。
//! 而它极容易发生：加一组新子命令时，`list` / `remove` 是顺手写下的，
//! 别名要另外加一行，忘了也没人会发现。
//!
//! 所以规则写在这里，由测试守着：叫 `list` 就必须能敲 `ls`，叫 `remove`
//! 就必须能敲 `rm`。新加的子命令自动被这条规则罩住。

use clap::CommandFactory;

/// 命令名 → 它必须提供的简写。
const REQUIRED: &[(&str, &str)] = &[("list", "ls"), ("remove", "rm"), ("delete", "rm")];

#[test]
fn every_list_and_remove_command_has_the_usual_short_alias() {
    let root = gld::Cli::command();
    let mut missing = Vec::new();
    let mut checked = 0usize;

    walk(&root, "gld", &mut |command, path| {
        let Some((_, alias)) = REQUIRED
            .iter()
            .find(|(name, _)| *name == command.get_name())
        else {
            return;
        };
        checked += 1;
        if !command.get_all_aliases().any(|existing| existing == *alias) {
            missing.push(format!("{path}（应当能写成 {alias}）"));
        }
    });

    assert!(
        missing.is_empty(),
        "这些命令缺少惯用简写：\n  {}\n在 crates/cli/src/cli.rs 对应的变体上加 \
         #[command(visible_alias = \"…\")]",
        missing.join("\n  ")
    );
    // 规则本身失效时要能看出来：命令树重构后一条都没匹配上，
    // 上面那个断言会毫无意义地通过。
    assert!(
        checked >= 4,
        "只检查到 {checked} 条 list / remove 命令，遍历逻辑可能失效了"
    );
}

/// 别名不能撞车：同一层里两条命令抢同一个简写，clap 只认先定义的那条，
/// 另一条会安静地失效——敲下去执行的是别人。
#[test]
fn no_two_sibling_commands_claim_the_same_name() {
    let root = gld::Cli::command();
    let mut clashes = Vec::new();

    walk_groups(&root, "gld", &mut |parent, path| {
        let mut seen: Vec<(String, String)> = Vec::new();
        for sub in parent.get_subcommands() {
            let owner = sub.get_name().to_string();
            for name in std::iter::once(sub.get_name()).chain(sub.get_all_aliases()) {
                if let Some((_, first)) = seen.iter().find(|(taken, _)| taken == name) {
                    clashes.push(format!("{path} 下的「{name}」同时属于 {first} 和 {owner}"));
                } else {
                    seen.push((name.to_string(), owner.clone()));
                }
            }
        }
    });

    assert!(clashes.is_empty(), "别名撞车：\n  {}", clashes.join("\n  "));
}

fn walk(command: &clap::Command, path: &str, visit: &mut impl FnMut(&clap::Command, &str)) {
    for sub in command.get_subcommands() {
        let full = format!("{path} {}", sub.get_name());
        visit(sub, &full);
        walk(sub, &full, visit);
    }
}

/// 只遍历"有子命令的命令"，包括根。
fn walk_groups(command: &clap::Command, path: &str, visit: &mut impl FnMut(&clap::Command, &str)) {
    if command.get_subcommands().next().is_none() {
        return;
    }
    visit(command, path);
    for sub in command.get_subcommands() {
        walk_groups(sub, &format!("{path} {}", sub.get_name()), visit);
    }
}
