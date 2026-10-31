//! 服务模式的退出规则，以及自启动命令和服务管理器之间的步骤。launchctl、systemctl、loginctl 用桩代替：
//! 测试不能往真实账号里装服务。真实服务管理器的结果记在审查里。

use super::*;

const MANAGER_STUB: &str = r#"#!/bin/sh
echo "$0 $*" >> "$STUB_LOG"
start() {
  "$GLD_FOR_STUB" node run --config "$CONFIG_FOR_STUB" --service >/dev/null 2>&1 &
  echo $! > "$STUB_PID"
}
stop() {
  if [ -f "$STUB_PID" ]; then
    pid="$(cat "$STUB_PID")"
    kill "$pid" 2>/dev/null
    while kill -0 "$pid" 2>/dev/null; do sleep 0.05; done
    rm -f "$STUB_PID"
  fi
}
case "$(basename "$0") $*" in
  "launchctl bootstrap"*) start ;;
  "launchctl bootout"*) stop ;;
  "systemctl --user enable --now"*) start ;;
  "systemctl --user stop"*) stop ;;
  "loginctl show-user"*) echo "Linger=yes" ;;
esac
exit 0
"#;

struct Fixture {
    hub: Env,
    service: Service,
    _relay: TlsRelay,
    machine: Env,
    config: PathBuf,
    stubs: tempfile::TempDir,
    account: tempfile::TempDir,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let hub = Env::new();
        let service = hub.serve("local", "bearer");
        let relay = TlsRelay::new(service.port);
        let machine = Env::new();
        let ids = projects(&machine, name);
        let registration = register(&hub, name, TOKEN_A);
        let mut value = config(&machine, &relay, &registration, TOKEN_A, &ids);
        value["hub_url"] = json!(format!("https://localhost:{}", relay.port));
        let config = write_config(&machine, "node.json", &value);
        let stubs = tempfile::tempdir().unwrap();
        for name in ["launchctl", "systemctl", "loginctl"] {
            let path = stubs.path().join(name);
            std::fs::write(&path, MANAGER_STUB).unwrap();
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        Self {
            hub,
            service,
            _relay: relay,
            machine,
            config,
            stubs,
            account: tempfile::tempdir().unwrap(),
        }
    }

    fn cli(&self) -> Command {
        let mut command = command(&self.machine);
        let path = std::env::var("PATH").unwrap();
        command
            .env("PATH", format!("{}:{path}", self.stubs.path().display()))
            .env("HOME", self.account.path())
            .env_remove("XDG_CONFIG_HOME")
            .env("STUB_LOG", self.stubs.path().join("calls.log"))
            .env("STUB_PID", self.stubs.path().join("node.pid"))
            .env("GLD_FOR_STUB", env!("CARGO_BIN_EXE_gld"))
            .env("CONFIG_FOR_STUB", &self.config)
            .env("HTTPS_PROXY", "http://user:hunter2@127.0.0.1:9");
        command
    }

    fn autostart(&self, action: &str) -> Output {
        self.cli()
            .args(["--json", "node", "autostart", action, "--config"])
            .arg(&self.config)
            .env("NO_PROXY", "localhost")
            .output()
            .unwrap()
    }

    fn calls(&self) -> String {
        std::fs::read_to_string(self.stubs.path().join("calls.log")).unwrap_or_default()
    }

    fn pid(&self) -> Option<String> {
        std::fs::read_to_string(self.stubs.path().join("node.pid")).ok()
    }

    fn definition(&self, status: &Value) -> PathBuf {
        let label = status["label"].as_str().unwrap();
        if cfg!(target_os = "macos") {
            self.account
                .path()
                .join(format!("Library/LaunchAgents/{label}.plist"))
        } else {
            self.account
                .path()
                .join(format!(".config/systemd/user/{label}.service"))
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(pid) = self.pid() {
            let _ = Command::new("kill").arg(pid.trim()).status();
        }
    }
}

fn parsed(output: &Output) -> Value {
    assert!(
        output.status.success(),
        "command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn enable_starts_the_node_and_enabling_again_replaces_it_without_a_second_connection() {
    let fixture = Fixture::new("auto");
    let status = parsed(&fixture.autostart("enable"));
    assert_eq!(status["installed"], true);
    assert_eq!(status["running"], true);
    wait_inventory(&fixture.service, 2, 2);

    let definition = fixture.definition(&status);
    let text = std::fs::read_to_string(&definition).unwrap();
    assert!(text.contains("--service"), "{text}");
    assert!(text.contains(env!("CARGO_BIN_EXE_gld")), "{text}");
    assert!(text.contains("HTTPS_PROXY"), "{text}");
    assert!(text.contains("GLD_HOME"), "{text}");
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(&definition).unwrap().permissions().mode() & 0o777,
        0o600,
        "the definition can hold proxy credentials"
    );
    assert!(!String::from_utf8_lossy(&fixture.autostart("status").stdout).contains("hunter2"));

    let first = fixture.pid().unwrap();
    let again = parsed(&fixture.autostart("enable"));
    assert_eq!(again["running"], true);
    assert_ne!(fixture.pid().unwrap(), first, "enable should restart");
    wait_inventory(&fixture.service, 2, 2);

    let shown = parsed(&fixture.autostart("status"));
    assert_eq!(shown["installed"], true);
    assert_eq!(shown["running"], true);

    let disabled = parsed(&fixture.autostart("disable"));
    assert_eq!(disabled["removed"], true);
    assert!(!definition.exists());
    wait_inventory(&fixture.service, 0, 2);
    let shown = parsed(&fixture.autostart("status"));
    assert_eq!(shown["installed"], false);
    assert_eq!(shown["running"], false);
    assert_eq!(parsed(&fixture.autostart("disable"))["removed"], false);
    // 配置还在，能接着用前台跑。
    assert!(fixture.config.exists());
}

#[test]
fn enable_refuses_while_a_foreground_node_is_running_and_installs_nothing() {
    let fixture = Fixture::new("foreground");
    let mut node = NodeProcess::start(&fixture.machine, &fixture.config);
    wait_inventory(&fixture.service, 2, 2);
    let output = fixture.autostart("enable");
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("already running"), "{error}");
    let shown = parsed(&fixture.autostart("status"));
    assert_eq!(shown["installed"], false);
    assert_eq!(shown["running"], true);
    node.stop();
}

#[test]
fn leaving_removes_autostart_so_the_manager_cannot_restart_a_deleted_node() {
    let fixture = Fixture::new("leaving");
    let status = parsed(&fixture.autostart("enable"));
    wait_inventory(&fixture.service, 2, 2);
    let left = fixture
        .cli()
        .args(["--json", "leave", "--yes", "--config"])
        .arg(&fixture.config)
        .output()
        .unwrap();
    parsed(&left);
    assert!(!fixture.config.exists());
    assert!(!fixture.definition(&status).exists());
    wait_inventory(&fixture.service, 0, 2);
    assert!(fixture.calls().contains("bootout") || fixture.calls().contains("--user stop"));
}

fn service_process(env: &Env, config: &Path) -> NodeProcess {
    let child = command(env)
        .args(["node", "run", "--service", "--config"])
        .arg(config)
        .spawn()
        .unwrap();
    NodeProcess(Some(child))
}

fn log_of(config: &Path) -> String {
    let mut name = config.as_os_str().to_os_string();
    name.push(".log");
    std::fs::read_to_string(PathBuf::from(name)).unwrap_or_default()
}

#[test]
fn a_revoked_service_stops_for_good_with_a_clean_exit_and_says_why() {
    let fixture = Fixture::new("revoked");
    let mut node = service_process(&fixture.machine, &fixture.config);
    wait_inventory(&fixture.service, 2, 2);
    wait_until("the log should say connected", || {
        log_of(&fixture.config).contains("connected to the Hub")
    });
    let machine: Value =
        serde_json::from_str(&std::fs::read_to_string(&fixture.config).unwrap()).unwrap();
    fixture
        .hub
        .ok(&["node", "revoke", machine["machine_id"].as_str().unwrap()]);
    node.wait_stopped();
    let status = node.0.as_mut().unwrap().try_wait().unwrap().unwrap();
    assert!(status.success(), "launchd restarts on a non-zero exit");
    let log = log_of(&fixture.config);
    assert!(log.contains("HTTP 401"), "{log}");
    assert!(log.contains("joins the Hub again"), "{log}");
}

#[test]
fn a_certificate_failure_keeps_the_service_alive_where_a_foreground_node_would_exit() {
    let fixture = Fixture::new("captive");
    // 不配 CA，测试证书不受信，和笔记本遇到认证页面劫持 TLS 是同一条路。
    let mut value: Value =
        serde_json::from_str(&std::fs::read_to_string(&fixture.config).unwrap()).unwrap();
    value.as_object_mut().unwrap().remove("ca_certificate");
    let config = write_config(&fixture.machine, "untrusted.json", &value);

    let error = NodeProcess::start(&fixture.machine, &config).rejected();
    assert!(error.contains("TLS verification failed"), "{error}");

    let mut node = service_process(&fixture.machine, &config);
    wait_until("the log should show the failure", || {
        log_of(&config).contains("trying again in 60s")
    });
    assert!(log_of(&config).contains("TLS verification failed"));
    assert!(node.0.as_mut().unwrap().try_wait().unwrap().is_none());
    assert!(!log_of(&config).contains(TOKEN_A));
    node.stop();
}

#[test]
fn a_service_with_a_missing_project_exits_cleanly_instead_of_being_restarted_forever() {
    let fixture = Fixture::new("gone");
    fixture.machine.ok(&["rm", "--yes", "api"]);
    let mut node = service_process(&fixture.machine, &fixture.config);
    node.wait_stopped();
    let status = node.0.as_mut().unwrap().try_wait().unwrap().unwrap();
    assert!(status.success());
    assert!(log_of(&fixture.config).contains("not starting"));
}
