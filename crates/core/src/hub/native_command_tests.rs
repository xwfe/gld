use super::*;

use crate::auth::Principal;
use crate::nodes::identity::{auth_from_meta, forwarded_meta};

const HUB_ID: &str = "11111111111111111111111111111111";

#[test]
fn node_commands_stay_in_the_allowed_project_and_with_the_caller_who_started_them() {
    const CHILD: &str = "GLD_TEST_NODE_COMMANDS_CHILD";
    if std::env::var_os(CHILD).is_some() {
        exercise_commands();
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "hub::native::command_tests::node_commands_stay_in_the_allowed_project_and_with_the_caller_who_started_them",
            "--nocapture",
        ])
        .env(CHILD, "1")
        .env("GLD_HOME", home.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed; 0 failed"));
}

fn exercise_commands() {
    let root = tempfile::tempdir().unwrap();
    let project = |name: &str| {
        let path = root.path().join(name);
        std::fs::create_dir_all(&path).unwrap();
        let mut profile =
            WorkspaceProfile::new(path.to_string_lossy().into_owned(), Some(name.into()));
        profile.runtime.tool_profile = "advanced".into();
        // 这里验权限和主体隔离，不验白名单；白名单在真实链路测试里验。
        profile.runtime.command_policy = "all".into();
        profile
    };
    let (api, web, docs) = (project("api"), project("web"), project("docs"));
    DataStore::update_file(|data| {
        data.profiles = vec![api.clone(), web.clone(), docs.clone()];
        Ok(())
    })
    .unwrap();
    let access = NodeAccess {
        shared: vec![api.id.clone(), web.id.clone(), docs.id.clone()],
        writable: vec![api.id.clone(), web.id.clone()],
        runnable: vec![api.id.clone()],
    };
    let hub = Hub::for_node(access.clone()).unwrap();
    let call_as = |who: &str, name: &str, args: Value| {
        let origin = AuthContext::new(
            Principal::OAuthClient {
                client_id: who.into(),
            },
            "hub",
        );
        let params =
            json!({"name":name,"arguments":args,"_meta":forwarded_meta(&origin,HUB_ID).unwrap()});
        let auth = auth_from_meta(&params, HUB_ID, &access.shared, &access.writable).unwrap();
        hub.handle_request(
            &auth,
            &json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":params}),
        )
        .0
    };
    let structured = |reply: Value| reply["result"]["structuredContent"].clone();

    // 只写的项目和只读的项目都不能跑，连名字带前缀也绕不过去。
    let refused = structured(call_as(
        "alice",
        "exec_command",
        json!({"workspace":web.id,"cmd":"pwd"}),
    ));
    assert_eq!(refused["error"]["code"], "NODE_COMMANDS_NOT_ALLOWED");
    let reader = call_as(
        "alice",
        "exec_command",
        json!({"workspace":docs.id,"cmd":"pwd"}),
    );
    assert_eq!(
        reader["error"]["data"]["reason"], "unknown_tool",
        "{reader}"
    );
    for selector in ["api", &api.id[..8]] {
        let by_name = call_as(
            "alice",
            "exec_command",
            json!({"workspace":selector,"cmd":"pwd"}),
        );
        assert_eq!(
            by_name["error"]["data"]["reason"], "unknown_tool",
            "only the exact ID the Hub sends carries the permission: {by_name}"
        );
    }

    // 项目自己看得到的工具表跟实际一致。
    let tools_of = |id: &str| {
        let context = structured(call_as(
            "alice",
            "workspace_context",
            json!({"workspace":id}),
        ));
        assert_eq!(context["ok"], true, "{context}");
        context
    };
    let context = tools_of(&api.id);
    assert_eq!(context["tool_profile"], "commands");
    assert_eq!(context["commands"], true);
    assert!(context["tools"]
        .as_array()
        .unwrap()
        .contains(&json!("exec_command")));
    for id in [&web.id, &docs.id] {
        let context = tools_of(id);
        assert_eq!(context["commands"], false);
        assert!(!context["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool == "exec_command" || tool == "kill_session"));
    }

    // alice 的命令，bob 读不到、停不了、列不出，连“存在”都不知道。
    let started = structured(call_as(
        "alice",
        "exec_command",
        json!({"workspace":api.id,"cmd":"sleep 60","yield_time_ms":0,"timeout_ms":60_000}),
    ));
    assert_eq!(started["ok"], true, "{started}");
    let session = started["session_id"].as_str().unwrap().to_string();
    for (tool, args) in [
        (
            "read_output",
            json!({"workspace":api.id,"session_id":session}),
        ),
        (
            "write_stdin",
            json!({"workspace":api.id,"session_id":session,"chars":"x"}),
        ),
        (
            "kill_session",
            json!({"workspace":api.id,"session_id":session}),
        ),
    ] {
        let stolen = structured(call_as("bob", tool, args));
        assert_eq!(
            stolen["error"]["code"], "SESSION_NOT_FOUND",
            "{tool}: {stolen}"
        );
    }
    let bobs = structured(call_as("bob", "list_runs", json!({"workspace":api.id})));
    assert_eq!(bobs["runs"], json!([]), "{bobs}");

    // 新连接带同一个主体来（节点重连后就是这样），还能找回自己的命令并停掉。
    let alices = structured(call_as("alice", "list_runs", json!({"workspace":api.id})));
    assert_eq!(alices["runs"][0]["session_id"], session, "{alices}");
    let killed = structured(call_as(
        "alice",
        "kill_session",
        json!({"workspace":api.id,"session_id":session}),
    ));
    assert_eq!(killed["ok"], true, "{killed}");

    // 本机把工具档收紧后立刻生效，不用等节点重启。
    DataStore::update_file(|data| {
        data.profiles[0].runtime.tool_profile = "read-only".into();
        Ok(())
    })
    .unwrap();
    let narrowed = structured(call_as(
        "alice",
        "exec_command",
        json!({"workspace":api.id,"cmd":"pwd"}),
    ));
    assert_eq!(narrowed["error"]["code"], "NODE_COMMANDS_NOT_ALLOWED");
    DataStore::update_file(|data| {
        data.profiles[0].runtime.tool_profile = "advanced".into();
        Ok(())
    })
    .unwrap();

    // 项目从本机移除后调用方够不着它了，它的命令要跟着停，否则一直跑到节点退出。
    let pid_file = root.path().join("api/pid.txt");
    let started = structured(call_as(
        "alice",
        "exec_command",
        json!({"workspace":api.id,"argv":["sh","-c","echo $$ > pid.txt; exec sleep 60"],
               "yield_time_ms":0,"timeout_ms":60_000}),
    ));
    assert_eq!(started["ok"], true, "{started}");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let pid = loop {
        let text = std::fs::read_to_string(&pid_file).unwrap_or_default();
        if let Ok(pid) = text.trim().parse::<libc::pid_t>() {
            break pid;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the command did not start"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    assert_eq!(unsafe { libc::kill(pid, 0) }, 0);
    DataStore::update_file(|data| {
        data.profiles.retain(|profile| profile.id != api.id);
        Ok(())
    })
    .unwrap();
    // 下一次调用发现项目走了；进程组要一点时间才消失。
    call_as("alice", "list_workspaces", json!({}));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while unsafe { libc::kill(pid, 0) } == 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "the command outlived its project"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

#[test]
fn a_write_only_project_cannot_make_git_tools_run_its_own_commands() {
    const CHILD: &str = "GLD_TEST_NODE_GIT_CONFIG_CHILD";
    if std::env::var_os(CHILD).is_some() {
        exercise_git_config();
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "hub::native::command_tests::a_write_only_project_cannot_make_git_tools_run_its_own_commands",
            "--nocapture",
        ])
        .env(CHILD, "1")
        .env("GLD_HOME", home.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed; 0 failed"));
}

fn exercise_git_config() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("api");
    std::fs::create_dir_all(&path).unwrap();
    let mut api = WorkspaceProfile::new(path.to_string_lossy().into_owned(), Some("api".into()));
    api.runtime.tool_profile = "advanced".into();
    DataStore::update_file(|data| {
        data.profiles = vec![api.clone()];
        Ok(())
    })
    .unwrap();
    let access = NodeAccess {
        shared: vec![api.id.clone()],
        writable: vec![api.id.clone()],
        runnable: Vec::new(),
    };
    let hub = Hub::for_node(access.clone()).unwrap();
    let call = |name: &str, args: Value| {
        let origin = AuthContext::new(
            Principal::OAuthClient {
                client_id: "alice".into(),
            },
            "hub",
        );
        let params =
            json!({"name":name,"arguments":args,"_meta":forwarded_meta(&origin,HUB_ID).unwrap()});
        let auth = auth_from_meta(&params, HUB_ID, &access.shared, &access.writable).unwrap();
        hub.handle_request(
            &auth,
            &json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":params}),
        )
        .0["result"]["structuredContent"]
            .clone()
    };
    let marker = path.join("marker");
    let config = format!(
        "[core]\n\trepositoryformatversion = 0\n\tbare = false\n\tworktree = .\n\tfsmonitor = touch {}; false #\n",
        marker.display()
    );
    let add = |dir: &str| {
        let mut patch = String::from("*** Begin Patch\n");
        for (file, text) in [
            ("HEAD", "ref: refs/heads/main"),
            ("objects/x/keep", "x"),
            ("refs/x/keep", "x"),
            ("commondir", "."),
        ] {
            patch.push_str(&format!("*** Add File: {dir}/{file}\n+{text}\n"));
        }
        patch.push_str(&format!("*** Add File: {dir}/config\n"));
        for line in config.lines() {
            patch.push_str(&format!("+{line}\n"));
        }
        patch.push_str("*** End Patch");
        call("apply_patch", json!({"workspace":api.id,"patch":patch}))
    };

    // 任意一层的 .git 都不让写，大小写和 Windows 的变体也一样。
    for dir in ["sub/.git", "sub/.GIT", "a/b/.git", "sub/.git.", "sub/git~1"] {
        let refused = add(dir);
        assert_eq!(refused["ok"], false, "{dir}: {refused}");
        assert_eq!(
            refused["error"]["code"], "PROTECTED_REPOSITORY_ASSET",
            "{dir}: {refused}"
        );
    }
    assert!(!path.join("sub").join(".git").exists());

    // 不含 .git 的裸仓库形状写得进去（就是普通文件），但 git 工具不能在里面跑。
    assert_eq!(add("bare")["ok"], true);
    let status = call("git_status", json!({"workspace":api.id,"path":"bare"}));
    assert!(!marker.exists(), "git ran the project's fsmonitor command");
    assert_eq!(status["ok"], false, "{status}");
    assert_eq!(
        status["error"]["code"], "GIT_BARE_REPOSITORY_REFUSED",
        "{status}"
    );

    // 只有 HEAD 和 commondir，对象库在别处，git 同样认它是仓库。
    assert_eq!(add("elsewhere")["ok"], true);
    std::fs::remove_dir_all(path.join("elsewhere/objects")).unwrap();
    std::fs::remove_dir_all(path.join("elsewhere/refs")).unwrap();
    std::fs::write(path.join("elsewhere/commondir"), "../bare\n").unwrap();
    let status = call("git_status", json!({"workspace":api.id,"path":"elsewhere"}));
    assert_eq!(
        status["error"]["code"], "GIT_BARE_REPOSITORY_REFUSED",
        "{status}"
    );
    assert!(!marker.exists());

    // 普通目录、真实仓库不受影响。
    assert_eq!(
        call("git_status", json!({"workspace":api.id}))["ok"],
        true,
        "a project without a repository still answers"
    );
}
