use super::*;
use serde_json::json;

const HUB: &str = "11111111111111111111111111111111";
const MACHINE: &str = "22222222222222222222222222222222";
const PROJECT: &str = "33333333333333333333333333333333";
const TOKEN: &str = "synthetic-node-credential-0000000000000001";
const OTHER_TOKEN: &str = "synthetic-node-credential-0000000000000002";

fn config(url: &str) -> NodeConfig {
    NodeConfig {
        hub_url: url.into(),
        hub_id: HUB.into(),
        machine_id: MACHINE.into(),
        credential: TOKEN.into(),
        workspaces: vec![PROJECT.into()],
        writable_workspaces: Vec::new(),
        ca_certificate: None,
    }
}

#[test]
fn registration_preserves_old_data_and_never_serializes_the_credential() {
    let mut data: AppData = serde_json::from_value(json!({})).unwrap();
    validate_registry(&data).unwrap();
    let registered = register_in(&mut data, "test-machine", Some(MACHINE), TOKEN).unwrap();
    assert!(valid_id(&registered.hub_id));
    assert_eq!(registered.machine_id, MACHINE);
    assert_eq!(
        data.node_machines[0].credential_sha256,
        credential_hash(TOKEN)
    );
    assert!(!serde_json::to_string(&data).unwrap().contains(TOKEN));
    assert!(!serde_json::to_string(&registered)
        .unwrap()
        .contains("credential"));
    assert!(!format!("{:?}", data.node_machines[0]).contains(&credential_hash(TOKEN)));
    assert!(!format!("{:?}", config("https://example.invalid")).contains(TOKEN));
    validate_registry(&data).unwrap();
}

#[test]
fn duplicate_registration_does_not_change_the_existing_identity_or_projects() {
    let mut data = AppData::default();
    register_in(&mut data, "first", Some(MACHINE), TOKEN).unwrap();
    data.node_machines[0].projects.push(NodeProject {
        id: PROJECT.into(),
        name: "api".into(),
        tool_profile: "read-only".into(),
    });
    let before = serde_json::to_value(&data).unwrap();
    for (name, id, credential) in [
        ("second", MACHINE, OTHER_TOKEN),
        ("FIRST", HUB, OTHER_TOKEN),
        ("second", HUB, TOKEN),
    ] {
        assert!(register_in(&mut data, name, Some(id), credential).is_err());
        assert_eq!(serde_json::to_value(&data).unwrap(), before);
    }
}

#[test]
fn node_credentials_are_distinct_from_existing_service_credentials() {
    let mut data = AppData::default();
    data.shared_secrets
        .insert("bearer_token".into(), TOKEN.into());
    assert!(register_in(&mut data, "node", Some(MACHINE), TOKEN).is_err());
    assert!(data.node_hub_id.is_empty());
    assert!(data.node_machines.is_empty());
    assert!(register_in(&mut data, "node", Some(MACHINE), OTHER_TOKEN).is_ok());
}

#[test]
fn node_urls_preserve_prefixes_and_refuse_credential_or_insecure_remote_urls() {
    assert_eq!(
        config("https://example.invalid/hub")
            .websocket_url()
            .unwrap(),
        format!("wss://example.invalid/hub/nodes/v1/connect/{MACHINE}")
    );
    assert_eq!(
        config("wss://example.invalid/prefix/")
            .websocket_url()
            .unwrap(),
        format!("wss://example.invalid/prefix/nodes/v1/connect/{MACHINE}")
    );
    for url in ["http://127.0.0.1:1234/hub", "ws://[::1]:1234"] {
        config(url).validate().unwrap();
    }
    for url in [
        "http://example.invalid",
        "ws://localhost:1234",
        "ftp://example.invalid",
        "https://user:secret@example.invalid",
        "https://example.invalid?token=secret",
        "https://example.invalid/#secret",
    ] {
        assert!(config(url).validate().is_err(), "{url}");
    }
}

#[test]
fn node_configuration_requires_explicit_unique_full_project_ids() {
    let mut value = config("https://example.invalid");
    value.workspaces.clear();
    assert!(value.validate().is_err());
    value.workspaces = vec![PROJECT.into(), PROJECT.into()];
    assert!(value.validate().is_err());
    value.workspaces = vec!["api".into()];
    assert!(value.validate().is_err());
    value.workspaces = vec![PROJECT.into()];
    value.credential = "short".into();
    assert!(value.validate().is_err());
    value.credential = TOKEN.into();
    value.machine_id = "m".repeat(32);
    assert!(value.validate().is_err());
}

#[test]
fn old_configuration_defaults_to_read_only_and_omits_empty_write_permissions() {
    let old = json!({
        "hub_url": "https://example.invalid",
        "hub_id": HUB,
        "machine_id": MACHINE,
        "credential": TOKEN,
        "workspaces": [PROJECT],
        "ca_certificate": null
    });
    let mut loaded: NodeConfig = serde_json::from_value(old.clone()).unwrap();
    loaded.validate().unwrap();
    assert!(loaded.writable_workspaces.is_empty());
    assert_eq!(serde_json::to_value(&loaded).unwrap(), old);

    loaded.writable_workspaces = vec![PROJECT.into()];
    let saved = serde_json::to_value(&loaded).unwrap();
    assert_eq!(saved["writable_workspaces"], json!([PROJECT]));
    let restored: NodeConfig = serde_json::from_value(saved).unwrap();
    restored.validate().unwrap();
    assert_eq!(restored.writable_workspaces, vec![PROJECT]);
}

#[test]
fn file_write_permissions_require_a_unique_subset_of_shared_projects() {
    let mut value = config("https://example.invalid");
    value.workspaces.push(HUB.into());
    value.writable_workspaces = vec![PROJECT.into()];
    value.validate().unwrap();
    for invalid in [
        vec![PROJECT.into(), PROJECT.into()],
        vec![MACHINE.into()],
        vec!["api".into()],
    ] {
        value.writable_workspaces = invalid;
        assert!(value.validate().is_err());
    }
    value.writable_workspaces.clear();
    value.validate().unwrap();
}

#[test]
fn configuration_file_has_bounded_redacted_reads_and_relative_ca_resolution() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("node.json");
    let mut value = config("https://example.invalid");
    value.ca_certificate = Some(PathBuf::from("ca.der"));
    let mut file = super::super::local_config::create_private_file(&path).unwrap();
    std::io::Write::write_all(&mut file, &serde_json::to_vec(&value).unwrap()).unwrap();
    drop(file);
    let loaded = NodeConfig::load(&path).unwrap();
    assert_eq!(loaded.ca_certificate, Some(dir.path().join("ca.der")));
    std::fs::write(&path, format!("{{\"credential\":\"{TOKEN}\"")).unwrap();
    let error = NodeConfig::load(&path).unwrap_err();
    assert!(!error.contains(TOKEN));
    std::fs::write(&path, vec![b' '; MAX_CONFIG_BYTES as usize + 1]).unwrap();
    assert!(NodeConfig::load(&path).unwrap_err().contains("64 KiB"));
}

#[cfg(unix)]
#[test]
fn configuration_file_refuses_shared_permissions_and_symlinks() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("node.json");
    std::fs::write(
        &path,
        serde_json::to_vec(&config("https://example.invalid")).unwrap(),
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(NodeConfig::load(&path).is_err());
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    NodeConfig::load(&path).unwrap();
    let alias = dir.path().join("alias.json");
    symlink(&path, &alias).unwrap();
    assert!(NodeConfig::load(&alias).is_err());
    let fifo = dir.path().join("config.fifo");
    use std::os::unix::ffi::OsStrExt;
    let fifo_path = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo_path.as_ptr(), 0o600) }, 0);
    assert!(NodeConfig::load(&fifo).is_err());
}

fn registered_data() -> AppData {
    let mut data = AppData::default();
    register_in(&mut data, "first", Some(MACHINE), TOKEN).unwrap();
    data.node_machines[0].projects.push(NodeProject {
        id: PROJECT.into(),
        name: "api".into(),
        tool_profile: "read-only".into(),
    });
    data
}

#[test]
fn legacy_machine_records_default_to_active_without_losing_identity_or_projects() {
    let data = registered_data();
    let mut old = serde_json::to_value(&data).unwrap();
    old["node_machines"][0]
        .as_object_mut()
        .unwrap()
        .remove("revoked");
    old["node_machines"][0]
        .as_object_mut()
        .unwrap()
        .remove("credential_generation");
    let loaded: AppData = serde_json::from_value(old).unwrap();
    validate_registry(&loaded).unwrap();
    assert_eq!(loaded.node_machines, data.node_machines);
    assert_eq!(loaded.node_hub_id, data.node_hub_id);
    assert!(authenticate_in(&loaded, MACHINE, TOKEN).is_ok());
}

#[test]
fn revocation_is_idempotent_preserves_catalog_and_survives_reload() {
    let mut data = registered_data();
    let authenticated = authenticate_in(&data, MACHINE, TOKEN).unwrap();
    let mut expected = serde_json::to_value(&data).unwrap();
    expected["node_machines"][0]["revoked"] = json!(true);
    expected["node_machines"][0]["credential_generation"] = json!(1);
    let revoked = revoke_in(&mut data, MACHINE).unwrap();
    assert!(revoked.revoked);
    assert_eq!(revoked.hub_id, authenticated.hub_id);
    assert_eq!(revoked.machine_id, MACHINE);
    assert_eq!(revoked.name, "first");
    assert_eq!(serde_json::to_value(&data).unwrap(), expected);
    let mut reloaded: AppData = serde_json::from_value(expected.clone()).unwrap();
    assert_eq!(
        authenticate_in(&reloaded, MACHINE, TOKEN).unwrap_err(),
        AUTH_FAILED
    );
    assert!(!is_current_in(&reloaded, &authenticated));
    revoke_in(&mut reloaded, MACHINE).unwrap();
    assert_eq!(serde_json::to_value(&reloaded).unwrap(), expected);
    for id in ["first", HUB] {
        assert!(revoke_in(&mut reloaded, id).is_err());
        assert_eq!(serde_json::to_value(&reloaded).unwrap(), expected);
    }
    // 撤销保留凭据占用，避免把同一把钥匙转给另一台机器。
    assert!(register_in(&mut reloaded, "second", Some(HUB), TOKEN).is_err());
    assert_eq!(serde_json::to_value(&reloaded).unwrap(), expected);
}

#[test]
fn rotation_preserves_identity_and_catalog_and_can_restore_a_revoked_machine() {
    for revoked in [false, true] {
        let mut data = registered_data();
        let old = authenticate_in(&data, MACHINE, TOKEN).unwrap();
        if revoked {
            revoke_in(&mut data, MACHINE).unwrap();
        }
        let registered = rotate_in(&mut data, MACHINE, OTHER_TOKEN).unwrap();
        assert!(!registered.revoked);
        assert_eq!(registered.hub_id, old.hub_id);
        assert_eq!(registered.machine_id, old.machine.id);
        assert_eq!(registered.name, old.machine.name);
        assert_eq!(data.node_machines[0].projects, old.machine.projects);
        assert_eq!(
            data.node_machines[0].credential_generation,
            if revoked { 2 } else { 1 }
        );
        assert_eq!(data.node_machines.len(), 1);
        assert!(!is_current_in(&data, &old));
        assert_eq!(
            authenticate_in(&data, MACHINE, TOKEN).unwrap_err(),
            AUTH_FAILED
        );
        let fresh = authenticate_in(&data, MACHINE, OTHER_TOKEN).unwrap();
        assert!(is_current_in(&data, &fresh));
        let stored = serde_json::to_string(&data).unwrap();
        assert!(!stored.contains(TOKEN));
        assert!(!stored.contains(OTHER_TOKEN));
        let response = serde_json::to_string(&registered).unwrap();
        assert!(!response.contains("credential"));
        assert!(!response.contains(OTHER_TOKEN));
    }
}

#[test]
fn rejected_rotation_never_changes_credentials_or_revocation() {
    let mut data = registered_data();
    register_in(&mut data, "second", Some(HUB), OTHER_TOKEN).unwrap();
    const SERVICE: &str = "synthetic-service-credential-0000000000000001";
    const APP: &str = "synthetic-app-credential-0000000000000000001";
    const WORKSPACE: &str = "synthetic-workspace-credential-000000000001";
    const GRANT_BEARER: &str = "synthetic-grant-bearer-0000000000000000001";
    const GRANT_OAUTH: &str = "synthetic-grant-oauth-00000000000000000001";
    data.shared_secrets
        .insert("bearer_token".into(), SERVICE.into());
    data.app_secrets
        .entry("hub".into())
        .or_default()
        .insert("oauth_password".into(), APP.into());
    data.workspace_secrets
        .entry(PROJECT.into())
        .or_default()
        .insert("bearer_token".into(), WORKSPACE.into());
    data.grants.push(crate::auth::GrantRecord {
        grant: crate::auth::Grant {
            id: "synthetic-grant".into(),
            name: "limited reader".into(),
            workspaces: vec![PROJECT.into()],
            read_only: true,
            browser: false,
        },
        oauth_password: GRANT_OAUTH.into(),
        bearer_token: GRANT_BEARER.into(),
        created_at: String::new(),
        browser: None,
    });
    revoke_in(&mut data, MACHINE).unwrap();
    let before = serde_json::to_value(&data).unwrap();
    for token in [
        "short",
        TOKEN,
        OTHER_TOKEN,
        SERVICE,
        APP,
        WORKSPACE,
        GRANT_BEARER,
        GRANT_OAUTH,
    ] {
        let error = rotate_in(&mut data, MACHINE, token).unwrap_err();
        assert!(!error.contains(token));
        assert_eq!(serde_json::to_value(&data).unwrap(), before);
    }
    for id in ["first", PROJECT] {
        assert!(rotate_in(&mut data, id, "synthetic-unused-credential-00000000000001").is_err());
        assert_eq!(serde_json::to_value(&data).unwrap(), before);
    }
}

#[test]
fn stale_discovery_cannot_overwrite_a_rotated_or_revoked_project_catalog() {
    let mut data = registered_data();
    let old = authenticate_in(&data, MACHINE, TOKEN).unwrap();
    let projects = vec![NodeProject {
        id: PROJECT.into(),
        name: "renamed-api".into(),
        tool_profile: "read-only".into(),
    }];
    rotate_in(&mut data, MACHINE, OTHER_TOKEN).unwrap();
    let before = serde_json::to_value(&data).unwrap();
    assert!(update_projects_in(&mut data, &old, &projects).is_err());
    assert_eq!(serde_json::to_value(&data).unwrap(), before);
    let fresh = authenticate_in(&data, MACHINE, OTHER_TOKEN).unwrap();
    let mut other_hub = fresh.clone();
    other_hub.hub_id = "f".repeat(32);
    assert!(!is_current_in(&data, &other_hub));
    assert!(update_projects_in(&mut data, &other_hub, &projects).is_err());
    assert_eq!(serde_json::to_value(&data).unwrap(), before);
    update_projects_in(&mut data, &fresh, &projects).unwrap();
    assert_eq!(data.node_machines[0].projects, projects);
    revoke_in(&mut data, MACHINE).unwrap();
    let revoked = serde_json::to_value(&data).unwrap();
    assert!(update_projects_in(&mut data, &fresh, &[]).is_err());
    assert_eq!(serde_json::to_value(&data).unwrap(), revoked);
}

#[test]
fn rotating_back_to_an_earlier_credential_does_not_revive_old_authentication() {
    let mut data = registered_data();
    let stale = authenticate_in(&data, MACHINE, TOKEN).unwrap();
    rotate_in(&mut data, MACHINE, OTHER_TOKEN).unwrap();
    rotate_in(&mut data, MACHINE, TOKEN).unwrap();
    assert_eq!(data.node_machines[0].credential_generation, 2);
    assert_eq!(
        data.node_machines[0].credential_sha256,
        stale.machine.credential_sha256
    );
    assert!(!is_current_in(&data, &stale));
    let before = serde_json::to_value(&data).unwrap();
    assert!(update_projects_in(&mut data, &stale, &[]).is_err());
    assert_eq!(serde_json::to_value(&data).unwrap(), before);
    let fresh = authenticate_in(&data, MACHINE, TOKEN).unwrap();
    assert!(is_current_in(&data, &fresh));
    update_projects_in(&mut data, &fresh, &[]).unwrap();
    assert!(data.node_machines[0].projects.is_empty());
}

#[test]
fn credential_generation_never_wraps_or_partially_changes_a_record() {
    let mut data = registered_data();
    data.node_machines[0].credential_generation = u64::MAX;
    let before = serde_json::to_value(&data).unwrap();
    assert!(rotate_in(&mut data, MACHINE, OTHER_TOKEN).is_err());
    assert_eq!(serde_json::to_value(&data).unwrap(), before);
    assert!(revoke_in(&mut data, MACHINE).is_err());
    assert_eq!(serde_json::to_value(&data).unwrap(), before);
}
