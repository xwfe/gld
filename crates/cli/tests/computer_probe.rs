//! 本机预检在 Backend 之前执行：缺执行器不会启动 daemon 或改服务配置。
mod common;

use common::env::Env;
use serde_json::Value;

#[test]
fn missing_binary_returns_json_without_starting_or_configuring_the_service() {
    let env = Env::new();
    let missing = env.project.path().join("no-such-driver");
    let output = env.gld(&[
        "--json",
        "--no-autostart",
        "computer",
        "probe",
        "--binary",
        missing.to_str().unwrap(),
    ]);
    assert_eq!(output.status.code(), Some(1));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["failure"]["code"], "PROVIDER_NOT_FOUND");
    assert_eq!(report["process_started"], false);
    assert_eq!(report["desktop_verified"], false);
    assert_eq!(std::fs::read_dir(env.home.path()).unwrap().count(), 0);
}

#[cfg(unix)]
#[test]
fn cli_inspects_then_probes_explicitly_without_inheriting_secrets_or_enabling_mcp() {
    let mut env = Env::new();
    let marker = env.project.path().join("methods.txt");
    let marker_json = serde_json::to_string(marker.to_str().unwrap()).unwrap();
    env.fake_binary("cua-driver", &format!(r##"#!/usr/bin/env python3
import sys,os,json
assert sys.argv[1:]==['mcp','--direct']
assert 'UNRELATED_API_KEY' not in os.environ
assert 'GLD_HOME' not in os.environ
assert os.environ['HOME']==os.getcwd()
assert os.environ['CUA_DRIVER_RS_TELEMETRY_ENABLED']=='false'
for line in sys.stdin:
    q=json.loads(line); method=q.get('method')
    with open({marker_json},'a') as f: f.write(method+'\n')
    if method=='initialize': r={{'protocolVersion':'2025-06-18','serverInfo':{{'name':'cua-driver','version':'0.38.0'}},'capabilities':{{'tools':{{}}}}}}
    elif method=='notifications/initialized': continue
    elif method=='tools/list': r={{'tools':[{{'name':'observe','inputSchema':{{'type':'object','properties':{{}}}}}}]}}
    else: raise AssertionError(method)
    print(json.dumps({{'jsonrpc':'2.0','id':q['id'],'result':r}}),flush=True)
"##));
    let inspected = env.json(&["--json", "--no-autostart", "computer", "probe"]);
    assert_eq!(inspected["mode"], "inspect");
    assert_eq!(inspected["process_started"], false);
    assert!(!marker.exists());
    let sha = inspected["binary"]["sha256"].as_str().unwrap();
    let rejected = env.gld(&[
        "--json",
        "computer",
        "probe",
        "--connect",
        "--sha256",
        &"0".repeat(64),
    ]);
    assert!(!rejected.status.success());
    assert!(!marker.exists());
    let result = env.gld_with_env(
        &[
            "--json",
            "--no-autostart",
            "computer",
            "probe",
            "--connect",
            "--sha256",
            sha,
        ],
        &[("UNRELATED_API_KEY", "fake-secret-only-in-parent")],
    );
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let report: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(report["ok"], true);
    assert_eq!(report["desktop_verified"], false);
    assert_eq!(report["state_directory_removed"], true);
    assert_eq!(report["protocol"]["tools"][0], "observe");
    assert_eq!(
        std::fs::read_to_string(marker).unwrap(),
        "initialize\nnotifications/initialized\ntools/list\n"
    );
    assert!(
        !env.home.path().join("data").exists(),
        "must not register providers or create settings"
    );
    assert_eq!(
        std::fs::read_dir(env.home.path()).unwrap().count(),
        1,
        "only fake-bin fixture may exist"
    );
}
