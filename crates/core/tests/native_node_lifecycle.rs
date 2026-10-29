//! 凭据变更夹在认证、预约和目录写回之间时，旧代次不能覆盖或释放新登记。
//! 数据路径按进程缓存；用独立子进程隔离 GLD_HOME，不改并行测试进程的环境。

use std::process::{Command, Stdio};
use std::sync::Arc;

use gld_core::nodes::config::{self, AuthenticatedMachine, NodeProject};
use gld_core::nodes::registry::Nodes;

const CHILD: &str = "GLD_TEST_NODE_REGISTRY_CHILD";
const TEST: &str = "stale_pending_reservations_cannot_survive_credential_changes";
const MACHINE: &str = "22222222222222222222222222222222";
const PROJECT: &str = "33333333333333333333333333333333";
const TOKEN: &str = "synthetic-node-credential-0000000000000001";
const SECOND_TOKEN: &str = "synthetic-node-credential-0000000000000002";
const THIRD_TOKEN: &str = "synthetic-node-credential-0000000000000003";

#[test]
fn stale_pending_reservations_cannot_survive_credential_changes() {
    if std::env::var(CHILD).as_deref() != Ok("1") {
        let home = tempfile::tempdir().expect("isolated node registry home");
        let output = Command::new(std::env::current_exe().expect("test executable"))
            .args(["--exact", TEST, "--nocapture"])
            .env("GLD_HOME", home.path())
            .env(CHILD, "1")
            .stdin(Stdio::null())
            .output()
            .expect("start isolated node registry test");
        assert!(
            output.status.success(),
            "isolated node registry test failed: {}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).contains(TEST),
            "isolated child did not run the selected lifecycle test"
        );
        return;
    }

    let registered = config::register_machine("test-machine", Some(MACHINE), TOKEN).unwrap();
    let old_auth = config::authenticate(MACHINE, TOKEN).unwrap();
    let original_projects = vec![NodeProject {
        id: PROJECT.into(),
        name: "api".into(),
        tool_profile: "read-only".into(),
    }];
    config::update_projects(&old_auth, &original_projects).unwrap();
    let nodes = Arc::new(Nodes::new());
    let old_reservation = nodes.reserve(old_auth.clone()).unwrap();
    let original_handle = nodes.members().unwrap()[0].id.clone();

    config::rotate_credential(MACHINE, SECOND_TOKEN).unwrap();
    assert!(!config::is_current(&old_auth).unwrap());
    expect_reservation_error(&nodes, &old_auth, "INVALID_NODE_CREDENTIALS");
    assert!(config::update_projects(&old_auth, &[]).is_err());
    assert_eq!(config::snapshot().unwrap().1[0].projects, original_projects);
    let fresh_auth = config::authenticate(MACHINE, SECOND_TOKEN).unwrap();
    expect_reservation_error(&nodes, &fresh_auth, "MACHINE_ALREADY_CONNECTED");
    nodes.close_stale(MACHINE).unwrap();
    let fresh_reservation = nodes.reserve(fresh_auth.clone()).unwrap();

    // 旧预约到现在才释放，不能把同机器的新代次从登记表中删掉。
    drop(old_reservation);
    expect_reservation_error(&nodes, &fresh_auth, "MACHINE_ALREADY_CONNECTED");
    nodes.close_stale(MACHINE).unwrap();
    expect_reservation_error(&nodes, &fresh_auth, "MACHINE_ALREADY_CONNECTED");
    let current_projects = vec![NodeProject {
        id: PROJECT.into(),
        name: "renamed-api".into(),
        tool_profile: "read-only".into(),
    }];
    config::update_projects(&fresh_auth, &current_projects).unwrap();
    assert_eq!(config::snapshot().unwrap().1[0].projects, current_projects);
    assert_eq!(nodes.members().unwrap()[0].id, original_handle);

    config::revoke_machine(MACHINE).unwrap();
    assert!(config::authenticate(MACHINE, SECOND_TOKEN).is_err());
    assert!(!config::is_current(&fresh_auth).unwrap());
    expect_reservation_error(&nodes, &fresh_auth, "INVALID_NODE_CREDENTIALS");
    assert!(config::update_projects(&fresh_auth, &[]).is_err());
    let revoked = nodes.members().unwrap();
    assert_eq!(revoked[0].id, original_handle);
    assert!(revoked[0].revoked);
    assert!(!revoked[0].online);
    nodes.close_stale(MACHINE).unwrap();

    let restored = config::rotate_credential(MACHINE, THIRD_TOKEN).unwrap();
    assert_eq!(restored.hub_id, registered.hub_id);
    assert_eq!(restored.machine_id, registered.machine_id);
    assert!(!restored.revoked);
    let latest_auth = config::authenticate(MACHINE, THIRD_TOKEN).unwrap();
    let latest_reservation = nodes.reserve(latest_auth.clone()).unwrap();
    drop(fresh_reservation);
    expect_reservation_error(&nodes, &latest_auth, "MACHINE_ALREADY_CONNECTED");
    nodes.close_stale(MACHINE).unwrap();
    expect_reservation_error(&nodes, &latest_auth, "MACHINE_ALREADY_CONNECTED");
    let (hub_id, machines) = config::snapshot().unwrap();
    assert_eq!(hub_id, registered.hub_id);
    assert_eq!(machines.len(), 1);
    assert_eq!(machines[0].projects, current_projects);
    assert_eq!(nodes.members().unwrap()[0].id, original_handle);

    // 当前预约正常释放后，可重新占用；没有残留预约或误删新代次。
    drop(latest_reservation);
    let reservation = nodes.reserve(latest_auth).unwrap();
    drop(reservation);
}

fn expect_reservation_error(nodes: &Arc<Nodes>, auth: &AuthenticatedMachine, expected: &str) {
    match nodes.reserve(auth.clone()) {
        Err(error) => assert_eq!(error, expected),
        Ok(_) => panic!("expected reservation error {expected}"),
    }
}
