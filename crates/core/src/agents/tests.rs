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
            timeout_minutes: None,
        })
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
