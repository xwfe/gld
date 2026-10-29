use super::*;

fn config() -> NodeConfig {
    NodeConfig {
        hub_url: "https://example.invalid/hub".into(),
        hub_id: "a".repeat(32),
        machine_id: "b".repeat(32),
        credential: "c".repeat(64),
        workspaces: vec!["d".repeat(32)],
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
