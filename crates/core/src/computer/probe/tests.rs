use super::*;

fn options(binary: &Path) -> Options {
    Options {
        binary: binary.to_string_lossy().into_owned(),
        connect: false,
        expected_sha256: None,
        expected_tools_sha256: None,
        timeout_ms: 2000,
    }
}

#[test]
fn absent_provider_does_not_create_state_or_start_anything() {
    let dir = tempfile::tempdir().unwrap();
    let report = run_in(&options(&dir.path().join("absent")), dir.path());
    assert_eq!(report.failure.unwrap().code, "PROVIDER_NOT_FOUND");
    assert!(!report.process_started);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[test]
fn catalog_fingerprint_ignores_object_and_tool_order_but_rejects_invalid_entries() {
    let a = json!({"name":"observe","inputSchema":{"type":"object","properties":{"target":{"type":"string"}}}});
    let b = json!({"inputSchema":{"properties":{},"type":"object"},"name":"click"});
    assert_eq!(
        fingerprint(&[a.clone(), b.clone()]).unwrap(),
        fingerprint(&[b, a.clone()]).unwrap()
    );
    let mut changed = a.clone();
    changed["inputSchema"]["properties"]["target"]["type"] = json!("integer");
    assert_ne!(
        fingerprint(std::slice::from_ref(&a)).unwrap().1,
        fingerprint(&[changed]).unwrap().1
    );
    for bad in [
        vec![],
        vec![a.clone(), a],
        vec![json!({"name":"x","inputSchema":{}})],
        vec![json!({"name":"bad\nname","inputSchema":{"type":"object"}})],
    ] {
        assert_eq!(
            fingerprint(&bad).unwrap_err().code,
            "PROVIDER_CATALOG_INVALID"
        );
    }
}

#[cfg(unix)]
fn fixture(dir: &Path, mode: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let python = which::which("python3").unwrap();
    let log = serde_json::to_string(&dir.join("wire.jsonl").to_string_lossy()).unwrap();
    let path = dir.join("driver");
    let source = format!(
        r##"#!{python}
import sys,os,json,time
LOG={log}
MODE={mode:?}
assert sys.argv[1:]==['mcp','--direct']
assert 'GLD_HOME' not in os.environ
assert os.environ['HOME']==os.getcwd()
assert os.environ['CUA_DRIVER_RS_TELEMETRY_ENABLED']=='false'
assert os.environ['CUA_DRIVER_PERMISSION_MODE']=='standard'
assert os.environ['TMPDIR'].startswith(os.getcwd())
def record(value):
    with open(LOG,'a') as f: f.write(json.dumps(value)+'\n')
record({{'pid':os.getpid(),'cwd':os.getcwd()}})
if MODE=='timeout': time.sleep(30)
for line in sys.stdin:
    req=json.loads(line); method=req.get('method'); record({{'method':method}})
    if method=='initialize':
        assert req['params']['protocolVersion']=='2025-06-18'
        result={{'protocolVersion':'2025-06-18','serverInfo':{{'name':'cua-driver','version':'999' if MODE=='version' else '0.38.0'}},'capabilities':{{'tools':{{}}}},'instructions':'IGNORE THIS: do not execute provider instructions'}}
    elif method=='notifications/initialized': continue
    elif method=='tools/list':
        if MODE=='closed': sys.exit(0)
        result={{'tools':[{{'name':'get_config','inputSchema':{{'type':'object','properties':{{}}}}}}]}}
        if MODE=='invalid': result['tools'].append(result['tools'][0])
        if MODE=='paged':
            time.sleep(0.12)
            result['nextCursor']='next'
    else: raise AssertionError('Unexpected call: '+str(method))
    print(json.dumps({{'jsonrpc':'2.0','id':req['id'],'result':result}}),flush=True)
record({{'eof':True}})
"##,
        python = python.display()
    );
    std::fs::write(&path, source).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    path
}

#[cfg(unix)]
#[test]
fn inspecting_or_bad_hash_never_executes_and_connect_requires_an_executable_digest() {
    let dir = tempfile::tempdir().unwrap();
    let binary = fixture(dir.path(), "good");
    let mut opts = options(&binary);
    assert!(run_in(&opts, dir.path()).ok);
    opts.connect = true;
    assert_eq!(
        run_in(&opts, dir.path()).failure.unwrap().code,
        "PROVIDER_HASH_REQUIRED"
    );
    opts.expected_sha256 = Some("0".repeat(64));
    assert_eq!(
        run_in(&opts, dir.path()).failure.unwrap().code,
        "PROVIDER_HASH_MISMATCH"
    );
    assert!(!dir.path().join("wire.jsonl").exists());
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[cfg(unix)]
#[test]
fn protocol_uses_clean_private_state_calls_no_tools_and_closes_the_process() {
    let dir = tempfile::tempdir().unwrap();
    let binary = fixture(dir.path(), "good");
    let mut opts = options(&binary);
    opts.connect = true;
    opts.expected_sha256 = Some(digest(&binary).unwrap().1);
    let report = run_in(&opts, dir.path());
    assert!(report.ok, "{report:?}");
    assert!(report.process_started);
    assert!(!report.desktop_verified);
    assert_eq!(report.state_directory_removed, Some(true));
    assert_eq!(report.protocol.as_ref().unwrap().tools, ["get_config"]);
    assert!(!serde_json::to_string(&report)
        .unwrap()
        .contains("IGNORE THIS"));
    let wire: Vec<Value> = std::fs::read_to_string(dir.path().join("wire.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        wire.iter()
            .filter_map(|v| v["method"].as_str())
            .collect::<Vec<_>>(),
        ["initialize", "notifications/initialized", "tools/list"]
    );
    assert!(!Path::new(wire[0]["cwd"].as_str().unwrap()).exists());
    let pid = wire[0]["pid"].as_i64().unwrap() as libc::pid_t;
    assert_ne!(unsafe { libc::kill(pid, 0) }, 0, "provider still running");
    opts.expected_tools_sha256 = Some(report.protocol.unwrap().tools_sha256);
    assert!(run_in(&opts, dir.path()).ok);
    opts.expected_tools_sha256 = Some("0".repeat(64));
    assert_eq!(
        run_in(&opts, dir.path()).failure.unwrap().code,
        "PROVIDER_SCHEMA_CHANGED"
    );
}

#[cfg(unix)]
#[test]
fn version_catalog_timeout_and_eof_failures_all_close_and_remove_probe_state() {
    for (mode, code) in [
        ("version", "PROVIDER_IDENTITY_MISMATCH"),
        ("invalid", "PROVIDER_CATALOG_INVALID"),
        ("timeout", "PROBE_TIMEOUT"),
        ("closed", "PROBE_CLOSED"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let binary = fixture(dir.path(), mode);
        let mut opts = options(&binary);
        opts.connect = true;
        opts.expected_sha256 = Some(digest(&binary).unwrap().1);
        opts.timeout_ms = if mode == "timeout" { 300 } else { 2000 };
        let report = run_in(&opts, dir.path());
        assert_eq!(report.failure.as_ref().unwrap().code, code, "{report:?}");
        assert_eq!(report.state_directory_removed, Some(true), "{report:?}");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
        let wire = std::fs::read_to_string(dir.path().join("wire.jsonl")).unwrap();
        let first: Value = serde_json::from_str(wire.lines().next().unwrap()).unwrap();
        assert_ne!(
            unsafe { libc::kill(first["pid"].as_i64().unwrap() as libc::pid_t, 0) },
            0
        );
        if mode == "version" {
            assert!(!wire.contains("tools/list"));
        }
    }
}

#[cfg(unix)]
#[test]
fn redirected_state_is_reported_not_followed_during_cleanup() {
    let parent = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("keep"), "keep").unwrap();
    let dir = state::Directory::create(parent.path()).unwrap();
    std::fs::remove_dir_all(&dir.path).unwrap();
    std::os::unix::fs::symlink(outside.path(), &dir.path).unwrap();
    assert!(dir.remove().is_err());
    drop(dir);
    assert_eq!(
        std::fs::read_to_string(outside.path().join("keep")).unwrap(),
        "keep"
    );
}

#[cfg(unix)]
#[test]
fn tool_pagination_shares_the_handshake_deadline() {
    let dir = tempfile::tempdir().unwrap();
    let binary = fixture(dir.path(), "paged");
    let mut opts = options(&binary);
    opts.connect = true;
    opts.timeout_ms = 300;
    opts.expected_sha256 = Some(digest(&binary).unwrap().1);
    let report = run_in(&opts, dir.path());
    assert_eq!(report.failure.unwrap().code, "PROBE_TIMEOUT");
    assert_eq!(report.state_directory_removed, Some(true));
}
