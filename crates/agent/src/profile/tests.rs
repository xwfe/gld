use super::*;

/// 每份内置档案的 `command` 是这家全放开的参数；有自动审批模式的三家给 `self_command`，
/// 里面不能混进全放开的参数（那就不是"自行判断"了）。
#[test]
fn the_builtin_profiles_give_full_access_and_self_approval_where_the_agent_has_it() {
    let profiles = builtin();
    for profile in profiles.values() {
        profile.check().expect("内置档案自己得过检查");
    }
    let full = |name: &str, flag: &str| {
        assert!(
            profiles[name].command.contains(&flag.to_string()),
            "{name} 的 command 要带 {flag}"
        );
    };
    full("codex", "--dangerously-bypass-approvals-and-sandbox");
    full("claude", "--dangerously-skip-permissions");
    full("qwen", "--yolo");
    full("agy", "--dangerously-skip-permissions");

    let own = |name: &str, flags: &[&str]| {
        let command = &profiles[name].self_command;
        for flag in flags {
            assert!(
                command.contains(&flag.to_string()),
                "{name} 的 self_command 要带 {flag}"
            );
        }
        for loud in [
            "--dangerously-bypass-approvals-and-sandbox",
            "--dangerously-skip-permissions",
            "--yolo",
        ] {
            assert!(
                !command.contains(&loud.to_string()),
                "{name} 的 self_command 混进了 {loud}"
            );
        }
    };
    own("codex", &["--approve-for-me"]);
    own(
        "claude",
        &["--permission-mode", "auto", "--permission-prompts", "none"],
    );
    own("qwen", &["--approval-mode", "auto"]);
    for name in ["agy", "pi", "kimi"] {
        assert!(
            !profiles[name].supports_self_approval(),
            "{name} 没有自行判断那一档"
        );
    }
    assert_eq!(profiles["claude"].stop_signal, StopSignal::Int);
    // json 要跑完才一次性吐，长任务会被"5 分钟没输出"当成卡住停掉；stream-json 边跑边出。
    // `-p` 下 stream-json 不带 --verbose 会直接报错（claude 2.1.304 的二进制里有这句报错）。
    for command in [
        &profiles["claude"].command,
        &profiles["claude"].self_command,
    ] {
        assert!(
            command
                .windows(2)
                .any(|w| w == ["--output-format", "stream-json"])
                && command.contains(&"--verbose".to_string()),
            "{command:?}"
        );
    }
    // 跑完才吐结果的，不能按"5 分钟没输出就是卡住"停掉。
    assert!(profiles["pi"].idle_timeout_secs >= 3600);
    assert!(profiles["agy"].idle_timeout_secs >= 3600);
}

#[test]
fn placeholders_are_filled_and_the_prompt_goes_to_stdin_unless_named() {
    let codex = &builtin()["codex"];
    let (argv, on_stdin) = codex.argv(
        Choice::default(),
        Path::new("/w"),
        Path::new("/t/result.txt"),
        "hi",
    );
    assert!(on_stdin);
    assert!(argv.contains(&"/w".to_string()) && argv.contains(&"/t/result.txt".to_string()));
    assert!(argv.contains(&"--dangerously-bypass-approvals-and-sandbox".to_string()));
    let own = Choice {
        self_approval: true,
        ..Choice::default()
    };
    let (argv, _) = codex.argv(own, Path::new("/w"), Path::new("/t/result.txt"), "hi");
    assert!(argv.contains(&"--approve-for-me".to_string()), "{argv:?}");

    let inline = &builtin()["kimi"];
    let (argv, on_stdin) = inline.argv(
        Choice::default(),
        Path::new("/w"),
        Path::new("/r"),
        "fix it",
    );
    assert!(!on_stdin);
    assert_eq!(argv, vec!["kimi", "-p", "fix it"]);
    // 没有自行判断那一档：给空的，绝不退回全放开的 command。
    assert!(inline
        .argv(own, Path::new("/w"), Path::new("/r"), "fix it")
        .0
        .is_empty());
}

/// 模型、推理强度按各家的参数加上：命令最后是 `-`（stdin 读提示）的加在它前面，别的加在末尾，
/// 不能插进 `-p {prompt}` 中间。
#[test]
fn model_and_effort_are_passed_where_each_agent_takes_them() {
    let profiles = builtin();
    let pick = Choice {
        self_approval: false,
        model: "m1",
        effort: "high",
    };
    let argv = |name: &str, choice: Choice<'_>| {
        profiles[name]
            .argv(choice, Path::new("/w"), Path::new("/r"), "fix")
            .0
    };
    let codex = argv("codex", pick);
    assert_eq!(
        codex[codex.len() - 5..],
        ["-m", "m1", "-c", "model_reasoning_effort=high", "-"],
        "{codex:?}"
    );
    let qwen = argv("qwen", Choice { effort: "", ..pick });
    assert_eq!(qwen, ["qwen", "-p", "fix", "--yolo", "-m", "m1"]);
    let claude = argv(
        "claude",
        Choice {
            self_approval: true,
            ..pick
        },
    );
    assert_eq!(
        claude[claude.len() - 4..],
        ["--model", "m1", "--effort", "high"]
    );
    let pi = argv("pi", pick);
    assert_eq!(pi[pi.len() - 4..], ["--model", "m1", "--thinking", "high"]);
    // 没给就一个都不加，用这家自己配置里的。
    assert!(!argv("codex", Choice::default()).contains(&"-m".to_string()));
}

/// AI 提议的（或放行时改的）取值，起之前就核对：这家没有那个参数、取值不在列表里、像开关的，都拒，不白花额度。
#[test]
fn proposals_are_checked_before_anything_runs() {
    let profiles = builtin();
    let error = |name: &str, model: Option<&str>, effort: Option<&str>| {
        profiles[name].resolve(model, effort).expect_err("该拒")
    };
    assert!(error("qwen", None, Some("high")).contains("没有推理强度"));
    assert!(error("kimi", None, Some("high")).contains("没有推理强度"));
    assert!(error("claude", None, Some("turbo")).contains("只能是"));
    assert!(error("agy", None, Some("xhigh")).contains("只能是"));
    assert!(error("codex", Some("-c"), None).contains("不能以 - 开头"));
    assert!(error("codex", Some("gpt 5"), None).contains("只能用"));
    assert_eq!(
        profiles["codex"].resolve(Some("gpt-5.4"), Some("ultra")),
        Ok(("gpt-5.4".into(), "ultra".into())),
        "codex 的取值帮助里没列，不在 gld 这边核对"
    );
    assert_eq!(
        profiles["claude"].resolve(None, None),
        Ok((String::new(), String::new()))
    );
}

/// `agents.toml` 里不写 command 就是在同名内置档案上改：只改默认模型和推理强度，启动参数照旧。
#[test]
fn agents_toml_can_set_just_the_default_model_and_effort() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("agents.toml");
    std::fs::write(
        &path,
        "[agents.codex]\nmodel = \"gpt-5.4\"\neffort = \"high\"\n",
    )
    .expect("write");
    let profiles = load(&path).expect("读得进来");
    let codex = &profiles["codex"];
    assert_eq!(codex.command, builtin()["codex"].command, "启动参数照旧");
    assert_eq!(
        codex.resolve(None, None),
        Ok(("gpt-5.4".into(), "high".into()))
    );
    assert_eq!(
        codex.resolve(Some("o4"), None),
        Ok(("o4".into(), "high".into())),
        "提议了模型就用提议的，强度还是默认"
    );

    for (text, expected) in [
            ("[agents.nobody]\nmodel = \"x\"\n", "没写 command"),
            ("[agents.claude]\neffort = \"turbo\"\n", "默认模型或推理强度不对"),
            ("[agents.qwen]\neffort = \"high\"\n", "没有推理强度"),
            ("[agents.x]\ncommand = [\"x\"]\n", "也要写 result"),
            ("[agents.x]\ncommand = [\"x\"]\nresult = { from = \"stdout\" }\nmodel_args = [\"-m\"]\n", "{model}"),
        ] {
            std::fs::write(&path, text).expect("write");
            let error = load(&path).expect_err(text);
            assert!(error.contains(expected), "{text} → {error}");
        }
}

#[test]
fn agents_toml_adds_and_replaces_profiles() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("agents.toml");
    assert_eq!(load(&path).expect("没有文件").len(), builtin().len());
    std::fs::write(
        &path,
        r#"
[agents.opencode]
command = ["opencode", "run", "--auto"]
result = { from = "stdout" }

[agents.qoder]
command = ["qodercli", "-p", "{prompt}", "--yolo"]
self_command = ["qodercli", "-p", "{prompt}", "--permission-mode", "auto"]
result = { from = "stdout" }

[agents.claude]
command = ["claude", "-p", "--output-format", "json"]
result = { from = "json", field = "result" }
idle_timeout_secs = 60
"#,
    )
    .expect("write");
    let profiles = load(&path).expect("读得进来");
    assert_eq!(profiles["opencode"].result.from, ResultFrom::Stdout);
    assert!(!profiles["opencode"].supports_self_approval());
    assert!(profiles["qoder"].supports_self_approval());
    assert_eq!(profiles["claude"].idle_timeout_secs, 60);
    assert!(!profiles["claude"]
        .command
        .contains(&"--dangerously-skip-permissions".to_string()));
    assert!(
        !profiles["claude"].supports_self_approval(),
        "同名整条换掉，内置的 self_command 不留"
    );
    assert!(profiles.contains_key("codex"));
}

#[test]
fn a_broken_agents_toml_is_reported_instead_of_ignored() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("agents.toml");
    for text in [
            "[agents.x]\ncommand = []\nresult = { from = \"stdout\" }\n",
            "[agents.x]\ncommand = [\"x\"]\nresult = { from = \"json\" }\n",
            "[agents.x]\ncommand = [\"x\"]\nresult = { from = \"file\" }\n",
            "[agents.x]\ncommand = [\"x\"]\nresult = { from = \"stdout\" }\nbogus = 1\n",
            "[agents.\"a b\"]\ncommand = [\"x\"]\nresult = { from = \"stdout\" }\n",
            "[agents.x]\ncommand = [\"x\"]\nself_command = [\"\"]\nresult = { from = \"stdout\" }\n",
            "[agents.x]\ncommand = [\"x\", \"{result_file}\"]\nself_command = [\"x\"]\nresult = { from = \"file\" }\n",
        ] {
            std::fs::write(&path, text).expect("write");
            let error = load(&path).expect_err(text);
            assert!(
                error.contains("agents.toml") || error.contains("档案"),
                "{error}"
            );
        }
}
