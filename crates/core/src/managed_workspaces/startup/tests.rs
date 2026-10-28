use super::*;

fn expired(store: &Store, owner: &str, request: &str) -> Record {
    let (record, _) = store.start(owner, request, "").unwrap();
    let mut op = store.operation(owner, &record.id).unwrap();
    op.record.expires_at_ms = Some(1);
    op.persist().unwrap();
    op.record.clone()
}

#[test]
fn startup_checks_registered_owners_but_preserves_pins_unknown_and_legacy_resources() {
    let home = tempfile::tempdir().unwrap();
    let store = Store::new(home.path().into());
    let a = expired(&store, "owner-a", "one");
    let b = expired(&store, "owner-b", "two");
    fs::write(a.root.join("result.txt"), "saved").unwrap();
    let item = store
        .operation("owner-a", &a.id)
        .unwrap()
        .preserve(&["result.txt".into()])
        .unwrap()
        .remove(0);
    let pinned = expired(&store, "owner-a", "pinned");
    store
        .operation("owner-a", &pinned.id)
        .unwrap()
        .set_retention(None)
        .unwrap();
    let unknown = expired(&store, "owner-b", "unknown");
    store
        .operation("owner-b", &unknown.id)
        .unwrap()
        .begin_command()
        .unwrap();
    let legacy = expired(&store, "owner-b", "legacy");
    {
        let mut op = store.operation("owner-b", &legacy.id).unwrap();
        op.record.resource_tracking = false;
        op.persist().unwrap();
    }
    let (fresh, _) = store.start("owner-c", "fresh", "").unwrap();
    let unmanaged = home.path().join("work/manual");
    fs::create_dir(&unmanaged).unwrap();
    let report = run_once(&store, &AtomicBool::new(false), LIMIT).unwrap();
    assert_eq!((report.examined, report.removed, report.blocked), (4, 2, 2));
    assert!(!a.root.exists() && !b.root.exists());
    for path in [
        &pinned.root,
        &unknown.root,
        &legacy.root,
        &fresh.root,
        &unmanaged,
    ] {
        assert!(path.exists());
    }
    assert_eq!(
        store
            .operation("owner-a", &a.id)
            .unwrap()
            .artifact(&item.id, true)
            .unwrap()["content"],
        "saved"
    );
    let report_text = fs::read_to_string(home.path().join(REGISTRY).join(REPORT)).unwrap();
    assert!(!report_text.contains("owner-a") && !report_text.contains("result.txt"));
}

#[test]
fn bounded_sweeps_advance_beyond_blocked_items_and_wrap_on_later_launches() {
    let home = tempfile::tempdir().unwrap();
    let store = Store::new(home.path().into());
    for index in 0..4 {
        expired(&store, "owner", &index.to_string());
    }
    let records = store.live().unwrap();
    for record in &records[..2] {
        store
            .operation("owner", &record.id)
            .unwrap()
            .begin_command()
            .unwrap();
    }
    let stop = AtomicBool::new(false);
    let first = run_once(&store, &stop, 2).unwrap();
    assert_eq!((first.examined, first.blocked), (2, 2));
    let second = run_once(&store, &stop, 2).unwrap();
    assert_eq!((second.examined, second.removed), (2, 2));
    let third = run_once(&store, &stop, 2).unwrap();
    assert_eq!((third.examined, third.blocked), (2, 2));
    assert_eq!(first.cursor, third.cursor);
}

#[test]
fn cancelled_sweeps_do_not_touch_files_and_policy_gates_do_not_launch_workers() {
    let home = tempfile::tempdir().unwrap();
    let store = Store::new(home.path().into());
    let record = expired(&store, "owner", "one");
    let report = run_once(&store, &AtomicBool::new(true), LIMIT).unwrap();
    assert!(report.cancelled);
    assert_eq!(report.examined, 0);
    assert!(record.root.exists());
    assert!(!home.path().join(REGISTRY).join(REPORT).exists());
    let mut config = crate::settings::HubConfig::default();
    assert!(StartupSweep::launch(&config).is_none());
    config.work_sessions = true;
    config.auth_type = "noauth".into();
    assert!(StartupSweep::launch(&config).is_none());
    config.auth_type = "bearer".into();
    config.tool_profile = "read-only".into();
    assert!(StartupSweep::launch(&config).is_none());
}

#[test]
fn invalid_scan_state_keeps_directories_and_a_changed_deadline_is_rechecked() {
    let home = tempfile::tempdir().unwrap();
    let store = Store::new(home.path().into());
    let record = expired(&store, "owner", "one");
    let path = home.path().join(REGISTRY).join(REPORT);
    fs::write(&path, "invalid").unwrap();
    assert_eq!(
        run_once(&store, &AtomicBool::new(false), LIMIT)
            .unwrap_err()
            .code(),
        "WORK_SESSION_SWEEP_STATE_INVALID"
    );
    assert!(record.root.exists());
    fs::remove_file(path).unwrap();
    store
        .operation("owner", &record.id)
        .unwrap()
        .touch()
        .unwrap();
    assert!(!store.expire(&record, None, now_ms()).unwrap());
    assert!(record.root.exists());
}

#[cfg(unix)]
#[test]
fn linked_scan_state_does_not_read_or_overwrite_an_external_file() {
    let home = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let store = Store::new(home.path().into());
    let record = expired(&store, "owner", "one");
    let secret = outside.path().join("other.json");
    fs::write(&secret, "untouched").unwrap();
    std::os::unix::fs::symlink(&secret, home.path().join(REGISTRY).join(REPORT)).unwrap();
    assert_eq!(
        run_once(&store, &AtomicBool::new(false), LIMIT)
            .unwrap_err()
            .code(),
        "WORK_SESSION_PATH_INVALID"
    );
    assert!(record.root.exists());
    assert_eq!(fs::read_to_string(secret).unwrap(), "untouched");
}
