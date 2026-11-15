//! 验证 App 的持久化变更与宿主提供的清理操作之间的顺序。

use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use gld_core::app::{App, GrantSpec, WorkspaceCreateOptions};
use gld_core::planning::{PlanningMode, PlanningService};

#[test]
fn planning_cleanup_failure_leaves_the_mode_saved() {
    in_isolated_home("planning_cleanup_failure_leaves_the_mode_saved", || {
        let workspace = tempfile::tempdir().expect("workspace");
        let root = workspace.path().canonicalize().expect("workspace root");
        let app = App::load().expect("app");
        let profile = app
            .create_workspace(&root, WorkspaceCreateOptions::default())
            .expect("project");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime");

        let error = runtime
            .block_on(app.set_planning_mode_with_cleanup(
                &profile.id,
                PlanningMode::Plan,
                move |path| {
                    assert_eq!(path, root.as_path());
                    assert_eq!(
                        PlanningService::new(path).state().expect("saved mode").mode,
                        PlanningMode::Plan
                    );
                    Err("执行代暂时不可用".into())
                },
            ))
            .expect_err("cleanup failure must be reported");

        assert_eq!(
            error.to_string(),
            "已切到 plan 模式，但停止工作区里还在跑的命令时出错：执行代暂时不可用"
        );
        assert_eq!(
            app.planning_state(&profile.id).expect("saved mode").mode,
            PlanningMode::Plan
        );
    });
}

#[test]
fn grant_cleanup_runs_twice_after_the_revocation_is_saved() {
    in_isolated_home(
        "grant_cleanup_runs_twice_after_the_revocation_is_saved",
        || {
            let workspace = tempfile::tempdir().expect("workspace");
            let home = gld_core::home::data_home().expect("isolated home");
            let app = App::load().expect("app");
            let profile = app
                .create_workspace(workspace.path(), WorkspaceCreateOptions::default())
                .expect("project");
            let grant = app
                .add_grant(GrantSpec {
                    name: "test grant".into(),
                    workspaces: vec![profile.id],
                    writable: false,
                })
                .expect("grant");
            let id = grant.id.clone();
            let invocations = AtomicUsize::new(0);
            let (send, receive) = mpsc::channel();

            let removed = app
                .remove_grant_with_cleanup(&grant.id, move |grant_id, data_home| {
                    assert_eq!(grant_id, id);
                    assert_eq!(data_home, Some(home.as_path()));
                    let persisted = App::load()
                        .expect("fresh app")
                        .list_grants()
                        .expect("grants");
                    assert!(persisted.iter().all(|grant| grant.id != grant_id));
                    let invocation = invocations.fetch_add(1, Ordering::Relaxed) + 1;
                    send.send((invocation, Instant::now()))
                        .expect("record cleanup");
                    (invocation + 2, invocation + 3)
                })
                .expect("revocation");

            assert_eq!(removed.grant.id, grant.id);
            assert_eq!(
                (removed.stopped_commands, removed.stopped_agent_tasks),
                (3, 4)
            );
            let (first, first_at) = receive.try_recv().expect("cleanup before returning");
            let (second, second_at) = receive
                .recv_timeout(Duration::from_secs(55))
                .expect("the same cleanup runs again");
            assert_eq!((first, second), (1, 2));
            assert!(second_at.duration_since(first_at) >= Duration::from_secs(35));
        },
    );
}

fn in_isolated_home(test: &str, run: impl FnOnce()) {
    const CHILD: &str = "GLD_TEST_APP_CLEANUP_CHILD";
    if std::env::var(CHILD).as_deref() == Ok(test) {
        run();
        return;
    }

    // 路径按进程缓存：给子进程独立的目录，不改并行测试进程的环境。
    let home = tempfile::tempdir().expect("isolated GLD_HOME");
    let home_path = home.path().canonicalize().expect("absolute GLD_HOME");
    let output = Command::new(std::env::current_exe().expect("test executable"))
        .args(["--exact", test, "--nocapture"])
        .env("GLD_HOME", home_path)
        .env(CHILD, test)
        .stdin(Stdio::null())
        .output()
        .expect("start isolated test");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains(test),
        "isolated test failed: {}\nstdout:\n{stdout}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );
}
