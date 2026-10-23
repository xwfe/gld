//! 用假 Agent 脚本测委派的全部状态：不花任何额度。
//!
//! 每条测试一个临时数据目录（放 `agents.toml` 和任务记录）、一个临时 git 项目。
//! 脚本照档案吐结果、改文件、卡住、被停、失败。

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use super::store::State;
use super::*;

struct Fixture {
    home: tempfile::TempDir,
    project: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let project = tempfile::tempdir().expect("project");
        let ok = Command::new("git")
            .args(["init", "-q"])
            .current_dir(project.path())
            .status()
            .expect("git init")
            .success();
        assert!(ok, "git init 失败");
        Self {
            home: tempfile::tempdir().expect("home"),
            project,
        }
    }

    fn data_home(&self) -> &Path {
        self.home.path()
    }

    fn root(&self) -> PathBuf {
        self.project.path().canonicalize().expect("canonical")
    }

    /// 放一个假 Agent 脚本，并在 agents.toml 里登记成 `name`。
    fn agent(&self, name: &str, script: &str, result: &str, extra: &str) -> PathBuf {
        let path = self.home.path().join(format!("{name}.sh"));
        let version = "if [ \"$1\" = \"--version\" ]; then echo 'fake-agent 1.0'; exit 0; fi";
        std::fs::write(&path, format!("#!/bin/sh\n{version}\n{script}\n")).expect("script");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let toml = agents_toml(self.data_home());
        let mut text = std::fs::read_to_string(&toml).unwrap_or_default();
        let args = if result.contains("file") {
            format!("[\"{}\", \"{{result_file}}\"]", path.display())
        } else {
            format!("[\"{}\"]", path.display())
        };
        text.push_str(&format!(
            "[agents.{name}]\ncommand = {args}\nresult = {result}\n{extra}\n"
        ));
        std::fs::write(toml, text).expect("agents.toml");
        path
    }

    fn start(&self, agent: &str, approval: Approval) -> Result<AgentTask, AgentError> {
        self.start_with(agent, approval, None)
    }

    fn start_with(
        &self,
        agent: &str,
        approval: Approval,
        client_request_id: Option<&str>,
    ) -> Result<AgentTask, AgentError> {
        self.start_as(agent, approval, client_request_id, "all")
    }

    fn start_as(
        &self,
        agent: &str,
        approval: Approval,
        client_request_id: Option<&str>,
        agent_approval: &str,
    ) -> Result<AgentTask, AgentError> {
        start(StartRequest {
            data_home: self.data_home(),
            workspace_root: self.project.path(),
            workspace_name: "demo",
            caller: "local",
            agent,
            prompt: "把 README 改好",
            brief: Some("背景：这是测试"),
            client_request_id,
            approval,
            agent_approval,
            model: None,
            effort: None,
            timeout_minutes: None,
        })
    }

    /// auto 档起一个，带上 AI 提议的模型和推理强度。
    fn start_choosing(
        &self,
        agent: &str,
        model: Option<&str>,
        effort: Option<&str>,
    ) -> Result<AgentTask, AgentError> {
        start(StartRequest {
            data_home: self.data_home(),
            workspace_root: self.project.path(),
            workspace_name: "demo",
            caller: "local",
            agent,
            prompt: "把 README 改好",
            brief: None,
            client_request_id: None,
            approval: Approval::Auto,
            agent_approval: "all",
            model,
            effort,
            timeout_minutes: None,
        })
    }

    /// 给刚登记的档案补一条 `self_command`（同一个脚本，多带一个参数 `self`）。
    /// 档案是追加在 agents.toml 末尾的，紧跟着写的键就归它。
    fn add_self_command(&self, script: &Path) {
        let toml = agents_toml(self.data_home());
        let mut text = std::fs::read_to_string(&toml).expect("agents.toml");
        text.push_str(&format!(
            "self_command = [\"{}\", \"self\"]\n",
            script.display()
        ));
        std::fs::write(toml, text).expect("agents.toml");
    }

    fn finished(&self, task: &AgentTask) -> AgentTask {
        let (dir, _) = get(self.data_home(), &task.task_id).expect("任务");
        run::wait_until_finished(&dir, Duration::from_secs(20))
    }

    fn detail_of(&self, task: &AgentTask) -> TaskDetail {
        let (dir, task) = get(self.data_home(), &task.task_id).expect("任务");
        detail(&dir, task)
    }

    /// 在项目里跑 git，要求成功，返回 stdout。
    fn git(&self, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args(args)
            .current_dir(self.project.path())
            .output()
            .expect("git");
        assert!(
            output.status.success(),
            "git {args:?}：{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    fn write(&self, name: &str, text: &str) {
        std::fs::write(self.project.path().join(name), text).expect("write");
    }
}

const STDOUT: &str = "{ from = \"stdout\" }";

#[test]
fn an_auto_task_runs_edits_and_comes_back_for_review() {
    let f = Fixture::new();
    f.agent(
        "fake",
        "cat > prompt-seen.txt\necho changed > touched.txt\necho 'recap: 做了一处改动'",
        STDOUT,
        "",
    );
    let task = f.start("fake", Approval::Auto).expect("起得来");
    assert_eq!(task.state, State::Running);
    let done = f.finished(&task);
    assert_eq!(done.state, State::AwaitingReview, "{done:?}");
    assert_eq!(done.exit_code, Some(0));
    assert!(done
        .recap
        .as_deref()
        .unwrap_or("")
        .contains("recap: 做了一处改动"));
    assert_eq!(
        done.changed_files,
        vec!["prompt-seen.txt".to_string(), "touched.txt".to_string()],
        "交接目录在 .gld/ 里，自带 .gitignore，不该算进改动"
    );

    let seen = std::fs::read_to_string(f.root().join("prompt-seen.txt")).expect("提示");
    assert!(seen.starts_with("把 README 改好"));
    assert!(seen.contains(&format!(".gld/handoff/{}", task.task_id)));
    assert!(seen.contains("brief.md") && seen.contains("不要 git commit"));
    let handoff = handoff_dir(&f.root(), &task.task_id);
    assert_eq!(
        std::fs::read_to_string(handoff.join("brief.md")).expect("brief"),
        "背景：这是测试"
    );
    assert!(
        std::fs::read_to_string(f.root().join(".gld/.gitignore"))
            .expect("gitignore")
            .lines()
            .any(|line| line == "*"),
        ".gld/ 第一次由委派建出来时，和 Planning 一样放忽略全部的 .gitignore"
    );
    assert_eq!(
        std::fs::read_to_string(f.root().join(".gld/handoff/.gitignore")).expect("gitignore"),
        "*\n"
    );
    assert!(running_in(&f.root()).is_none(), "跑完要放开项目");
}

/// 用户删掉 `.gld/.gitignore` 是想让团队共享 Planning（concepts.md 写明"之后不会再补回来"），
/// 委派不能把它补回来；交接文件照样不进 git，也不算进 Agent 的改动。
#[test]
fn a_shared_gld_directory_stays_shared_but_handoff_files_stay_out_of_git() {
    let f = Fixture::new();
    let planning = f.root().join(".gld/planning");
    std::fs::create_dir_all(&planning).expect("planning");
    std::fs::write(planning.join("state.json"), "{}").expect("state");
    let committed = Command::new("git")
        .args(["add", "-A"])
        .current_dir(f.root())
        .status()
        .expect("git add")
        .success()
        && Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args(["commit", "-q", "-m", "share planning"])
            .current_dir(f.root())
            .status()
            .expect("git commit")
            .success();
    assert!(committed, "共享 Planning 的提交没做成");

    f.agent(
        "fake",
        "cat >/dev/null\necho x > touched.txt\necho ok",
        STDOUT,
        "",
    );
    let done = f.finished(&f.start("fake", Approval::Auto).expect("起"));
    assert!(
        !f.root().join(".gld/.gitignore").exists(),
        "用户共享着 .gld/，委派不该补回 .gitignore"
    );
    assert!(f.root().join(".gld/handoff/.gitignore").exists());
    assert_eq!(
        done.changed_files,
        vec!["touched.txt".to_string()],
        "{done:?}"
    );
}

#[test]
fn json_and_file_results_are_read_as_the_profile_says() {
    let f = Fixture::new();
    f.agent(
        "jsonish",
        "cat >/dev/null\necho 'progress line'\necho '{\"result\":\"json recap\",\"cost\":1}'",
        "{ from = \"json\", field = \"result\" }",
        "",
    );
    f.agent(
        "fileish",
        "cat >/dev/null\necho noise\necho 'file recap' > \"$1\"",
        "{ from = \"file\" }",
        "",
    );
    let json = f.finished(&f.start("jsonish", Approval::Auto).expect("起"));
    assert_eq!(json.recap.as_deref(), Some("json recap"), "{json:?}");
    let file = f.finished(&f.start("fileish", Approval::Auto).expect("起"));
    assert_eq!(file.recap.as_deref(), Some("file recap"), "{file:?}");
}

#[test]
fn a_nonzero_exit_or_missing_result_fails_but_still_lists_changes() {
    let f = Fixture::new();
    f.agent(
        "broken",
        "cat >/dev/null\necho x > half.txt\nexit 3",
        STDOUT,
        "",
    );
    let done = f.finished(&f.start("broken", Approval::Auto).expect("起"));
    assert_eq!(done.state, State::Failed);
    assert_eq!(done.exit_code, Some(3));
    assert_eq!(done.changed_files, vec!["half.txt".to_string()]);

    f.agent("silent", "cat >/dev/null", "{ from = \"file\" }", "");
    let done = f.finished(&f.start("silent", Approval::Auto).expect("起"));
    assert_eq!(done.state, State::Failed);
    assert!(
        done.failure.as_deref().unwrap_or("").contains("结果文件"),
        "{done:?}"
    );
}

#[test]
fn ask_waits_for_local_approval_and_deny_or_stop_ends_it() {
    let f = Fixture::new();
    f.agent("fake", "cat >/dev/null\necho ok", STDOUT, "");
    let waiting = f.start("fake", Approval::Ask).expect("存下");
    assert_eq!(waiting.state, State::WaitingLocalApproval);
    assert!(running_in(&f.root()).is_none(), "等放行时不占项目");
    let started = approve(f.data_home(), &waiting.task_id).expect("放行");
    assert_eq!(started.state, State::Running);
    assert_eq!(f.finished(&started).state, State::AwaitingReview);
    assert_eq!(
        approve(f.data_home(), &waiting.task_id)
            .expect_err("跑过了")
            .code,
        "AGENT_TASK_STATE"
    );

    let other = f.start("fake", Approval::Ask).expect("存下");
    assert_eq!(
        deny(f.data_home(), &other.task_id).expect("拒").state,
        State::Denied
    );
    let third = f.start("fake", Approval::Ask).expect("存下");
    assert_eq!(
        stop(f.data_home(), &third.task_id).expect("停").state,
        State::Stopped
    );
}

#[test]
fn one_project_runs_one_task_at_a_time_and_stop_frees_it() {
    let f = Fixture::new();
    f.agent(
        "ticker",
        "cat >/dev/null\nwhile true; do echo tick; sleep 0.2; done",
        STDOUT,
        "",
    );
    let first = f.start("ticker", Approval::Auto).expect("起");
    assert_eq!(
        running_in(&f.root()).as_deref(),
        Some(first.task_id.as_str())
    );
    let busy = f
        .start("ticker", Approval::Auto)
        .expect_err("同一个项目只许一个");
    assert_eq!(busy.code, "AGENT_TASK_RUNNING");
    let me = std::process::id();
    assert_eq!(running_under(f.data_home(), me).len(), 1);

    stop(f.data_home(), &first.task_id).expect("叫停");
    let stopped = f.finished(&first);
    assert_eq!(stopped.state, State::Stopped, "{stopped:?}");
    assert!(running_in(&f.root()).is_none());
    assert!(running_under(f.data_home(), me).is_empty());
    let again = f.start("ticker", Approval::Auto).expect("停了就能再起");
    stop(f.data_home(), &again.task_id).expect("收掉");
    f.finished(&again);
}

#[test]
fn a_silent_agent_is_stopped_as_stuck() {
    let f = Fixture::new();
    f.agent(
        "stuck",
        "cat >/dev/null\nsleep 30",
        STDOUT,
        "idle_timeout_secs = 1",
    );
    let done = f.finished(&f.start("stuck", Approval::Auto).expect("起"));
    assert_eq!(done.state, State::Timeout, "{done:?}");
    assert!(done
        .failure
        .as_deref()
        .unwrap_or("")
        .contains("没有任何输出"));
}

#[test]
fn review_records_the_verdict_and_only_after_the_agent_is_done() {
    let f = Fixture::new();
    f.agent("fake", "cat >/dev/null\necho ok", STDOUT, "");
    let waiting = f.start("fake", Approval::Ask).expect("存下");
    assert_eq!(
        review(f.data_home(), &waiting.task_id, "accept", "")
            .expect_err("还没跑")
            .code,
        "AGENT_TASK_STATE"
    );
    let done = f.finished(&approve(f.data_home(), &waiting.task_id).expect("放行"));
    assert_eq!(
        review(f.data_home(), &done.task_id, "maybe", "")
            .expect_err("不认识")
            .code,
        "INVALID_ARGUMENT"
    );
    let rejected = review(f.data_home(), &done.task_id, "reject", "测试没跑全").expect("审");
    assert_eq!(rejected.state, State::Rejected);
    assert_eq!(
        rejected.review.as_ref().map(|r| r.note.as_str()),
        Some("测试没跑全")
    );
    let note = std::fs::read_to_string(handoff_dir(&f.root(), &done.task_id).join("review.md"))
        .expect("review.md");
    assert!(note.contains("reject") && note.contains("测试没跑全"));
}

#[test]
fn the_same_client_request_id_returns_the_same_task() {
    let f = Fixture::new();
    f.agent("fake", "cat >/dev/null\necho ok", STDOUT, "");
    let first = f
        .start_with("fake", Approval::Ask, Some("req-1"))
        .expect("存下");
    let again = f
        .start_with("fake", Approval::Ask, Some("req-1"))
        .expect("同一个");
    assert_eq!(first.task_id, again.task_id);
    assert_eq!(list(f.data_home(), None).len(), 1);
}

#[test]
fn unknown_agents_and_empty_prompts_are_refused() {
    let f = Fixture::new();
    let error = f.start("nobody", Approval::Auto).expect_err("没有这个档案");
    assert_eq!(error.code, "AGENT_UNKNOWN");
    assert!(error.message.contains("codex") && error.message.contains("claude"));
    let error = start(StartRequest {
        data_home: f.data_home(),
        workspace_root: f.project.path(),
        workspace_name: "demo",
        caller: "local",
        agent: "codex",
        prompt: "  ",
        brief: None,
        client_request_id: None,
        approval: Approval::Auto,
        agent_approval: "all",
        model: None,
        effort: None,
        timeout_minutes: None,
    })
    .expect_err("空提示");
    assert_eq!(error.code, "INVALID_ARGUMENT");
}

/// 起跑前问一次版本，记进任务；不在档案 `verified` 里的标出来。
#[test]
fn the_agent_version_is_recorded_and_unverified_ones_are_flagged() {
    let f = Fixture::new();
    f.agent(
        "fake",
        "cat >/dev/null\necho ok",
        STDOUT,
        "verified = [\"fake-agent 2\"]",
    );
    let done = f.finished(&f.start("fake", Approval::Auto).expect("起"));
    assert_eq!(done.agent_version.as_deref(), Some("fake-agent 1.0"));
    assert!(done.version_unverified);
}

/// 认不得 `--version` 的程序会把它当普通参数照常干活：问版本那一次写出来的东西
/// 只能落在任务目录，不能落进项目，也不能落进 gld 的当前目录（2026-10-22 撞过）。
#[test]
fn asking_the_version_runs_inside_the_task_directory() {
    let f = Fixture::new();
    let path = f.home.path().join("naive.sh");
    std::fs::write(&path, "#!/bin/sh\necho stray > stray.txt\necho ok\n").expect("script");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    std::fs::write(
        agents_toml(f.data_home()),
        format!(
            "[agents.naive]\ncommand = [\"{}\"]\nresult = {STDOUT}\n",
            path.display()
        ),
    )
    .expect("agents.toml");
    let task = f.start("naive", Approval::Auto).expect("起");
    let done = f.finished(&task);
    let (dir, _) = get(f.data_home(), &task.task_id).expect("任务");
    assert!(
        dir.join("stray.txt").exists(),
        "问版本那一次应该写在任务目录"
    );
    assert!(
        !std::env::current_dir()
            .expect("cwd")
            .join("stray.txt")
            .exists(),
        "不能写进 gld 的当前目录"
    );
    assert_eq!(
        done.changed_files,
        vec!["stray.txt".to_string()],
        "真跑那一次在项目里"
    );
}

/// 两档放权：all 用 `command`，self 用 `self_command`，任务里记下是哪一档（用户要的"一键批准所有操作"
/// 和"由 Agent 自行判断、自行批准"）。
#[test]
fn each_approval_level_runs_its_own_command_and_is_recorded() {
    let f = Fixture::new();
    let script = f.agent(
        "fake",
        "cat >/dev/null\necho \"mode=$1\" > mode.txt\necho ok",
        STDOUT,
        "",
    );
    f.add_self_command(&script);
    let mode = || std::fs::read_to_string(f.root().join("mode.txt")).expect("mode.txt");

    let own = f.finished(
        &f.start_as("fake", Approval::Auto, None, "self")
            .expect("起"),
    );
    assert_eq!(own.state, State::AwaitingReview, "{own:?}");
    assert_eq!(own.agent_approval, "self");
    assert_eq!(mode(), "mode=self\n");

    let all = f.finished(&f.start_as("fake", Approval::Auto, None, "all").expect("起"));
    assert_eq!(all.agent_approval, "all");
    assert_eq!(mode(), "mode=\n");
}

/// 没有"自行判断"那一档的 Agent，项目设了 self 就起不来，报出哪几家有；绝不退回全放开。
/// ask 档等放行期间档案被改掉了这一档，放行时照样拒，Agent 一下都没跑。
#[test]
fn self_approval_never_falls_back_to_full_access() {
    let f = Fixture::new();
    f.agent(
        "plain",
        "cat >/dev/null\necho ran > ran.txt\necho ok",
        STDOUT,
        "",
    );
    let error = f
        .start_as("plain", Approval::Auto, None, "self")
        .expect_err("plain 没有 self_command");
    assert_eq!(error.code, "AGENT_APPROVAL_UNSUPPORTED");
    assert!(
        error.message.contains("codex") && error.message.contains("agent-approval=all"),
        "{error}"
    );
    assert!(list(f.data_home(), None).is_empty(), "拒了就不该留任务");

    let script = f.agent(
        "flip",
        "cat >/dev/null\necho ran > ran.txt\necho ok",
        STDOUT,
        "",
    );
    f.add_self_command(&script);
    let waiting = f
        .start_as("flip", Approval::Ask, None, "self")
        .expect("存下");
    // 用户在等放行的时候把 flip 的 self_command 删了。
    let toml = agents_toml(f.data_home());
    let text = std::fs::read_to_string(&toml).expect("agents.toml");
    let kept: String = text
        .lines()
        .filter(|line| !line.starts_with("self_command"))
        .map(|line| format!("{line}\n"))
        .collect();
    std::fs::write(&toml, kept).expect("agents.toml");
    let error = approve(f.data_home(), &waiting.task_id).expect_err("放行时再查一次");
    assert_eq!(error.code, "AGENT_APPROVAL_UNSUPPORTED");
    assert!(!f.root().join("ran.txt").exists(), "不能换成全放开的命令跑");
    assert!(running_in(&f.root()).is_none());
}

/// AI 提议的模型、推理强度按档案的参数模板传给 Agent，记进任务；没提就用档案默认；提了这家没有的值，
/// 起之前就拒、不留任务（用户问"model 和 effort 怎么设"，2026-10-24）。
#[test]
fn proposed_model_and_effort_reach_the_agent_and_are_recorded() {
    let f = Fixture::new();
    f.agent(
        "fake",
        "cat >/dev/null\necho \"$@\" > args.txt\necho ok",
        STDOUT,
        "model_args = [\"--model\", \"{model}\"]\neffort_args = [\"--effort\", \"{effort}\"]\nefforts = [\"low\", \"high\"]\nmodel = \"base-model\"",
    );
    let args = || std::fs::read_to_string(f.root().join("args.txt")).expect("args.txt");

    let done = f.finished(
        &f.start_choosing("fake", Some("big-model"), Some("high"))
            .expect("起"),
    );
    assert_eq!(
        (done.model.as_str(), done.effort.as_str()),
        ("big-model", "high")
    );
    assert_eq!(args(), "--model big-model --effort high\n");

    let default = f.finished(&f.start_choosing("fake", None, None).expect("起"));
    assert_eq!(
        (default.model.as_str(), default.effort.as_str()),
        ("base-model", "")
    );
    assert_eq!(
        args(),
        "--model base-model\n",
        "没提强度就不传，用它自己的配置"
    );

    let error = f
        .start_choosing("fake", None, Some("turbo"))
        .expect_err("不在 efforts 里");
    assert_eq!(error.code, "INVALID_ARGUMENT");
    assert!(error.message.contains("low、high"), "{error}");
    assert_eq!(list(f.data_home(), None).len(), 2, "拒了就不留任务");
}

/// ask 档放行时用户能改：换 Agent（模型、强度改用新那家的默认）、只改模型、改强度；改了什么记进任务。
/// 改得不对就报错，任务原样留着等放行，Agent 一下都没跑。
#[test]
fn approval_can_change_the_agent_model_and_effort() {
    let f = Fixture::new();
    let choices = |model: &str| {
        format!(
            "model_args = [\"--model\", \"{{model}}\"]\neffort_args = [\"--effort\", \"{{effort}}\"]\nefforts = [\"low\", \"high\"]\nmodel = \"{model}\""
        )
    };
    f.agent(
        "one",
        "cat >/dev/null\necho \"one $@\" > ran.txt\necho ok",
        STDOUT,
        &choices("one-default"),
    );
    f.agent(
        "two",
        "cat >/dev/null\necho \"two $@\" > ran.txt\necho ok",
        STDOUT,
        &choices("two-default"),
    );
    let ran = || std::fs::read_to_string(f.root().join("ran.txt")).expect("ran.txt");
    let ask = |model: Option<&str>, effort: Option<&str>| {
        start(StartRequest {
            data_home: f.data_home(),
            workspace_root: f.project.path(),
            workspace_name: "demo",
            caller: "local",
            agent: "one",
            prompt: "改一下",
            brief: None,
            client_request_id: None,
            approval: Approval::Ask,
            agent_approval: "all",
            model,
            effort,
            timeout_minutes: None,
        })
        .expect("存下")
    };

    // 换一家：AI 给 one 提议的模型和强度不沿用，改用 two 的默认。
    let waiting = ask(Some("m1"), Some("high"));
    let change = ApprovalChange {
        agent: Some("two"),
        ..ApprovalChange::default()
    };
    let done = f.finished(&approve_with(f.data_home(), &waiting.task_id, change).expect("放行"));
    assert_eq!(done.agent, "two");
    assert_eq!(
        (done.model.as_str(), done.effort.as_str()),
        ("two-default", "")
    );
    assert_eq!(
        done.approval_changes,
        vec![
            "agent one → two",
            "model m1 → two-default",
            "effort high → 默认"
        ]
    );
    assert_eq!(ran(), "two --model two-default\n");

    // 只改模型：强度照 AI 的提议。
    let waiting = ask(Some("m1"), Some("low"));
    let change = ApprovalChange {
        model: Some("m2"),
        ..ApprovalChange::default()
    };
    let done = f.finished(&approve_with(f.data_home(), &waiting.task_id, change).expect("放行"));
    assert_eq!(done.approval_changes, vec!["model m1 → m2"]);
    assert_eq!(ran(), "one --model m2 --effort low\n");

    // 改得不对：报错，任务还在等放行，没跑。
    std::fs::remove_file(f.root().join("ran.txt")).expect("清掉");
    let waiting = ask(None, None);
    for (change, code) in [
        (
            ApprovalChange {
                effort: Some("turbo"),
                ..ApprovalChange::default()
            },
            "INVALID_ARGUMENT",
        ),
        (
            ApprovalChange {
                agent: Some("nobody"),
                ..ApprovalChange::default()
            },
            "AGENT_UNKNOWN",
        ),
    ] {
        let error = approve_with(f.data_home(), &waiting.task_id, change).expect_err("该拒");
        assert_eq!(error.code, code, "{error}");
        let (_, still) = get(f.data_home(), &waiting.task_id).expect("任务");
        assert_eq!(still.state, State::WaitingLocalApproval);
        assert!(still.approval_changes.is_empty());
    }
    assert!(!f.root().join("ran.txt").exists());
    // 照原样放行：没改就不记。
    let done = f.finished(&approve(f.data_home(), &waiting.task_id).expect("放行"));
    assert!(done.approval_changes.is_empty());
    assert_eq!(ran(), "one --model one-default\n");
}

/// 起它的进程已经不在了（这里假装一个死掉的实例）：读到时改成 unknown，不猜结局。
#[test]
fn a_running_record_without_a_live_owner_reads_as_unknown() {
    let f = Fixture::new();
    f.agent("fake", "cat >/dev/null\necho ok", STDOUT, "");
    let waiting = f.start("fake", Approval::Ask).expect("存下");
    let (dir, mut task) = get(f.data_home(), &waiting.task_id).expect("任务");
    task.state = State::Running;
    task.owner = Some("0000000000000000000000000000dead".into());
    store::write(&dir, &task).expect("写");
    let (_, read) = get(f.data_home(), &waiting.task_id).expect("任务");
    assert_eq!(read.state, State::Unknown);
    assert_eq!(
        store::read(&dir).expect("盘上").state,
        State::Unknown,
        "要写回去"
    );
}

/// 回给审核的 diff 只有这次任务自己的改动：之前没提交的改动、没跟踪的文件不算进来（审查 §44：
/// 跟 HEAD 比时上一轮没提交的改动混在一起，ChatGPT 分不清）。用户的暂存区原样不动。
#[test]
fn the_diff_is_only_this_tasks_own_change_and_the_index_is_left_alone() {
    let f = Fixture::new();
    f.write("a.txt", "one\n");
    f.write("s.txt", "staged before\n");
    f.git(&["add", "-A"]);
    f.git(&["commit", "-q", "-m", "base"]);
    f.write("a.txt", "one\ntwo\n");
    f.write("b.txt", "earlier, untracked\n");
    f.write("s.txt", "staged before, changed\n");
    f.git(&["add", "s.txt"]);
    let staged_before = f.git(&["diff", "--cached"]);

    f.agent(
        "fake",
        "cat >/dev/null\necho three >> a.txt\necho new > c.txt\necho ok",
        STDOUT,
        "",
    );
    let task = f.start("fake", Approval::Auto).expect("起");
    let done = f.finished(&task);
    assert_eq!(done.state, State::AwaitingReview, "{done:?}");
    assert_eq!(
        done.changed_files,
        vec!["a.txt".to_string(), "c.txt".to_string()],
        "b.txt、s.txt 是跑之前就有的改动"
    );
    assert!(!done.head_moved);
    let detail = f.detail_of(&done);
    let diff = detail.diff.as_deref().expect("diff");
    assert!(diff.contains("+three") && diff.contains("+new"), "{diff}");
    assert!(
        !diff.contains("+two") && !diff.contains("b.txt") && !diff.contains("s.txt"),
        "之前的改动混进来了：{diff}"
    );
    assert!(!detail.diff_truncated);
    assert_eq!(done.diff_bytes, Some(diff.len() as u64));
    // 两棵树的 id 拿去 git diff，得到的就是回包里这份。
    let (before, after) = (
        done.before_tree.as_deref().expect("跑前的树"),
        done.after_tree.as_deref().expect("跑后的树"),
    );
    assert_ne!(before, after);
    assert_eq!(f.git(&["cat-file", "-t", before]), "tree\n");
    assert_eq!(f.git(&["diff", "--no-color", before, after]), diff);
    assert_eq!(
        f.git(&["diff", "--cached"]),
        staged_before,
        "用户的暂存区被动了"
    );
    let (dir, _) = get(f.data_home(), &task.task_id).expect("任务");
    let leftovers: Vec<_> = std::fs::read_dir(&dir)
        .expect("任务目录")
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".index") || name.ends_with(".lock"))
        .collect();
    assert!(leftovers.is_empty(), "临时 index 没收拾：{leftovers:?}");
}

/// 每个改了的文件带增删行数，和 changed_files 一一对上；二进制文件数不了行（审查 §47 ChatGPT 提的）。
#[test]
fn each_changed_file_comes_with_its_line_counts() {
    let f = Fixture::new();
    f.write("a.txt", "1\n2\n3\n");
    f.write("gone.txt", "x\ny\n");
    f.write("run.sh", "echo hi\n");
    f.git(&["add", "-A"]);
    f.git(&["commit", "-q", "-m", "base"]);
    f.agent(
        "fake",
        "cat >/dev/null\nprintf '1\\nTWO\\n3\\n' > a.txt\nrm gone.txt\nprintf '\\000\\001\\002' > blob.bin\necho new > new.txt\n: > empty.txt\nchmod +x run.sh\necho ok",
        STDOUT,
        "",
    );
    let done = f.finished(&f.start("fake", Approval::Auto).expect("起"));
    assert_eq!(done.state, State::AwaitingReview, "{done:?}");
    let counts: Vec<(&str, Option<u64>, Option<u64>)> = done
        .file_stats
        .iter()
        .map(|s| (s.path.as_str(), s.added, s.deleted))
        .collect();
    assert_eq!(
        counts,
        vec![
            ("a.txt", Some(1), Some(1)),
            ("blob.bin", None, None),
            ("empty.txt", Some(0), Some(0)),
            ("gone.txt", Some(0), Some(2)),
            ("new.txt", Some(1), Some(0)),
            ("run.sh", Some(0), Some(0)),
        ]
    );
    let paths: Vec<String> = done.file_stats.iter().map(|s| s.path.clone()).collect();
    assert_eq!(done.changed_files, paths, "两份列表要一一对上");

    // 每个文件在 task.diff 里那一段：首尾相接、盖满整份，每段从自己的 diff --git 起。
    // 二进制、空文件、只改权限也各有一段（file_sections 的注释这么说，这里核）。
    let (dir, _) = get(f.data_home(), &done.task_id).expect("任务");
    let whole = std::fs::read(dir.join(store::DIFF_FILE)).expect("task.diff");
    let mut at = 0;
    for stat in &done.file_stats {
        let (offset, bytes) = (
            stat.diff_offset.expect("位置") as usize,
            stat.diff_bytes.expect("大小") as usize,
        );
        assert_eq!(offset, at, "{} 没接上前一段", stat.path);
        let header = format!("diff --git a/{0} b/{0}\n", stat.path);
        assert!(
            whole[offset..offset + bytes].starts_with(header.as_bytes()),
            "{}：{}",
            stat.path,
            String::from_utf8_lossy(&whole[offset..offset + bytes])
        );
        at = offset + bytes;
    }
    assert_eq!(at, whole.len());
}

/// 大 diff 里只读一个文件：从它那一段的开头读到结尾，超过一页照样分页，不会读进下一个文件（审查 §50：
/// 代码排在 170 KB 的生成数据后面，ChatGPT 得先读完数据才看得到）。
#[test]
fn one_file_can_be_read_alone_out_of_a_big_diff() {
    let f = Fixture::new();
    f.agent(
        "flood",
        "cat >/dev/null\necho 'print(1)' > a.py\ni=0; while [ $i -lt 6000 ]; do echo \"line $i of a long generated file\"; i=$((i+1)); done > big.txt\necho 'def f():\n    return 2' > z.py\necho ok",
        STDOUT,
        "",
    );
    let done = f.finished(&f.start("flood", Approval::Auto).expect("起"));
    let (dir, task) = get(f.data_home(), &done.task_id).expect("任务");
    let whole = std::fs::read(dir.join(store::DIFF_FILE)).expect("task.diff");
    let section = |path: &str| {
        let stat = task
            .file_stats
            .iter()
            .find(|s| s.path == path)
            .expect("文件");
        let (offset, bytes) = (stat.diff_offset.unwrap(), stat.diff_bytes.unwrap());
        (
            offset,
            whole[offset as usize..(offset + bytes) as usize].to_vec(),
        )
    };

    // 排在最后的小文件：一页就完。
    let (offset, z) = section("z.py");
    let page = diff_page(&dir, &task, None, Some("z.py"), DIFF_MAX_BYTES).expect("z.py");
    assert_eq!(page.offset, offset);
    assert_eq!(page.next_offset, None);
    assert_eq!(page.path.as_deref(), Some("z.py"));
    assert_eq!(page.diff.as_bytes(), z.as_slice());

    // 夹在中间、比一页大的：一页页读到它的结尾，拼起来正好是它那一段，没有读进 z.py。
    let (offset, big) = section("big.txt");
    assert!(big.len() > DIFF_MAX_BYTES, "{}", big.len());
    let (mut read, mut next) = (Vec::new(), None);
    loop {
        let page = diff_page(&dir, &task, next, Some("big.txt"), DIFF_MAX_BYTES).expect("一页");
        assert!(page.diff.len() <= DIFF_MAX_BYTES);
        if next.is_none() {
            assert_eq!(page.offset, offset, "不给 offset 就从这个文件开头读");
        }
        read.extend_from_slice(page.diff.as_bytes());
        match page.next_offset {
            Some(offset) => next = Some(offset),
            None => break,
        }
    }
    assert_eq!(read, big);

    // 只想看大文件开头几行：页小到 1 KB，照样截在整行上，下一页紧接着这一页（审查 §54：看 CSV 格式只要 247 字节，
    // 以前最少得收 64 KB）。一页页读下去拼起来也还是它那一段。
    let peek = diff_page(&dir, &task, None, Some("big.txt"), 1024).expect("小页");
    assert!(
        peek.diff.len() <= 1024 && peek.diff.ends_with('\n'),
        "{}",
        peek.diff.len()
    );
    assert!(peek.diff.starts_with("diff --git a/big.txt b/big.txt\n"));
    assert_eq!(peek.next_offset, Some(offset + peek.diff.len() as u64));
    let (mut read, mut next) = (Vec::new(), None);
    loop {
        let page = diff_page(&dir, &task, next, Some("big.txt"), 1024).expect("小页");
        assert!(page.diff.len() <= 1024);
        read.extend_from_slice(page.diff.as_bytes());
        match page.next_offset {
            Some(offset) => next = Some(offset),
            None => break,
        }
    }
    assert_eq!(read, big);

    let error = |offset: Option<u64>, path: &str| {
        diff_page(&dir, &task, offset, Some(path), DIFF_MAX_BYTES)
            .expect_err("该报错")
            .code
    };
    assert_eq!(
        error(Some(0), "z.py"),
        "INVALID_ARGUMENT",
        "offset 不在这个文件里"
    );
    assert_eq!(error(None, "nobody.txt"), "INVALID_ARGUMENT", "没改的文件");

    // 早于记位置的任务：文件在列表里，但没有位置。
    let mut old = task.clone();
    for stat in &mut old.file_stats {
        stat.diff_offset = None;
        stat.diff_bytes = None;
    }
    assert_eq!(
        diff_page(&dir, &old, None, Some("z.py"), DIFF_MAX_BYTES)
            .expect_err("没记位置")
            .code,
        "AGENT_DIFF_UNAVAILABLE"
    );
}

/// `--numstat -z` 的路径原样带着，里面有制表符也不切开。
#[test]
fn numstat_records_keep_tabs_in_paths_and_mark_binaries() {
    let stats = run::parse_numstat(b"3\t1\ta\tb.txt\0-\t-\timg.png\0\0");
    assert_eq!(
        stats,
        vec![
            store::FileStat {
                path: "a\tb.txt".into(),
                added: Some(3),
                deleted: Some(1),
                diff_offset: None,
                diff_bytes: None,
            },
            store::FileStat {
                path: "img.png".into(),
                added: None,
                deleted: None,
                diff_offset: None,
                diff_bytes: None,
            },
        ]
    );
}

/// 不是 git 仓库：改动记成"不知道"，没有树、没有 diff，不瞎报。
#[test]
fn outside_git_the_changes_are_unknown_and_there_are_no_trees() {
    let f = Fixture::new();
    std::fs::remove_dir_all(f.root().join(".git")).expect("去掉 .git");
    f.agent(
        "fake",
        "cat >/dev/null\necho x > touched.txt\necho ok",
        STDOUT,
        "",
    );
    let task = f.start("fake", Approval::Auto).expect("起");
    let done = f.finished(&task);
    assert_eq!(done.state, State::AwaitingReview, "{done:?}");
    assert!(done.changes_unknown && done.changed_files.is_empty());
    assert_eq!((&done.before_tree, &done.after_tree), (&None, &None));
    assert_eq!(done.diff_bytes, None);
    let (dir, _) = get(f.data_home(), &task.task_id).expect("任务");
    let error = diff_page(&dir, &done, Some(0), None, DIFF_MAX_BYTES).expect_err("没有 diff");
    assert_eq!(error.code, "AGENT_DIFF_UNAVAILABLE");
    assert!(error.message.contains("不是 git 仓库"), "{error}");
}

/// Agent 被叮嘱别提交，还是提交了：标出来，改动照样在 diff 里（比的是工作区，不是 HEAD）。
#[test]
fn a_commit_by_the_agent_is_flagged_and_still_in_the_diff() {
    let f = Fixture::new();
    f.write("a.txt", "one\n");
    f.git(&["add", "-A"]);
    f.git(&["commit", "-q", "-m", "base"]);
    f.agent(
        "committer",
        "cat >/dev/null\necho d > d.txt\ngit add d.txt\ngit -c user.name=a -c user.email=a@example.com commit -qm sneaky\necho ok",
        STDOUT,
        "",
    );
    let done = f.finished(&f.start("committer", Approval::Auto).expect("起"));
    assert!(done.head_moved, "{done:?}");
    assert_eq!(done.changed_files, vec!["d.txt".to_string()]);
    assert!(f.detail_of(&done).diff.unwrap_or_default().contains("+d"));
}

/// diff 超过 64 KB 只回前面一段，截在整行上，标出来。
#[test]
fn a_huge_diff_is_cut_on_a_line_boundary() {
    let f = Fixture::new();
    f.agent(
        "flood",
        "cat >/dev/null\ni=0; while [ $i -lt 3000 ]; do echo \"line $i of a long generated file\"; i=$((i+1)); done > big.txt\necho ok",
        STDOUT,
        "",
    );
    let done = f.finished(&f.start("flood", Approval::Auto).expect("起"));
    let detail = f.detail_of(&done);
    let diff = detail.diff.expect("diff");
    assert!(detail.diff_truncated);
    assert!(
        diff.len() <= DIFF_MAX_BYTES && diff.ends_with('\n'),
        "{}",
        diff.len()
    );
    assert!(done.diff_bytes.unwrap_or(0) > DIFF_MAX_BYTES as u64);
    assert_eq!(
        detail.diff_next_offset,
        Some(diff.len() as u64),
        "没有要脱敏的内容时，下一页正好接在这一页后面"
    );

    // 剩下的一页页读（审查 §47：AI 读不到数据目录里的 task.diff），拼起来就是整份。
    let (dir, task) = get(f.data_home(), &done.task_id).expect("任务");
    let (mut whole, mut offset, mut pages) = (String::new(), 0, 0);
    loop {
        let page = diff_page(&dir, &task, Some(offset), None, DIFF_MAX_BYTES).expect("一页");
        assert!(page.diff.len() <= DIFF_MAX_BYTES, "{}", page.diff.len());
        assert_eq!(Some(page.total_bytes), done.diff_bytes);
        if pages == 0 {
            assert_eq!(page.diff, diff, "第一页就是回包里那份");
        }
        whole.push_str(&page.diff);
        pages += 1;
        match page.next_offset {
            Some(next) => {
                assert!(page.diff.ends_with('\n'), "要截在整行上");
                offset = next;
            }
            None => break,
        }
    }
    assert!(pages >= 2, "{pages}");
    assert_eq!(
        whole.into_bytes(),
        std::fs::read(dir.join(store::DIFF_FILE)).expect("task.diff")
    );

    let total = done.diff_bytes.expect("diff_bytes");
    let end = diff_page(&dir, &task, Some(total), None, DIFF_MAX_BYTES).expect("正好在末尾");
    assert!(end.diff.is_empty() && end.next_offset.is_none());
    assert_eq!(
        diff_page(&dir, &task, Some(total + 1), None, DIFF_MAX_BYTES)
            .expect_err("超过总长")
            .code,
        "INVALID_ARGUMENT"
    );
}

/// 一行比一页还长（压缩过的文件）就在行中间切，切在字符边界上；一页页接起来不丢不重。
#[test]
fn pages_cut_inside_an_overlong_line_on_a_char_boundary() {
    let dir = tempfile::tempdir().expect("dir");
    let path = dir.path().join("task.diff");
    for (text, max) in [("aéééb", 4), ("ab\ncdefgh\n", 5), ("", 4)] {
        std::fs::write(&path, text).expect("write");
        let (mut whole, mut offset) = (String::new(), 0);
        loop {
            let (page, next) = read_diff(&path, offset, u64::MAX, max).expect("读");
            assert!(page.len() <= max && !page.contains('\u{FFFD}'), "{page:?}");
            whole.push_str(&page);
            match next {
                Some(next) => {
                    assert!(next > offset, "要往前走");
                    offset = next;
                }
                None => break,
            }
        }
        assert_eq!(whole, text);
    }
}

/// 还没跑完的任务没有 diff 可读。
#[test]
fn the_diff_cannot_be_paged_before_the_task_has_run() {
    let f = Fixture::new();
    f.agent("fake", "cat >/dev/null\necho ok", STDOUT, "");
    let waiting = f.start("fake", Approval::Ask).expect("存下");
    let (dir, task) = get(f.data_home(), &waiting.task_id).expect("任务");
    assert_eq!(
        diff_page(&dir, &task, Some(0), None, DIFF_MAX_BYTES)
            .expect_err("还没跑")
            .code,
        "AGENT_TASK_STATE"
    );
}

/// 在跑时每次 wait 回的输出尾巴要小（审查 §44：Codex 的事件 JSON 让一次回包到了 14 KB）。
#[test]
fn a_running_task_reports_only_a_short_output_tail() {
    let f = Fixture::new();
    f.agent(
        "chatty",
        "cat >/dev/null\nwhile true; do echo '{\"type\":\"item.completed\",\"item\":{\"text\":\"a fairly long event line from the agent\"}}'; sleep 0.05; done",
        STDOUT,
        "",
    );
    let task = f.start("chatty", Approval::Auto).expect("起");
    std::thread::sleep(Duration::from_millis(800));
    let detail = f.detail_of(&task);
    assert_eq!(detail.task.state, State::Running);
    let tail = detail.output_tail.expect("在跑时有输出尾巴");
    assert!(
        !tail.stdout.is_empty() && tail.stdout.len() <= RUNNING_TAIL_BYTES,
        "{}",
        tail.stdout.len()
    );
    stop(f.data_home(), &task.task_id).expect("停");
    let stopped = f.finished(&task);
    let tail = f.detail_of(&stopped).output_tail.expect("停了也有");
    assert!(
        tail.stdout.len() > RUNNING_TAIL_BYTES,
        "收尾后给长一点：{}",
        tail.stdout.len()
    );
}
