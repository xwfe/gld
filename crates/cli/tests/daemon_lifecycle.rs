//! 真实进程级集成测试：用编译出来的 `gld` 二进制走一遍
//! 加项目 → 自动拉起守护进程 → 启动服务 → 通过 TCP 请求 → 停止 → 退出守护进程。
//!
//! 每个测试用独立的 `GLD_HOME`，不会碰真实的 `~/.config/gld`。

mod common;

use std::net::TcpStream;
use std::path::Path;
use std::process::Command;

use common::env::{free_port, Env};
use common::http::get;

#[test]
fn full_lifecycle_through_the_real_binary() {
    let env = Env::new();
    let port = free_port();
    let port_arg = port.to_string();

    // 守护进程还没起来：状态命令用退出码 3 表示。
    let status = env.gld(&["daemon", "status"]);
    assert_eq!(status.status.code(), Some(3));

    // 直连模式下加项目、改配置，不需要守护进程。
    let created = env.json(&["--json", "add", ".", "--name", "it"]);
    assert_eq!(created[0]["name"], "it");
    env.ok(&["upgrade", "--port", &port_arg, "--auth", "noauth"]);
    assert!(!Path::new(&env.home.path().join("daemon.json")).exists());

    // start 会自动拉起守护进程。
    let started = env.json(&["--json", "start"]);
    assert_eq!(started["state"], "running");
    assert_eq!(
        started["localEndpoint"],
        format!("http://127.0.0.1:{port}/mcp")
    );
    let info = env.json(&["--json", "daemon", "status"]);
    assert_eq!(info["running"], true);
    assert_eq!(info["version_matches_cli"], true);

    let status = env.json(&["--json", "status"]);
    assert_eq!(status["service"]["state"], "running");
    assert_eq!(status["service"]["members"][0]["name"], "it");

    // 服务真的在这个端口上提供 MCP。
    let response = get(port, "/mcp");
    assert_eq!(response.status, 200);
    assert!(
        response.body.contains("protocolVersion"),
        "{}",
        response.body
    );

    // 守护进程在跑时，配置类命令也走守护进程。
    let shown = env.json(&["--json", "ls", "it"]);
    assert_eq!(shown["project"]["name"], "it");
    assert_eq!(shown["inService"], true);
    let logs = env.json(&["--json", "logs", "-n", "5"]);
    assert!(logs.as_array().map(|a| !a.is_empty()).unwrap_or(false));

    env.ok(&["stop"]);
    let status = env.json(&["--json", "status"]);
    assert_eq!(status["service"]["state"], "stopped");
    assert!(
        TcpStream::connect(("127.0.0.1", port)).is_err(),
        "port should be released"
    );

    env.ok(&["daemon", "stop"]);
    let status = env.gld(&["daemon", "status"]);
    assert_eq!(status.status.code(), Some(3));
    assert!(!env.home.path().join("daemon.json").exists());
}

/// pid 被系统回收再发给别人时，别把那个无辜进程当成守护进程去报告、去杀。
///
/// 复现的是真事：守护进程异常退出后 `daemon.json` 留在盘上，过一阵这个号被分给
/// 别的程序，`daemon status` 就说"守护进程存在但不响应"，`daemon stop --force`
/// 直接把它连同子进程一起 SIGTERM + SIGKILL。集成测试一轮起几百个进程，
/// 号绕回来只是时间问题。这里用一个睡着的进程当受害者，它必须活到测试结束。
///
/// Windows 上认人是另一套实现（`QueryFullProcessImageNameW` 取映像路径），认错了
/// `stop --force` 会用 `TerminateProcess` 杀整棵树，所以这条也在 Windows 那一小组里。
#[test]
fn a_recycled_pid_is_not_mistaken_for_the_daemon() {
    // 记录里带 exe（0.6.0 起）时按路径认，不带（更早的记录）时退回按文件名认。
    // 两种记录都要挡住，升级上来的用户才不会在换版本的那几天里踩到。
    for exe in [Some(env!("CARGO_BIN_EXE_gld")), None] {
        let env = Env::new();
        // Windows 没有 sleep；ping 本机 31 次约 30 秒，是系统自带、不用 shell 的睡法。
        let mut victim = if cfg!(windows) {
            let mut ping = Command::new("ping");
            ping.args(["-n", "31", "127.0.0.1"])
                .stdout(std::process::Stdio::null());
            ping
        } else {
            let mut sleep = Command::new("sleep");
            sleep.arg("30");
            sleep
        }
        .spawn()
        .expect("spawn 一个无辜进程");
        let record = env.home.path().join("daemon.json");
        std::fs::create_dir_all(env.home.path()).unwrap();
        let mut fields = serde_json::json!({
            "pid": victim.id(),
            "version": "0.6.0",
            "protocol": 3,
            "started_at_unix": 1,
            "socket": env.home.path().join("daemon.sock"),
            "log": env.home.path().join("logs/daemon.log"),
        });
        if let Some(exe) = exe {
            fields["exe"] = serde_json::json!(exe);
        }
        std::fs::write(&record, fields.to_string()).unwrap();

        // 认不出来就当记录已经失效：报"没在跑"（退出码 3），而不是"存在但不响应"。
        let status = env.gld(&["daemon", "status"]);
        assert_eq!(status.status.code(), Some(3), "exe={exe:?}");

        let stop = env.gld(&["daemon", "stop", "--force", "--wait", "1"]);
        assert!(stop.status.success(), "exe={exe:?} {:?}", stop.status);
        assert!(
            !record.exists(),
            "exe={exe:?}：失效的记录应当被清掉，否则下一条命令还会撞上同一个 pid"
        );

        let survived = victim.try_wait().expect("查无辜进程").is_none();
        let _ = victim.kill();
        let _ = victim.wait();
        assert!(survived, "exe={exe:?}：只因为 pid 对上了就把无关进程杀了");
    }
}

/// 同一个数据目录只能有一个守护进程：已有一个在跑时，再直接 `gld daemon run` 要立刻失败，
/// 而且原来那个照常响应。
///
/// 挡人的是 `daemon.lock` 上的排他锁。Unix 是 flock（建议锁），Windows 是 LockFileEx
/// （强制锁、按句柄），语义不同，编得过不代表拦得住。万一两道保护（锁、命名管道的
/// first_pipe_instance）都没挡住，第二个实例会一直前台跑下去，所以这里限时等、超时就杀。
#[test]
fn a_second_daemon_on_the_same_home_is_refused() {
    let env = Env::new();
    let first = env.json(&["--json", "daemon", "start"]);
    let pid = first["pid"].as_u64().expect("daemon pid");

    let mut second = Command::new(env!("CARGO_BIN_EXE_gld"))
        .args(["daemon", "run"])
        .env("GLD_HOME", env.home.path())
        .env("NO_COLOR", "1")
        .env_remove("GLD_WORKSPACE")
        .current_dir(env.project.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn 第二个守护进程");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    let status = loop {
        if let Some(status) = second.try_wait().expect("查第二个守护进程") {
            break Some(status);
        }
        if std::time::Instant::now() >= deadline {
            let _ = second.kill();
            let _ = second.wait();
            break None;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    };
    let output = second.wait_with_output().expect("收第二个守护进程的输出");
    let stderr = String::from_utf8_lossy(&output.stderr);
    let status = status.unwrap_or_else(|| panic!("第二个守护进程 15 秒还没退出：{stderr}"));
    assert!(!status.success(), "第二个守护进程居然成功退出了：{stderr}");
    assert!(
        stderr.contains("daemon.lock"),
        "应当是被单实例锁挡下的：{stderr}"
    );

    let info = env.json(&["--json", "daemon", "status"]);
    assert_eq!(info["running"], true, "{info}");
    assert_eq!(
        info["info"]["pid"].as_u64(),
        Some(pid),
        "原来那个守护进程被换掉了：{info}"
    );
}

#[test]
fn no_autostart_reports_exit_code_3() {
    let env = Env::new();
    env.ok(&["add", ".", "--name", "quiet"]);
    let output = env.gld(&["--no-autostart", "start"]);
    assert_eq!(output.status.code(), Some(3));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("gld daemon start"), "{stderr}");
}

#[test]
fn project_resolution_by_name_prefix_and_cwd() {
    let env = Env::new();
    // 下面要在子目录里执行命令，先把它建出来。
    env.write("src/main.js", "console.log('ok')\n");
    let created = env.json(&["--json", "add", ".", "--name", "alpha"]);
    let id = created[0]["id"].as_str().unwrap().to_string();
    let by_prefix = env.json(&["--json", "ls", &id[..6]]);
    assert_eq!(by_prefix["project"]["id"], id);
    let by_name = env.json(&["--json", "-w", "ALPHA", "ls"]);
    assert_eq!(by_name["project"]["id"], id);
    // 在项目子目录里执行，不指定项目也能找到（ws show 是按当前目录看一个项目的老写法）。
    let sub = env.project.path().join("src");
    let output = Command::new(env!("CARGO_BIN_EXE_gld"))
        .args(["--json", "ws", "show"])
        .env("GLD_HOME", env.home.path())
        .env_remove("GLD_WORKSPACE")
        .current_dir(&sub)
        .output()
        .unwrap();
    assert!(output.status.success());
    let shown: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(shown["project"]["id"], id);

    let missing = env.gld(&["ls", "does-not-exist"]);
    assert_eq!(missing.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&missing.stderr).contains("未找到项目"));
}

/// 一条会睡 `ms` 毫秒的命令，以及按命令行认出它、收掉它的办法。`ms` 每条测试取不同的值，
/// 当记号用。
///
/// 为什么要这一层：Windows 没有 `sleep`，也没有 `pgrep` / `pkill`。那边用 PowerShell 的
/// `Start-Sleep` 睡（`powershell` 本来就在默认命令白名单里），按 `Win32_Process` 的命令行
/// 找。查询用的那个 PowerShell 自己的命令行里也带着这个记号，所以要排除 `$PID`，不排除
/// 的话它永远"还在跑"。
struct Sleeper {
    ms: u32,
}

impl Sleeper {
    /// 写进 `exec_command` 的 cmd 或脚本里的那一行。
    fn command(&self) -> String {
        if cfg!(windows) {
            format!(
                "powershell -NoProfile -Command Start-Sleep -Milliseconds {}",
                self.ms
            )
        } else {
            format!("sleep {}.{:03}", self.ms / 1000, self.ms % 1000)
        }
    }

    /// 要加进 `allowed-commands` 的程序名。
    fn program(&self) -> &'static str {
        if cfg!(windows) {
            "powershell"
        } else {
            "sleep"
        }
    }

    fn is_running(&self) -> bool {
        if !cfg!(windows) {
            return Command::new("pgrep")
                .args(["-f", &self.command()])
                .output()
                .map(|output| output.status.success())
                .unwrap_or(false);
        }
        // 查不了就 panic，别当成"没在跑"：不然"命令行退出后后台命令还在跑"这类断言
        // 会在查询坏掉时白白通过。
        let output = Command::new("powershell")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                &format!("@({}).Count", self.windows_query()),
            ])
            .output()
            .expect("run powershell");
        let stdout = String::from_utf8_lossy(&output.stdout);
        stdout.trim().parse::<u32>().unwrap_or_else(|_| {
            panic!(
                "按命令行查进程失败：stdout={stdout} stderr={}",
                String::from_utf8_lossy(&output.stderr)
            )
        }) > 0
    }

    /// 测试失败时别把它留下。
    fn kill(&self) {
        if !cfg!(windows) {
            let _ = Command::new("pkill").args(["-f", &self.command()]).status();
            return;
        }
        let script = format!(
            "{} | ForEach-Object {{ Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }}",
            self.windows_query()
        );
        let _ = Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .status();
    }

    fn windows_query(&self) -> String {
        format!(
            "Get-CimInstance Win32_Process | Where-Object {{ $_.ProcessId -ne $PID -and $_.CommandLine -like '*Start-Sleep*{}*' }}",
            self.ms
        )
    }
}

/// 在项目里放一个脚本，返回 `exec_command` 里调它的写法。
///
/// Unix 是 `#!/bin/sh` 脚本加可执行位；Windows 是 `.cmd`：它在默认允许的工作区脚本扩展名
/// 里，gld 用 `cmd.exe /d /s /c call` 跑它。两边语法不通用，所以各给一份。Windows 那份用
/// CRLF 换行，只写 LF 的话 cmd 大多也认，但 `call`、标签这类在个别情况下会读错行。
fn write_script(env: &Env, name: &str, unix: &[&str], windows: &[&str]) -> String {
    if cfg!(windows) {
        let file = format!("{name}.cmd");
        let mut body = String::from("@echo off\r\n");
        for line in windows {
            body.push_str(line);
            body.push_str("\r\n");
        }
        env.write(&file, &body);
        return format!("./{file}");
    }
    env.write(name, &format!("#!/bin/sh\n{}\n", unix.join("\n")));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            env.project.path().join(name),
            std::fs::Permissions::from_mode(0o755),
        )
        .expect("chmod script");
    }
    format!("./{name}")
}

/// 项目里放一个先打一行字、再睡着不动的脚本，返回调它的写法。
fn write_ticker(env: &Env, sleeper: &Sleeper) -> String {
    let sleep = sleeper.command();
    write_script(
        env,
        "tick",
        &["echo before-restart", &format!("exec {sleep}")],
        &["echo before-restart", &sleep],
    )
}

/// 起一条后台命令，返回它的 session_id。
fn start_ticker(env: &Env, extra: &[&str], ticker: &str) -> String {
    let cmd = format!("cmd={ticker}");
    let mut args = extra.to_vec();
    args.extend([
        "--json",
        "tool",
        "call",
        "exec_command",
        cmd.as_str(),
        "yield_time_ms:=500",
        "timeout_ms:=120000",
    ]);
    let started = env.json(&args);
    assert_eq!(started["status"], "running", "{started}");
    started["session_id"]
        .as_str()
        .expect("session_id")
        .to_string()
}

/// 不拿 id，按 `list_runs` 找回最新的那条：重启后换了对话、id 没转述出来时就是这么找的。
fn newest_listed(env: &Env) -> serde_json::Value {
    let listed = env.json(&["--json", "tool", "call", "list_runs", "limit:=1"]);
    assert_eq!(listed["total"], 1, "{listed}");
    listed["runs"][0].clone()
}

fn read_back(env: &Env, extra: &[&str], session_id: &str) -> serde_json::Value {
    let output_ref = format!("output_ref=session:{session_id}:stdout");
    let mut args = extra.to_vec();
    args.extend(["--json", "tool", "call", "read_output", output_ref.as_str()]);
    env.json(&args)
}

/// 经守护进程跑一条命令：退出码原样带回来、输出读得到；守护进程重启之后，运行记录里的
/// 结局是 `exited`、退出码还是那个数（审查 D09）。
///
/// 这是"命令跑完了"那一半；"被停掉"那一半见下面几条 `interrupted`。
#[test]
fn a_command_exit_code_and_outcome_survive_a_daemon_restart() {
    let env = Env::new();
    let script = write_script(
        &env,
        "seven",
        &["echo exit-seven", "exit 7"],
        &["echo exit-seven", "exit /b 7"],
    );
    env.ok(&["add", ".", "--name", "it"]);
    env.ok(&["daemon", "start"]);
    let done = env.json(&[
        "--json",
        "tool",
        "call",
        "exec_command",
        &format!("cmd={script}"),
        "yield_time_ms:=10000",
        "timeout_ms:=30000",
    ]);
    assert_eq!(done["status"], "exited", "{done}");
    assert_eq!(done["exit_code"], 7, "{done}");
    assert_eq!(done["command_ok"], false, "{done}");
    assert!(
        done["stdout"]
            .as_str()
            .is_some_and(|text| text.contains("exit-seven")),
        "{done}"
    );
    let session_id = done["session_id"].as_str().expect("session_id").to_string();

    env.ok(&["daemon", "stop"]);
    env.ok(&["daemon", "start"]);
    let listed = newest_listed(&env);
    assert_eq!(listed["session_id"], session_id.as_str(), "{listed}");
    assert_eq!(listed["exit_code"], 7, "{listed}");
    let read = read_back(&env, &[], &session_id);
    assert_eq!(read["source"], "run_record", "{read}");
    assert_eq!(read["termination_reason"], "exited", "{read}");
    assert_eq!(read["exit_code"], 7, "{read}");
    assert_eq!(read["command_ok"], false, "{read}");
    assert!(
        read["content"]
            .as_str()
            .is_some_and(|text| text.contains("exit-seven")),
        "{read}"
    );
}

/// 守护进程退出时，经 `gld tool call` 起、还在跑的后台命令要一起停掉。
///
/// 以前只停了经服务（MCP）起的：`gld tool call` 起的命令留了下来，守护进程一走就再没人
/// 读得到、停得掉它们，以你的身份一直跑到自己的 timeout。2026-09-24 在 Linux 容器里
/// 验 glibc 包时发现，macOS 上一样。Windows 上停法是 `taskkill /T /F`，另一套实现。
#[test]
fn stopping_the_daemon_stops_commands_started_through_tool_call() {
    let env = Env::new();
    let sleeper = Sleeper { ms: 61481 };
    env.ok(&["add", ".", "--name", "it"]);
    env.ok(&[
        "set",
        "it",
        &format!("allowed-commands={}", sleeper.program()),
    ]);
    env.ok(&["daemon", "start"]);
    let started = env.json(&[
        "--json",
        "tool",
        "call",
        "exec_command",
        &format!("cmd={}", sleeper.command()),
        "yield_time_ms:=0",
        "timeout_ms:=120000",
    ]);
    assert_eq!(started["status"], "running", "{started}");
    assert!(sleeper.is_running(), "后台命令没起来");

    env.ok(&["daemon", "stop"]);
    // Windows 上一次查询就要起一个 PowerShell（约 1 秒），所以给 10 秒。
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while sleeper.is_running() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let leaked = sleeper.is_running();
    if leaked {
        sleeper.kill();
    }
    assert!(!leaked, "守护进程退出后，它起的后台命令还在跑");
}

/// 没有守护进程时，`gld tool call` 起的后台命令是命令行进程的子进程：命令行退出前要停掉它，
/// 并说清楚为什么。不停的话它成了孤儿，下一条命令读不到也停不掉。
#[test]
fn a_direct_tool_call_does_not_leave_its_background_command_behind() {
    let env = Env::new();
    let sleeper = Sleeper { ms: 62519 };
    env.ok(&["add", ".", "--name", "it"]);
    env.ok(&[
        "set",
        "it",
        &format!("allowed-commands={}", sleeper.program()),
    ]);
    let output = env.gld(&[
        "--no-autostart",
        "tool",
        "call",
        "exec_command",
        &format!("cmd={}", sleeper.command()),
        "yield_time_ms:=0",
        "timeout_ms:=120000",
    ]);
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let still_running = sleeper.is_running();
    if still_running {
        sleeper.kill();
    }
    assert!(!still_running, "命令行退出后，它起的后台命令还在跑");
    assert!(
        stderr.contains("gld daemon start"),
        "没说清楚怎么办：{stderr}"
    );
}

/// 守护进程重启之后，重启前那条后台命令的结局和输出还查得到（审查 D09）。
///
/// 以前命令会话只在内存里：守护进程一停，`read_output` 报 `SESSION_NOT_FOUND`，
/// 连"它是被停掉的"都不知道。守护进程正常退出时是它自己停掉的命令，记 `interrupted`。
#[test]
fn a_command_stopped_by_a_daemon_restart_is_on_record_as_interrupted() {
    let env = Env::new();
    let sleeper = Sleeper { ms: 63281 };
    let ticker = write_ticker(&env, &sleeper);
    env.ok(&["add", ".", "--name", "it"]);
    env.ok(&["daemon", "start"]);
    let session_id = start_ticker(&env, &[], &ticker);

    env.ok(&["daemon", "stop"]);
    env.ok(&["daemon", "start"]);
    let listed = newest_listed(&env);
    let read = read_back(&env, &[], &session_id);
    if sleeper.is_running() {
        sleeper.kill();
    }
    assert_eq!(listed["session_id"], session_id.as_str(), "{listed}");
    assert_eq!(listed["termination_reason"], "interrupted", "{listed}");
    assert_eq!(listed["started_by_this_gld"], false, "{listed}");
    assert_eq!(read["termination_reason"], "interrupted", "{read}");
    assert_eq!(read["running"], false, "{read}");
    assert_eq!(read["command_ok"], false, "{read}");
    assert!(
        read["content"]
            .as_str()
            .is_some_and(|text| text.contains("before-restart")),
        "重启前的输出没留下：{read}"
    );
}

/// 守护进程被 `kill -9` 时来不及记下命令怎样了：重启后如实说 `unknown`，不猜成失败或成功，
/// 也不假装进程已经没了——它可能还作为孤儿在跑（审查 D09）。
///
/// 只在 Unix 上跑：`kill_session` 回的 `pid_in_use` 是向那个进程组发 0 号信号查出来的，
/// Windows 没有进程组信号，那里这个字段是 `null`（不知道），下面断言的 `false` 不成立。
/// 这是两边产品行为本来就不同，不是 fixture 的问题。
#[cfg(unix)]
#[test]
fn a_command_whose_daemon_was_killed_is_on_record_as_unknown() {
    let env = Env::new();
    let sleeper = Sleeper { ms: 64117 };
    let ticker = write_ticker(&env, &sleeper);
    env.ok(&["add", ".", "--name", "it"]);
    let daemon = env.json(&["--json", "daemon", "start"]);
    let pid = daemon["pid"].as_u64().expect("daemon pid").to_string();
    let session_id = start_ticker(&env, &[], &ticker);

    let killed = Command::new("kill").args(["-9", &pid]).status();
    assert!(killed.is_ok_and(|status| status.success()));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while env.gld(&["daemon", "status"]).status.code() != Some(3)
        && std::time::Instant::now() < deadline
    {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    // 先收掉孤儿再读：读失败会 panic，那时就没人清它了。收掉它不影响结论——起它的
    // 守护进程已经不在，记录照样停在 running，读的人照样该判成 unknown。
    let orphan = sleeper.is_running();
    if orphan {
        sleeper.kill();
    }
    env.ok(&["daemon", "start"]);
    // 先列再读：列表这一步就把停在 running 的记录判成 unknown，不靠 read_output 那条路。
    let listed = newest_listed(&env);
    assert_eq!(listed["session_id"], session_id.as_str(), "{listed}");
    assert_eq!(listed["termination_reason"], "unknown", "{listed}");
    assert!(listed["command_ok"].is_null(), "{listed}");
    let read = read_back(&env, &[], &session_id);
    assert!(
        orphan,
        "被 kill -9 的守护进程起的命令应当还活着（成了孤儿）"
    );
    assert_eq!(read["termination_reason"], "unknown", "{read}");
    assert_eq!(read["running"], false, "{read}");
    assert!(
        read["command_ok"].is_null(),
        "结局不知道就不该说成败：{read}"
    );
    assert!(
        read["content"]
            .as_str()
            .is_some_and(|text| text.contains("before-restart")),
        "被杀之前的输出没留下：{read}"
    );
    // kill_session 不去碰那个 pid（可能已经给了别的进程），如实说它怎样、让人自己看。
    let session_arg = format!("session_id={session_id}");
    let kill = env.json(&["--json", "tool", "call", "kill_session", &session_arg]);
    assert_eq!(kill["killed"], false, "{kill}");
    assert_eq!(
        kill["status"], "unknown",
        "不知道结局就不该说 exited：{kill}"
    );
    assert_eq!(kill["pid_in_use"], false, "孤儿已经收掉了：{kill}");
    assert!(
        kill["warnings"][0]
            .as_str()
            .is_some_and(|text| text.contains("not signalled")),
        "{kill}"
    );
}

/// 没有守护进程时，上一条命令行起的命令被它退出时停掉；下一条命令行读得到它的结局和输出。
/// 以前两个进程之间什么也没留下（审查 D09）。
#[test]
fn the_next_direct_tool_call_reads_what_the_previous_one_left() {
    let env = Env::new();
    let sleeper = Sleeper { ms: 65402 };
    let ticker = write_ticker(&env, &sleeper);
    env.ok(&["add", ".", "--name", "it"]);
    let session_id = start_ticker(&env, &["--no-autostart"], &ticker);
    let read = read_back(&env, &["--no-autostart"], &session_id);
    if sleeper.is_running() {
        sleeper.kill();
    }
    assert_eq!(read["termination_reason"], "interrupted", "{read}");
    assert!(
        read["content"]
            .as_str()
            .is_some_and(|text| text.contains("before-restart")),
        "{read}"
    );

    // 早就跑完的命令不算"停掉了"：以前退出提示把它也数进去，说成被停掉（独立审查发现）。
    let done = write_script(&env, "done", &["echo done"], &["echo done"]);
    let quick = env.gld(&[
        "--no-autostart",
        "tool",
        "call",
        "exec_command",
        &format!("cmd={done}"),
    ]);
    assert!(quick.status.success());
    let stderr = String::from_utf8_lossy(&quick.stderr);
    assert!(
        !stderr.contains("停掉了"),
        "跑完的命令被说成停掉了：{stderr}"
    );
}

/// 经服务（MCP，ChatGPT 走的就是这条路）起的命令，守护进程重启后同一个客户端读得到，
/// 结局是 `interrupted` 而不是 `killed`：不是谁要停它，是守护进程要走了（审查 D09）。
///
/// 守护进程退出时先停服务、服务再停经它起的命令，那一步记的是 `killed`（`gld stop` 也走
/// 那里，那时确实是有人要停）。所以退出时要先按 `interrupted` 把命令收掉，再停服务。
#[test]
fn a_command_started_through_the_service_is_interrupted_by_a_daemon_restart() {
    let env = Env::new();
    let sleeper = Sleeper { ms: 68330 };
    let ticker = write_ticker(&env, &sleeper);
    let service = env.serve("it", "bearer");
    let started = service.call_tool(
        "exec_command",
        serde_json::json!({"cmd": ticker, "yield_time_ms": 500, "timeout_ms": 120000}),
    );
    assert_eq!(started["status"], "running", "{started}");
    let session_id = started["session_id"]
        .as_str()
        .expect("session_id")
        .to_string();

    env.ok(&["daemon", "stop"]);
    env.ok(&["start"]);
    let read = service.call_tool(
        "read_output",
        serde_json::json!({"output_ref": format!("session:{session_id}:stdout")}),
    );
    if sleeper.is_running() {
        sleeper.kill();
    }
    assert_eq!(read["termination_reason"], "interrupted", "{read}");
    assert_eq!(read["source"], "run_record", "{read}");
    assert!(
        read["content"]
            .as_str()
            .is_some_and(|text| text.contains("before-restart")),
        "{read}"
    );
}
