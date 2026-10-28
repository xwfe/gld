//! C0 的本机预检：默认只找文件；显式核验摘要后才握手/列工具，不调用任何桌面动作。

mod state;

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use toexec_mcp::{client::Client, Error, Recv, Transport};

pub const PROVIDER_VERSION: &str = "0.38.0";
pub const PROVIDER_COMMIT: &str = "85e2b1318bd77ccf3cfa88ccdf875a4bb7eaed19";
const MAX_BINARY_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct Options {
    pub binary: String,
    pub connect: bool,
    /// 安装后可执行文件的摘要，不是下载压缩包的摘要。
    pub expected_sha256: Option<String>,
    pub expected_tools_sha256: Option<String>,
    pub timeout_ms: u64,
}

#[derive(Debug, Serialize)]
pub struct Binary {
    pub path: PathBuf,
    pub bytes: u64,
    pub sha256: String,
}

#[derive(Debug, Serialize)]
pub struct Protocol {
    pub version: String,
    pub server_name: Option<String>,
    pub server_version: Option<String>,
    pub tools: Vec<String>,
    pub tools_sha256: String,
}

#[derive(Debug, Serialize)]
pub struct Failure {
    pub code: &'static str,
    pub message: String,
}

type Result<T> = std::result::Result<T, Failure>;

#[derive(Debug, Serialize)]
pub struct Report {
    pub ok: bool,
    pub mode: &'static str,
    pub host_os: &'static str,
    pub host_arch: &'static str,
    pub provider_pin: Value,
    pub binary: Option<Binary>,
    pub process_started: bool,
    pub protocol: Option<Protocol>,
    pub desktop_verified: bool,
    pub state_directory_removed: Option<bool>,
    pub retained_state_directory: Option<PathBuf>,
    pub failure: Option<Failure>,
    pub note: &'static str,
}

/// 不读取 gld 配置、不连接守护进程。调用者须在阻塞线程上运行。
pub fn run(options: &Options) -> Report {
    run_in(options, &std::env::temp_dir())
}

fn run_in(options: &Options, temporary_parent: &Path) -> Report {
    let mut report = Report {
        ok: false,
        mode: if options.connect { "protocol" } else { "inspect" },
        host_os: std::env::consts::OS,
        host_arch: std::env::consts::ARCH,
        provider_pin: json!({"version":PROVIDER_VERSION,"tag":"cua-driver-rs-v0.38.0",
            "commit":PROVIDER_COMMIT,"prerelease":true}),
        binary: None,
        process_started: false,
        protocol: None,
        desktop_verified: false,
        state_directory_removed: None,
        retained_state_directory: None,
        failure: None,
        note: "Inspection does not run the provider. Protocol mode sends only initialize, notifications/initialized and tools/list; server-initiated requests use the existing client's refusal rules. No screenshot, UI tree, permissions, input or desktop-stop verification is performed. Process environment isolation is not an OS sandbox or a signature check.",
    };
    let checked = (|| {
        if !(100..=30000).contains(&options.timeout_ms) {
            return Err(fail("PROBE_ARGUMENT", "timeout_ms must be 100..=30000"));
        }
        for hash in [&options.expected_sha256, &options.expected_tools_sha256]
            .into_iter()
            .flatten()
        {
            if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(fail(
                    "PROBE_ARGUMENT",
                    "Expected SHA-256 must be 64 hexadecimal characters",
                ));
            }
        }
        if options.expected_tools_sha256.is_some() && !options.connect {
            return Err(fail(
                "PROBE_ARGUMENT",
                "A tool schema fingerprint requires --connect",
            ));
        }
        let cwd = std::env::current_dir()
            .map_err(|_| fail("PROBE_IO", "Cannot read working directory"))?;
        let path = which::which_in(&options.binary, std::env::var_os("PATH"), &cwd)
            .map_err(|_| {
                fail(
                    "PROVIDER_NOT_FOUND",
                    "Provider executable was not found; no software was installed or started",
                )
            })?
            .canonicalize()
            .map_err(|_| fail("PROBE_IO", "Cannot resolve provider executable"))?;
        let (bytes, sha256) = digest(&path)?;
        report.binary = Some(Binary {
            path,
            bytes,
            sha256: sha256.clone(),
        });
        if options
            .expected_sha256
            .as_ref()
            .is_some_and(|expected| !expected.eq_ignore_ascii_case(&sha256))
        {
            return Err(fail(
                "PROVIDER_HASH_MISMATCH",
                "Executable digest does not match; provider was not started",
            ));
        }
        if options.connect && options.expected_sha256.is_none() {
            return Err(fail("PROVIDER_HASH_REQUIRED", "--connect requires --sha256 for the executable after verifying its installation; an archive digest is not the executable digest"));
        }
        Ok(())
    })();
    if let Err(failure) = checked {
        report.failure = Some(failure);
        return report;
    }
    if !options.connect {
        report.ok = true;
        return report;
    }
    let state = match state::Directory::create(temporary_parent) {
        Ok(state) => state,
        Err(failure) => {
            report.failure = Some(failure);
            return report;
        }
    };
    let outcome = connect(options, &state, &mut report);
    report.failure = outcome.err();
    // connect 的 Client/Transport 已先释放；只删除本次创建的私有目录，不扫描系统缓存。
    report.state_directory_removed = Some(state.remove().is_ok());
    if report.state_directory_removed == Some(false) {
        report.retained_state_directory = Some(state.path.clone());
        if report.failure.is_none() {
            report.failure = Some(fail(
                "PROBE_CLEANUP_FAILED",
                "Probe stopped but its private state directory could not be removed",
            ));
        }
    }
    report.ok = report.failure.is_none();
    report
}

fn connect(options: &Options, state: &state::Directory, report: &mut Report) -> Result<()> {
    let binary = report.binary.as_ref().expect("checked binary");
    let program = binary
        .path
        .to_str()
        .ok_or_else(|| fail("PROBE_IO", "Executable path must be UTF-8"))?;
    let path = std::env::var_os("PATH");
    let child = crate::machine_mcp::open::spawn_isolated(
        program,
        &["mcp".into(), "--direct".into()],
        &state.environment()?,
        &state.path,
        path.as_ref(),
    )
    .map_err(transport_failure)?;
    report.process_started = true;
    let timeout = Duration::from_millis(options.timeout_ms);
    let transport = Deadline {
        inner: Box::new(child),
        deadline: Instant::now() + timeout,
    };
    let mut client = Client::connect(
        Box::new(transport),
        ("gld-computer-probe", crate::VERSION),
        timeout,
    )
    .map_err(transport_failure)?;
    if client.server_name.as_deref() != Some("cua-driver")
        || client.server_version.as_deref() != Some(PROVIDER_VERSION)
        || client.protocol != "2025-06-18"
    {
        return Err(fail("PROVIDER_IDENTITY_MISMATCH", "Provider must identify as cua-driver 0.38.0 with MCP 2025-06-18; no desktop tool was called"));
    }
    let tools = client.list_tools(timeout).map_err(transport_failure)?;
    let (names, sha256) = fingerprint(&tools)?;
    report.protocol = Some(Protocol {
        version: client.protocol.clone(),
        server_name: client.server_name.clone(),
        server_version: client.server_version.clone(),
        tools: names,
        tools_sha256: sha256.clone(),
    });
    drop(client);
    // 不是对同一账号恶意换文件的强隔离；普通升级/覆盖也不能误报为同一个被检验版本。
    if digest(&binary.path)? != (binary.bytes, binary.sha256.clone()) {
        return Err(fail(
            "PROVIDER_BINARY_CHANGED",
            "Executable changed during the probe; do not use this result as a version pin",
        ));
    }
    if options
        .expected_tools_sha256
        .as_ref()
        .is_some_and(|expected| !expected.eq_ignore_ascii_case(&sha256))
    {
        return Err(fail("PROVIDER_SCHEMA_CHANGED", "Tool catalog differs from the expected fingerprint; inspect the new schema before adapting it"));
    }
    Ok(())
}

fn digest(path: &Path) -> Result<(u64, String)> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file: File = options
        .open(path)
        .map_err(|_| fail("PROBE_IO", "Cannot read provider executable"))?;
    let meta = file
        .metadata()
        .map_err(|_| fail("PROBE_IO", "Cannot inspect provider executable"))?;
    if !meta.is_file() || meta.len() > MAX_BINARY_BYTES {
        return Err(fail(
            "PROBE_BINARY_INVALID",
            "Executable must be a regular file no larger than 256 MiB",
        ));
    }
    let mut input = file.take(MAX_BINARY_BYTES + 1);
    let mut hash = Sha256::new();
    let mut total = 0;
    let mut bytes = [0; 65536];
    loop {
        let count = input
            .read(&mut bytes)
            .map_err(|_| fail("PROBE_IO", "Executable digest read failed"))?;
        if count == 0 {
            break;
        }
        total += count as u64;
        if total > MAX_BINARY_BYTES {
            return Err(fail(
                "PROBE_BINARY_INVALID",
                "Executable grew beyond the digest limit",
            ));
        }
        hash.update(&bytes[..count]);
    }
    Ok((total, format!("{:x}", hash.finalize())))
}

fn fingerprint(tools: &[Value]) -> Result<(Vec<String>, String)> {
    if tools.is_empty()
        || tools.len() > 256
        || serde_json::to_vec(tools).map_or(true, |v| v.len() > 1024 * 1024)
    {
        return Err(fail(
            "PROVIDER_CATALOG_INVALID",
            "Expected 1..=256 tools and at most 1 MiB of catalog data",
        ));
    }
    let mut selected = std::collections::BTreeMap::new();
    for tool in tools {
        let name = tool["name"]
            .as_str()
            .filter(|s| {
                !s.is_empty()
                    && s.len() <= 128
                    && s.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
            })
            .ok_or_else(|| fail("PROVIDER_CATALOG_INVALID", "Invalid tool name"))?;
        if !tool["inputSchema"].is_object()
            || tool["inputSchema"]["type"] != "object"
            || selected.insert(name.to_string(), canonical(tool)).is_some()
        {
            return Err(fail(
                "PROVIDER_CATALOG_INVALID",
                "Duplicate tool or invalid object input schema",
            ));
        }
    }
    let names = selected.keys().cloned().collect();
    let bytes = serde_json::to_vec(&selected.into_values().collect::<Vec<_>>())
        .map_err(|_| fail("PROVIDER_CATALOG_INVALID", "Cannot fingerprint catalog"))?;
    Ok((names, format!("{:x}", Sha256::digest(bytes))))
}

fn canonical(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let sorted: std::collections::BTreeMap<_, _> = map.iter().collect();
            Value::Object(
                sorted
                    .into_iter()
                    .map(|(k, v)| (k.clone(), canonical(v)))
                    .collect(),
            )
        }
        Value::Array(values) => Value::Array(values.iter().map(canonical).collect()),
        _ => value.clone(),
    }
}

fn fail(code: &'static str, message: &str) -> Failure {
    Failure {
        code,
        message: message.into(),
    }
}

fn transport_failure(error: Error) -> Failure {
    // 不把第三方 stderr/握手说明带到 JSON 报告；其中可能含用户配置或凭据。
    match error {
        Error::Timeout { .. } => fail(
            "PROBE_TIMEOUT",
            "MCP handshake or tool discovery exceeded the total probe deadline",
        ),
        Error::Closed { .. } => fail(
            "PROBE_CLOSED",
            "Provider closed its protocol connection before discovery completed",
        ),
        Error::Start(_) => fail(
            "PROBE_START_FAILED",
            "Provider process could not be started",
        ),
        _ => fail(
            "PROBE_PROTOCOL",
            "Provider rejected the request or returned an unsupported MCP response",
        ),
    }
}

/// list_tools 的每页自带超时；这里让全部握手/分页合起来也受同一截止时间约束。
struct Deadline {
    inner: Box<dyn Transport>,
    deadline: Instant,
}
impl Transport for Deadline {
    fn send(&mut self, line: &str, timeout: Duration) -> std::result::Result<(), Error> {
        let left = self
            .deadline
            .saturating_duration_since(Instant::now())
            .min(timeout);
        if left.is_zero() {
            return Err(Error::Timeout {
                during: "probe".into(),
                waited: timeout,
            });
        }
        self.inner.send(line, left)
    }
    fn recv(&mut self, timeout: Duration) -> Recv {
        let left = self
            .deadline
            .saturating_duration_since(Instant::now())
            .min(timeout);
        if left.is_zero() {
            return Recv::Timeout;
        }
        self.inner.recv(left)
    }
    fn close(&mut self) {
        self.inner.close();
    }
}

#[cfg(test)]
mod tests;
