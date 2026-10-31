use super::*;

fn config() -> NodeConfig {
    NodeConfig {
        hub_url: "https://example.invalid/hub".into(),
        hub_id: "a".repeat(32),
        hub_public_key: "e".repeat(64),
        machine_id: "b".repeat(32),
        credential: "c".repeat(64),
        workspaces: vec!["d".repeat(32)],
        writable_workspaces: Vec::new(),
        runnable_workspaces: Vec::new(),
        ca_certificate: None,
    }
}

#[test]
fn pending_identity_survives_reopening_and_is_published_without_secret_changes() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("node.json");
    let expected = config();
    {
        let store = LocalConfig::lock(&path).unwrap();
        assert!(!store.exists().unwrap());
        store.save_pending(&expected).unwrap();
        assert!(!path.exists());
        store.save_pending(&expected).unwrap();
        let mut changed = expected.clone();
        changed.credential = "e".repeat(64);
        assert!(store.save_pending(&changed).is_err());
    }
    let store = LocalConfig::lock(&path).unwrap();
    assert!(same_config(
        &store.load_pending().unwrap().unwrap(),
        &expected
    ));
    store.finish(&expected).unwrap();
    assert!(same_config(&store.load().unwrap(), &expected));
    assert!(store.load_pending().unwrap().is_none());
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_eq!(fs::metadata(path).unwrap().mode() & 0o777, 0o600);
    }
}

#[test]
fn pending_file_write_scope_cannot_change_before_publication() {
    for writable in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("node.json");
        let store = LocalConfig::lock(&path).unwrap();
        let mut original = config();
        if writable {
            original.writable_workspaces = original.workspaces.clone();
        }
        store.save_pending(&original).unwrap();
        let saved = fs::read(&store.pending).unwrap();
        let legacy: serde_json::Value = serde_json::from_slice(&saved).unwrap();
        assert_eq!(legacy.get("writable_workspaces").is_some(), writable);
        let mut changed = original.clone();
        changed.writable_workspaces = if writable {
            Vec::new()
        } else {
            original.workspaces.clone()
        };
        assert!(store.save_pending(&changed).is_err());
        assert!(store.finish(&changed).is_err());
        assert!(!path.exists());
        assert_eq!(fs::read(&store.pending).unwrap(), saved);
        store.finish(&original).unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(loaded.writable_workspaces, original.writable_workspaces);
        assert_eq!(fs::read(&path).unwrap(), saved);
    }
}

#[test]
fn configuration_locations_require_existing_project_directories_and_stay_outside_them() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    let nested = project.join("nested");
    fs::create_dir_all(&nested).unwrap();
    let projects = vec![project.to_string_lossy().into_owned()];
    validate_config_location(&root.path().join("node.json"), &projects).unwrap();
    for path in [project.join("node.json"), nested.join("node.json")] {
        assert!(validate_config_location(&path, &projects)
            .unwrap_err()
            .contains("outside every shared project"));
    }
    let missing = root.path().join("missing");
    assert!(validate_config_location(
        &root.path().join("node.json"),
        &[missing.to_string_lossy().into_owned()],
    )
    .is_err());
    let file = root.path().join("file");
    fs::write(&file, "not a project directory").unwrap();
    assert!(validate_config_location(
        &root.path().join("node.json"),
        &[file.to_string_lossy().into_owned()],
    )
    .is_err());
}

#[test]
fn shared_roots_cannot_contain_or_live_inside_the_gld_control_data_directory() {
    let root = tempfile::tempdir().unwrap();
    let control = root.path().join("control");
    let inside = control.join("project");
    let separate = root.path().join("separate-project");
    fs::create_dir_all(&inside).unwrap();
    fs::create_dir(&separate).unwrap();
    let config = root.path().join("node.json");
    for shared in [root.path(), control.as_path(), inside.as_path()] {
        assert!(validate_config_location_in(
            &config,
            &[shared.to_string_lossy().into_owned()],
            &control,
        )
        .unwrap_err()
        .contains("control data directory"));
    }
    validate_config_location_in(
        &config,
        &[separate.to_string_lossy().into_owned()],
        &control,
    )
    .unwrap();
    assert!(validate_config_location_in(
        &config,
        &[separate.to_string_lossy().into_owned()],
        &root.path().join("missing-control"),
    )
    .is_err());
}

#[cfg(unix)]
#[test]
fn configuration_location_checks_resolve_project_and_parent_directory_symlinks() {
    use std::os::unix::fs::symlink;
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    fs::create_dir(&project).unwrap();
    let alias = root.path().join("alias");
    symlink(&project, &alias).unwrap();
    for (config_parent, shared_root) in [(&project, &alias), (&alias, &project)] {
        assert!(validate_config_location(
            &config_parent.join("node.json"),
            &[shared_root.to_string_lossy().into_owned()],
        )
        .unwrap_err()
        .contains("outside every shared project"));
    }
    let config = root.path().join("node.json");
    for (shared_root, data_home) in [(&project, &alias), (&alias, &project)] {
        assert!(validate_config_location_in(
            &config,
            &[shared_root.to_string_lossy().into_owned()],
            data_home,
        )
        .unwrap_err()
        .contains("control data directory"));
    }
}

#[test]
fn finishing_never_overwrites_a_file_created_after_prepare() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("node.json");
    let store = LocalConfig::lock(&path).unwrap();
    let config = config();
    store.save_pending(&config).unwrap();
    fs::write(&path, "other configuration").unwrap();
    assert!(store.finish(&config).is_err());
    assert_eq!(fs::read_to_string(&path).unwrap(), "other configuration");
    assert!(same_config(
        &store.load_pending().unwrap().unwrap(),
        &config
    ));
}

#[test]
fn configuration_lock_is_exclusive_until_the_holder_exits() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("node.json");
    let first = LocalConfig::lock(&path).unwrap();
    assert!(LocalConfig::lock(&path).is_err());
    drop(first);
    LocalConfig::lock(&path).unwrap();
    assert!(root.path().join("node.json.lock").is_file());
}

#[test]
fn replacement_preserves_identity_and_invalid_input_leaves_original_bytes() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("node.json");
    let store = LocalConfig::lock(&path).unwrap();
    let mut config = config();
    store.save_pending(&config).unwrap();
    store.finish(&config).unwrap();
    config.workspaces.push("e".repeat(32));
    store.replace(&config).unwrap();
    assert!(same_config(&store.load().unwrap(), &config));
    let saved = fs::read(&path).unwrap();
    config.workspaces.clear();
    assert!(store.replace(&config).is_err());
    assert_eq!(fs::read(path).unwrap(), saved);
}

#[cfg(unix)]
#[test]
fn directory_aliases_share_the_lock_and_interrupted_publication_remains_readable() {
    use std::os::unix::fs::symlink;
    let root = tempfile::tempdir().unwrap();
    let actual = root.path().join("actual");
    fs::create_dir(&actual).unwrap();
    symlink(&actual, root.path().join("alias")).unwrap();
    let store = LocalConfig::lock(&actual.join("node.json")).unwrap();
    assert!(LocalConfig::lock(&root.path().join("alias/node.json")).is_err());
    store.save_pending(&config()).unwrap();
    // 模拟发布后、移除pending前退出；两个链接都保有私有权限，正式文件仍可运行。
    fs::hard_link(actual.join("node.json.pending"), actual.join("node.json")).unwrap();
    assert!(same_config(&store.load().unwrap(), &config()));
    fs::remove_file(actual.join("node.json")).unwrap();
    symlink(root.path().join("outside"), actual.join("node.json")).unwrap();
    assert!(store.load().is_err());
}

#[cfg(unix)]
#[test]
fn readable_by_other_accounts_or_symlink_lock_files_are_rejected() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("node.json");
    let store = LocalConfig::lock(&path).unwrap();
    store.save_pending(&config()).unwrap();
    store.finish(&config()).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(store.load().is_err());
    drop(store);
    let lock = root.path().join("node.json.lock");
    fs::remove_file(&lock).unwrap();
    symlink(&path, &lock).unwrap();
    assert!(LocalConfig::lock(&path).is_err());
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o644
    );
}

#[test]
fn the_default_location_is_found_only_when_it_is_unambiguous() {
    let dir = tempfile::tempdir().unwrap();
    let error = find_in(&dir.path().join("missing")).unwrap_err();
    assert!(error.contains("gld join"), "{error}");
    let error = find_in(dir.path()).unwrap_err();
    assert!(error.contains("gld join"), "{error}");

    // 锁和日志不算一份配置。
    for sidecar in ["a.json.lock", "a.json.log"] {
        fs::write(dir.path().join(sidecar), "").unwrap();
    }
    assert!(find_in(dir.path()).is_err());

    // 没配对完的算，gld leave 要能找到它；和正式配置同时在时只算一份。
    let first = dir.path().join("a.json");
    fs::write(dir.path().join("a.json.pending"), "").unwrap();
    assert_eq!(find_in(dir.path()).unwrap(), first);
    fs::write(&first, "{}").unwrap();
    assert_eq!(find_in(dir.path()).unwrap(), first);

    fs::write(dir.path().join("b.json"), "{}").unwrap();
    let error = find_in(dir.path()).unwrap_err();
    assert!(error.contains("--config"), "{error}");
    assert!(
        error.contains("a.json") && error.contains("b.json"),
        "{error}"
    );
}

#[test]
fn an_explicit_path_is_used_as_given() {
    let path = Path::new("somewhere/node.json");
    assert_eq!(find(Some(path)).unwrap(), path);
}
