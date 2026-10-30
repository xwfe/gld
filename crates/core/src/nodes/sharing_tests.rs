use super::*;

use crate::workspace::WorkspaceProfile;

const TEST_NAME: &str =
    "nodes::sharing::tests::sharing_preserves_identity_and_requires_the_running_node_to_stop";
const CHILD: &str = "GLD_TEST_NODE_SHARING_CHILD";
const CREDENTIAL: &str = "isolated-node-sharing-test-secret-1234567890";

fn config(ids: Vec<String>) -> NodeConfig {
    NodeConfig {
        hub_url: "https://example.invalid/hub".into(),
        hub_id: "a".repeat(32),
        hub_public_key: "e".repeat(64),
        machine_id: "b".repeat(32),
        credential: CREDENTIAL.into(),
        workspaces: ids,
        writable_workspaces: Vec::new(),
        ca_certificate: None,
    }
}

#[test]
fn sharing_view_keeps_missing_projects_visible_without_disclosing_secrets() {
    let mut data = AppData::default();
    let mut available = WorkspaceProfile::new("selected-project".into(), Some("api".into()));
    available.id = "c".repeat(32);
    let mut unavailable = WorkspaceProfile::new("unconfined-project".into(), Some("web".into()));
    unavailable.id = "d".repeat(32);
    unavailable.runtime.confine_reads = false;
    data.profiles = vec![available, unavailable];
    let mut config = config(vec!["c".repeat(32), "d".repeat(32), "e".repeat(32)]);
    config.ca_certificate = Some(PathBuf::from("private-ca-file-marker"));
    let listed = view(&config, Path::new("node.json"), catalog(&data, &config));
    assert_eq!(listed.workspaces.len(), 3);
    assert!(listed.workspaces[0].available);
    assert!(!listed.workspaces[1].available);
    assert!(!listed.workspaces[2].available);
    assert!(listed.workspaces[2].name.is_none());
    assert!(listed
        .workspaces
        .iter()
        .all(|workspace| workspace.read_only));
    let output = serde_json::to_string(&listed).unwrap();
    assert!(!output.contains(CREDENTIAL));
    assert!(!output.contains("credential"));
    assert!(!output.contains("private-ca-file-marker"));

    config.writable_workspaces = config.workspaces.clone();
    let writable = catalog(&data, &config);
    assert!(!writable[0].read_only);
    assert!(writable[1].read_only);
    assert!(writable[2].read_only);
    data.profiles[0].runtime.tool_profile = "read-only".into();
    assert!(catalog(&data, &config)[0].read_only);
}

#[test]
fn sharing_preserves_identity_and_requires_the_running_node_to_stop() {
    if std::env::var_os(CHILD).is_some() {
        exercise_sharing();
        return;
    }
    // 每个子进程独享 GLD_HOME，不修改并行单元测试进程的全局环境。
    let isolated = tempfile::tempdir().unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", TEST_NAME, "--nocapture"])
        .env(CHILD, "1")
        .env("GLD_HOME", isolated.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "isolated sharing regression failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("1 passed; 0 failed"),
        "isolated sharing test did not run"
    );
}

fn exercise_sharing() {
    let root = tempfile::tempdir().unwrap();
    let api = WorkspaceProfile::new(
        root.path().join("api").to_string_lossy().into_owned(),
        Some("api".into()),
    );
    let web = WorkspaceProfile::new(
        root.path().join("web").to_string_lossy().into_owned(),
        Some("web".into()),
    );
    std::fs::create_dir_all(&api.path).unwrap();
    std::fs::create_dir_all(&web.path).unwrap();
    let containing_config = WorkspaceProfile::new(
        root.path().to_string_lossy().into_owned(),
        Some("config-container".into()),
    );
    DataStore::update_file(|data| {
        data.profiles = vec![api.clone(), web.clone(), containing_config];
        Ok(())
    })
    .unwrap();
    let path = root.path().join("node.json");
    let mut original = config(vec![api.id.clone(), web.id.clone()]);
    original.ca_certificate = Some(root.path().join("ca.der"));
    let storage = LocalConfig::lock(&path).unwrap();
    storage.save_pending(&original).unwrap();
    storage.finish(&original).unwrap();
    let saved = std::fs::read(&path).unwrap();

    assert_eq!(
        show(&path).unwrap().workspaces.len(),
        2,
        "view is available while the node holds its lock"
    );
    assert!(replace(&path, &["web".into()], &["web".into()]).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), saved);
    drop(storage);

    let updated = replace(&path, &["web".into()], &["web".into()]).unwrap();
    assert_eq!(updated.workspaces.len(), 1);
    assert_eq!(updated.workspaces[0].id, web.id);
    assert!(!updated.workspaces[0].read_only);
    assert!(!show(&path).unwrap().workspaces[0].read_only);
    assert!(!serde_json::to_string(&updated)
        .unwrap()
        .contains(CREDENTIAL));
    let stored = read_config(&path).unwrap();
    assert_eq!(stored.hub_url, original.hub_url);
    assert_eq!(stored.hub_id, original.hub_id);
    assert_eq!(stored.machine_id, original.machine_id);
    assert_eq!(stored.credential, original.credential);
    assert_eq!(stored.ca_certificate, original.ca_certificate);
    assert_eq!(stored.workspaces, vec![web.id.clone()]);
    assert_eq!(stored.writable_workspaces, vec![web.id.clone()]);

    let saved = std::fs::read(&path).unwrap();
    for invalid in [
        vec![],
        vec!["unknown".into()],
        vec!["web".into(), web.id.clone()],
        vec!["config-container".into()],
    ] {
        assert!(replace(&path, &invalid, &[]).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), saved);
    }
    for invalid in [
        vec!["unknown".into()],
        vec!["api".into()],
        vec!["web".into(), web.id.clone()],
    ] {
        assert!(replace(&path, &["web".into()], &invalid).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), saved);
    }
    DataStore::update_file(|data| {
        data.profiles[1].runtime.tool_profile = "read-only".into();
        Ok(())
    })
    .unwrap();
    assert!(show(&path).unwrap().workspaces[0].read_only);
    assert!(replace(&path, &["web".into()], &["web".into()]).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), saved);
    DataStore::update_file(|data| {
        data.profiles[1].runtime.tool_profile = "core".into();
        Ok(())
    })
    .unwrap();

    let expanded = replace(&path, &["api".into(), "web".into()], &["web".into()]).unwrap();
    assert!(expanded.workspaces[0].read_only);
    assert!(!expanded.workspaces[1].read_only);
    let revoked = replace(&path, &["web".into()], &[]).unwrap();
    assert!(revoked.workspaces[0].read_only);
    assert!(read_config(&path).unwrap().writable_workspaces.is_empty());
    let saved = std::fs::read(&path).unwrap();
    let stored: serde_json::Value = serde_json::from_slice(&saved).unwrap();
    assert!(stored.get("writable_workspaces").is_none());
    DataStore::update_file(|data| {
        data.profiles[0].runtime.confine_reads = false;
        Ok(())
    })
    .unwrap();
    assert!(replace(&path, &[api.id], &[]).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), saved);
    DataStore::update_file(|data| {
        data.profiles.retain(|profile| profile.id != web.id);
        Ok(())
    })
    .unwrap();
    let missing = show(&path).unwrap();
    assert_eq!(missing.workspaces.len(), 1);
    assert_eq!(missing.workspaces[0].id, web.id);
    assert!(!missing.workspaces[0].available);
}
