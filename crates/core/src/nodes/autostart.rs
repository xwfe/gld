//! 用户级自启动：macOS LaunchAgent、Linux systemd 用户单元、Windows 登录计划任务。都不要管理员权限。
//! 按运行时的系统选路径而不是 cfg：三份定义在任何一台机器上都能编译和测试。

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use serde::Serialize;

use super::local_config::{create_private_file, read_config, LocalConfig};
use super::service::log_path;

#[cfg(test)]
#[path = "autostart_tests.rs"]
mod tests;

/// 服务管理器给的环境几乎是空的。节点要靠这些找到数据目录、走代理、让 AI 跑的命令找得到工具，
/// launchd 的 PATH 只有 `/usr/bin:/bin:/usr/sbin:/sbin`。开启时记一份快照。
const CAPTURED_ENV: [&str; 11] = [
    "PATH",
    "GLD_HOME",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "HTTPS_PROXY",
    "https_proxy",
    "ALL_PROXY",
    "all_proxy",
    "NO_PROXY",
    "no_proxy",
];
const STOP_WAIT: Duration = Duration::from_secs(10);
const START_WAIT: Duration = Duration::from_secs(5);
const BOOTSTRAP_ATTEMPTS: u32 = 5;

#[derive(Debug, Serialize)]
pub struct Status {
    pub manager: &'static str,
    pub label: String,
    pub installed: bool,
    /// 这份配置的节点进程在不在，不管是自启动起的还是前台起的。
    pub running: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub definition: Option<PathBuf>,
    pub log: PathBuf,
    /// 配上了，但人得知道的限制，比如重启后要登录才回来。
    pub notes: Vec<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Manager {
    Launchd,
    Systemd,
    TaskScheduler,
}

struct Spec {
    label: String,
    exe: PathBuf,
    config: PathBuf,
    env: Vec<(String, String)>,
}

impl Spec {
    fn arguments(&self) -> Vec<String> {
        vec![
            "node".into(),
            "run".into(),
            "--config".into(),
            self.config.display().to_string(),
            "--service".into(),
        ]
    }
}

fn manager() -> Result<Manager, String> {
    match std::env::consts::OS {
        "macos" => Ok(Manager::Launchd),
        "linux" => Ok(Manager::Systemd),
        "windows" => Ok(Manager::TaskScheduler),
        other => Err(format!("Node autostart is not supported on {other}")),
    }
}

fn manager_name(manager: Manager) -> &'static str {
    match manager {
        Manager::Launchd => "launchd",
        Manager::Systemd => "systemd",
        Manager::TaskScheduler => "task-scheduler",
    }
}

/// 机器 ID 不变，所以换二进制、重跑一次 enable 改的是同一个服务；同账号接多个 Hub 也不打架。
fn label(machine_id: &str) -> String {
    format!(
        "dev.gld.node.{}",
        machine_id.get(..12).unwrap_or(machine_id)
    )
}

fn spec_for(config_path: &Path) -> Result<Spec, String> {
    let config = read_config(config_path)?;
    let exe = std::env::current_exe()
        .and_then(|path| path.canonicalize())
        .map_err(|_| "Cannot locate this program for autostart")?;
    let spec = Spec {
        label: label(&config.machine_id),
        exe: without_verbatim_prefix(exe),
        config: without_verbatim_prefix(
            config_path
                .canonicalize()
                .map_err(|_| "Cannot resolve the node configuration path")?,
        ),
        env: CAPTURED_ENV
            .iter()
            .filter_map(|name| {
                let value = std::env::var(name).ok().filter(|value| !value.is_empty())?;
                // 服务的工作目录不是这里，相对路径的 GLD_HOME 会指到别处，节点就找不到项目了。
                let value = match *name {
                    "GLD_HOME" => crate::home::data_home()
                        .map(|home| home.display().to_string())
                        .unwrap_or(value),
                    _ => value,
                };
                Some((name.to_string(), value))
            })
            .collect(),
    };
    // 换行能往 systemd 单元里多塞一行指令，也会写坏 plist。这些值只来自本账号，但不该写进去。
    let texts = [
        spec.exe.display().to_string(),
        spec.config.display().to_string(),
    ];
    if texts
        .iter()
        .map(String::as_str)
        .chain(spec.env.iter().map(|(_, value)| value.as_str()))
        .any(|text| text.chars().any(char::is_control))
    {
        return Err(
            "A path or environment variable for autostart contains control characters".into(),
        );
    }
    Ok(spec)
}

/// Windows 的 canonicalize 给出 `\\?\C:\…`，别的程序未必认，写进任务定义要还原成普通路径。
fn without_verbatim_prefix(path: PathBuf) -> PathBuf {
    let text = path.display().to_string();
    match text.strip_prefix(r"\\?\") {
        Some(plain) if plain.as_bytes().get(1) == Some(&b':') => PathBuf::from(plain),
        _ => path,
    }
}

/// 把这份配置交给服务管理器并启动。已配过的先停掉再换，重跑就是升级后的重启。
pub fn enable(config_path: &Path) -> Result<Status, String> {
    let manager = manager()?;
    let spec = spec_for(config_path)?;
    let definition = definition_path(manager, &spec.label)?;
    stop_service(manager, &spec.label);
    // 自启动的已经停了，还占着锁就是前台那个。两个都留着，后起的拿不到锁，每分钟报一次错。
    wait_until_free(&spec.config)?;
    let started = match manager {
        Manager::Launchd => write_definition(definition.as_deref(), &launchd_plist(&spec))
            .and_then(|()| start_launchd(&spec.label, definition.as_deref())),
        Manager::Systemd => write_definition(definition.as_deref(), &systemd_unit(&spec))
            .and_then(|()| start_systemd(&spec.label)),
        Manager::TaskScheduler => start_task(&spec),
    };
    let mut notes = match started {
        Ok(notes) => notes,
        Err(error) => {
            // 报了失败就不能留着：定义里可能有代理密码，下次登录还会自己起来，而用户以为没配上。
            let _ = remove_service(manager, &spec.label, definition.as_deref());
            return Err(error);
        }
    };
    let running = wait_until_running(&spec.config);
    if !running {
        notes.push(format!(
            "The node has not started yet; check {}",
            log_path(&spec.config).display()
        ));
    }
    Ok(Status {
        manager: manager_name(manager),
        label: spec.label,
        installed: true,
        running,
        definition,
        log: log_path(&spec.config),
        notes,
    })
}

/// 停掉并删除定义。返回之前有没有配过；没配过不算错。
pub fn disable(config_path: &Path) -> Result<bool, String> {
    let manager = manager()?;
    let config = read_config(config_path)?;
    let label = label(&config.machine_id);
    let definition = definition_path(manager, &label)?;
    let installed = is_installed(manager, &label, definition.as_deref());
    if !installed {
        return Ok(false);
    }
    stop_service(manager, &label);
    // 只看命令有没有返回成功不够：比如 su 切来的会话连不上用户服务，定义删了，节点却还连着 Hub，
    // 这时说“已停止”就是在权限上误导人。没停下就报错，定义留着。
    wait_until_free(config_path).map_err(|_| {
        "The node is still running, so autostart was left in place. If it runs in a terminal press Ctrl-C there; otherwise run this from a normal login session, not su".to_string()
    })?;
    remove_service(manager, &label, definition.as_deref())?;
    Ok(true)
}

fn remove_service(manager: Manager, label: &str, definition: Option<&Path>) -> Result<(), String> {
    match manager {
        Manager::Launchd => remove_definition(definition),
        Manager::Systemd => {
            let _ = tool("systemctl", &["--user", "disable", &unit_name(label)]);
            remove_definition(definition)?;
            let _ = tool("systemctl", &["--user", "daemon-reload"]);
            Ok(())
        }
        Manager::TaskScheduler => {
            let deleted = tool("schtasks", &["/Delete", "/TN", &task_name(label), "/F"])?;
            if deleted.status.success() {
                Ok(())
            } else {
                Err(tool_error("schtasks /Delete", &deleted))
            }
        }
    }
}

pub fn status(config_path: &Path) -> Result<Status, String> {
    let manager = manager()?;
    let config = read_config(config_path)?;
    let label = label(&config.machine_id);
    let definition = definition_path(manager, &label)?;
    let installed = is_installed(manager, &label, definition.as_deref());
    let mut notes = Vec::new();
    if installed && manager == Manager::Systemd && !linger_enabled() {
        notes.push(LINGER_NOTE.to_string());
    }
    Ok(Status {
        manager: manager_name(manager),
        label,
        installed,
        running: LocalConfig::in_use(config_path)?,
        definition,
        log: log_path(config_path),
        notes,
    })
}

const LINGER_NOTE: &str = "Without lingering this node stops at logout and returns only at the next login; run: loginctl enable-linger";

fn definition_path(manager: Manager, label: &str) -> Result<Option<PathBuf>, String> {
    let home = || dirs::home_dir().ok_or("Cannot locate the home directory for autostart");
    Ok(match manager {
        Manager::Launchd => Some(
            home()?
                .join("Library/LaunchAgents")
                .join(format!("{label}.plist")),
        ),
        Manager::Systemd => Some(
            std::env::var_os("XDG_CONFIG_HOME")
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
                .unwrap_or(home()?.join(".config"))
                .join("systemd/user")
                .join(unit_name(label)),
        ),
        Manager::TaskScheduler => None,
    })
}

fn is_installed(manager: Manager, label: &str, definition: Option<&Path>) -> bool {
    match manager {
        Manager::TaskScheduler => tool("schtasks", &["/Query", "/TN", &task_name(label)])
            .is_ok_and(|output| output.status.success()),
        _ => definition.is_some_and(Path::exists),
    }
}

fn stop_service(manager: Manager, label: &str) {
    // 没配过时这些都会失败，本来就不要紧；真正启动失败会在后面报。
    match manager {
        Manager::Launchd => {
            for domain in launchd_domains() {
                let _ = tool("launchctl", &["bootout", &format!("{domain}/{label}")]);
            }
            let deadline = Instant::now() + STOP_WAIT;
            while launchd_loaded(label) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        Manager::Systemd => {
            let _ = tool("systemctl", &["--user", "stop", &unit_name(label)]);
        }
        Manager::TaskScheduler => {
            let _ = tool("schtasks", &["/End", "/TN", &task_name(label)]);
        }
    }
}

fn wait_until_free(config: &Path) -> Result<(), String> {
    let deadline = Instant::now() + STOP_WAIT;
    while LocalConfig::in_use(config)? {
        if Instant::now() >= deadline {
            return Err("A node is already running with this configuration; stop it first (Ctrl-C in its terminal)".into());
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    Ok(())
}

fn wait_until_running(config: &Path) -> bool {
    let deadline = Instant::now() + START_WAIT;
    loop {
        if LocalConfig::in_use(config).unwrap_or(false) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

// ------------------------------------------------------------------ launchd

fn launchd_domains() -> [String; 2] {
    let uid = current_uid();
    [format!("gui/{uid}"), format!("user/{uid}")]
}

/// 有图形登录的用 gui 域；SSH 进来、没人登录桌面时 gui 域不存在，用 user 域，起得来但重启后要登录才回来。
/// 先看域存不存在再选，不靠失败后回退：gui 域一时失败就退到 user 域，服务会装到用户看不到的地方。
fn start_launchd(label: &str, definition: Option<&Path>) -> Result<Vec<String>, String> {
    let plist = definition
        .ok_or("launchd needs a definition file")?
        .display()
        .to_string();
    let [gui, user] = launchd_domains();
    let desktop = tool("launchctl", &["print", &gui]).is_ok_and(|out| out.status.success());
    let domain = if desktop { &gui } else { &user };
    // 实测：刚 bootout 完的服务马上 bootstrap 会报 5: Input/output error，过一会儿就好。
    let mut attempts = 0;
    let failure = loop {
        let output = tool("launchctl", &["bootstrap", domain, &plist])?;
        if output.status.success() {
            return Ok(if desktop {
                Vec::new()
            } else {
                vec![format!(
                    "No desktop login right now, so {label} runs in the user domain; after a reboot it returns only once this account logs in"
                )]
            });
        }
        attempts += 1;
        if attempts >= BOOTSTRAP_ATTEMPTS {
            break output;
        }
        std::thread::sleep(Duration::from_secs(1));
    };
    Err(tool_error("launchctl bootstrap", &failure))
}

fn launchd_loaded(label: &str) -> bool {
    launchd_domains().iter().any(|domain| {
        tool("launchctl", &["print", &format!("{domain}/{label}")])
            .is_ok_and(|output| output.status.success())
    })
}

fn launchd_plist(spec: &Spec) -> String {
    let mut program = vec![spec.exe.display().to_string()];
    program.extend(spec.arguments());
    let arguments: String = program
        .iter()
        .map(|argument| format!("    <string>{}</string>\n", xml_escape(argument)))
        .collect();
    let environment: String = spec
        .env
        .iter()
        .map(|(name, value)| {
            format!(
                "    <key>{}</key><string>{}</string>\n",
                xml_escape(name),
                xml_escape(value)
            )
        })
        .collect();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>{label}</string>
  <key>ProgramArguments</key><array>
{arguments}  </array>
  <key>EnvironmentVariables</key><dict>
{environment}  </dict>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
  <key>ThrottleInterval</key><integer>60</integer>
  <key>StandardErrorPath</key><string>{log}</string>
</dict></plist>
"#,
        label = xml_escape(&spec.label),
        log = xml_escape(&log_path(&spec.config).display().to_string()),
    )
}

// ------------------------------------------------------------------ systemd

fn unit_name(label: &str) -> String {
    format!("{label}.service")
}

fn start_systemd(label: &str) -> Result<Vec<String>, String> {
    let unit = unit_name(label);
    let reload = tool("systemctl", &["--user", "daemon-reload"])?;
    if !reload.status.success() {
        return Err(format!(
            "{}. This session has no systemd user manager (common over su, in containers, or without systemd); run `gld node run` in a terminal instead",
            tool_error("systemctl --user", &reload)
        ));
    }
    let started = tool("systemctl", &["--user", "enable", "--now", &unit])?;
    if !started.status.success() {
        return Err(tool_error("systemctl --user enable", &started));
    }
    if linger_enabled() {
        return Ok(Vec::new());
    }
    // 用户单元默认随最后一次退出登录一起停。服务器上这等于“开机自启”不生效，所以这里直接开。
    match tool("loginctl", &["enable-linger"]) {
        Ok(output) if output.status.success() => Ok(Vec::new()),
        _ => Ok(vec![LINGER_NOTE.to_string()]),
    }
}

fn linger_enabled() -> bool {
    tool(
        "loginctl",
        &["show-user", &current_uid().to_string(), "--property=Linger"],
    )
    .is_ok_and(|output| String::from_utf8_lossy(&output.stdout).contains("Linger=yes"))
}

fn systemd_unit(spec: &Spec) -> String {
    let mut command = vec![systemd_quote(&spec.exe.display().to_string())];
    command.extend(
        spec.arguments()
            .iter()
            .map(|argument| systemd_quote(argument)),
    );
    let environment: String = spec
        .env
        .iter()
        .map(|(name, value)| {
            format!(
                "Environment={}\n",
                systemd_quote(&format!("{name}={value}"))
            )
        })
        .collect();
    format!(
        "[Unit]\nDescription=gld-node {label}\n\n[Service]\nExecStart={command}\n{environment}Restart=on-failure\nRestartSec=60\n\n[Install]\nWantedBy=default.target\n",
        label = spec.label,
        command = command.join(" "),
    )
}

/// systemd 的引号里 `\\` 和 `"` 要转义，`%` 是说明符。
fn systemd_environment(value: &str) -> String {
    let escaped = value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('%', "%%");
    format!("\"{escaped}\"")
}

/// ExecStart 比 Environment 多一层：`$` 会被当变量展开。
fn systemd_quote(value: &str) -> String {
    systemd_environment(value).replace('$', "$$")
}

// ------------------------------------------------------------------ Windows

/// 放在根目录：建任务子文件夹，普通账号可能没权限。
fn task_name(label: &str) -> String {
    label.to_string()
}

fn start_task(spec: &Spec) -> Result<Vec<String>, String> {
    let user = windows_user()?;
    // schtasks /Create /SC ONLOGON 对普通账号常报拒绝访问；用 XML 只给自己账号建登录触发器不需要管理员。
    let xml = task_xml(spec, &user);
    let mut name = spec.config.as_os_str().to_os_string();
    name.push(".task.xml");
    let path = PathBuf::from(name);
    let mut encoded = vec![0xFF_u8, 0xFE];
    encoded.extend(xml.encode_utf16().flat_map(u16::to_le_bytes));
    let _ = std::fs::remove_file(&path);
    let mut file =
        create_private_file(&path).map_err(|_| "Cannot write the scheduled task definition")?;
    file.write_all(&encoded)
        .map_err(|_| "Cannot write the scheduled task definition")?;
    drop(file);
    let task = task_name(&spec.label);
    let created = tool(
        "schtasks",
        &[
            "/Create",
            "/TN",
            &task,
            "/XML",
            &path.display().to_string(),
            "/F",
        ],
    );
    let _ = std::fs::remove_file(&path);
    let created = created?;
    if !created.status.success() {
        return Err(tool_error("schtasks /Create", &created));
    }
    let started = tool("schtasks", &["/Run", "/TN", &task])?;
    if !started.status.success() {
        return Err(tool_error("schtasks /Run", &started));
    }
    Ok(vec![
        "It starts at this account's next login, not at boot; GLD_HOME, proxy and PATH come from the account's own environment, not from this shell".into(),
    ])
}

fn windows_user() -> Result<String, String> {
    let user =
        std::env::var("USERNAME").map_err(|_| "Cannot determine the current Windows user")?;
    Ok(match std::env::var("USERDOMAIN") {
        Ok(domain) if !domain.is_empty() => format!("{domain}\\{user}"),
        _ => user,
    })
}

fn task_xml(spec: &Spec, user: &str) -> String {
    let arguments = spec
        .arguments()
        .iter()
        .map(|argument| format!("\"{argument}\""))
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo><Description>gld-node {label}</Description></RegistrationInfo>
  <Triggers><LogonTrigger><Enabled>true</Enabled><UserId>{user}</UserId></LogonTrigger></Triggers>
  <Principals><Principal id="Author"><UserId>{user}</UserId><LogonType>InteractiveToken</LogonType><RunLevel>LeastPrivilege</RunLevel></Principal></Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <RestartOnFailure><Interval>PT1M</Interval><Count>999</Count></RestartOnFailure>
  </Settings>
  <Actions Context="Author"><Exec><Command>{command}</Command><Arguments>{arguments}</Arguments></Exec></Actions>
</Task>
"#,
        label = xml_escape(&spec.label),
        user = xml_escape(user),
        command = xml_escape(&spec.exe.display().to_string()),
        arguments = xml_escape(&arguments),
    )
}

// ------------------------------------------------------------------ 公共

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// 定义里可能有代理账号密码（来自环境变量），只给本账号读。先写临时文件再改名，中途失败不留半份。
fn write_definition(definition: Option<&Path>, content: &str) -> Result<(), String> {
    let path = definition.ok_or("This service manager needs a definition file")?;
    let parent = path.parent().ok_or("Invalid autostart definition path")?;
    std::fs::create_dir_all(parent).map_err(|_| "Cannot create the autostart directory")?;
    let mut name = path.as_os_str().to_os_string();
    name.push(".tmp");
    let temp = PathBuf::from(name);
    let _ = std::fs::remove_file(&temp);
    let written = create_private_file(&temp)
        .and_then(|mut file| file.write_all(content.as_bytes()))
        .and_then(|()| std::fs::rename(&temp, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&temp);
        return Err("Cannot write the autostart definition".into());
    }
    Ok(())
}

fn remove_definition(definition: Option<&Path>) -> Result<(), String> {
    match definition {
        Some(path) => match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err("Cannot remove the autostart definition".into()),
        },
        None => Ok(()),
    }
}

fn tool(program: &str, arguments: &[&str]) -> Result<Output, String> {
    Command::new(program)
        .args(arguments)
        .output()
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                format!("{program} was not found; this machine cannot use autostart, run `gld node run` in a terminal instead")
            } else {
                format!("Cannot run {program}")
            }
        })
}

fn tool_error(what: &str, output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let detail = [stderr.trim(), stdout.trim()]
        .into_iter()
        .find(|text| !text.is_empty())
        .unwrap_or("no output");
    format!("{what} failed: {detail}")
}

fn current_uid() -> u32 {
    #[cfg(unix)]
    {
        unsafe { libc::geteuid() }
    }
    #[cfg(not(unix))]
    {
        0
    }
}
