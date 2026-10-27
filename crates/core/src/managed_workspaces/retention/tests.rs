use super::*;

#[test]
fn expiration_keeps_artifacts_pins_unknown_resources_and_other_owners() {
    let home = tempfile::tempdir().unwrap();
    let store = Store::new(home.path().into());
    let (expired, _) = store.start("a", "expired", "").unwrap();
    let (pinned, _) = store.start("a", "pinned", "").unwrap();
    let (unknown, _) = store.start("a", "unknown", "").unwrap();
    let (other, _) = store.start("b", "other", "").unwrap();
    fs::write(expired.root.join("result.txt"), "kept").unwrap();
    let artifact = {
        let mut op = store.operation("a", &expired.id).unwrap();
        op.preserve(&["result.txt".into()]).unwrap().remove(0)
    };
    store
        .operation("a", &pinned.id)
        .unwrap()
        .set_retention(None)
        .unwrap();
    store
        .operation("a", &unknown.id)
        .unwrap()
        .begin_command()
        .unwrap();
    let report = store.sweep_at("a", None, 16, None, u64::MAX).unwrap();
    assert_eq!(report["removed"], json!([expired.id]));
    assert_eq!(report["blocked"].as_array().unwrap().len(), 1);
    assert!(!expired.root.exists());
    assert!(pinned.root.exists());
    assert!(unknown.root.exists());
    assert!(other.root.exists());
    assert_eq!(
        store
            .operation("a", &expired.id)
            .unwrap()
            .artifact(&artifact.id, true)
            .unwrap()["content"],
        "kept"
    );
    assert!(
        store.sweep_at("a", None, 16, None, u64::MAX).unwrap()["removed"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn sweeps_are_bounded_and_can_advance_past_blocked_entries() {
    let home = tempfile::tempdir().unwrap();
    let store = Store::new(home.path().into());
    for i in 0..3 {
        store.start("a", &format!("{i}"), "").unwrap();
    }
    let first = store.sweep_at("a", None, 1, None, u64::MAX).unwrap();
    assert_eq!(first["examined"], 1);
    let second = store
        .sweep_at("a", first["next_cursor"].as_str(), 1, None, u64::MAX)
        .unwrap();
    assert_eq!(second["examined"], 1);
    assert_ne!(first["removed"][0], second["removed"][0]);
    assert!(store.sweep_at("a", None, 0, None, u64::MAX).is_err());
    assert!(store
        .sweep_at("a", Some("../x"), 1, None, u64::MAX)
        .is_err());
}

#[test]
fn touching_or_retaining_changes_the_deadline_but_status_does_not() {
    let home = tempfile::tempdir().unwrap();
    let store = Store::new(home.path().into());
    let (record, _) = store.start("a", "one", "").unwrap();
    assert!(store.sweep("a", None, 16, None).unwrap()["removed"]
        .as_array()
        .unwrap()
        .is_empty());
    let mut op = store.operation("a", &record.id).unwrap();
    op.set_retention(Some(24)).unwrap();
    assert_eq!(op.record.idle_retention_ms, Some(24 * 60 * 60 * 1000));
    assert_eq!(
        op.record.expires_at_ms,
        store.inspect("a", &record.id).unwrap().expires_at_ms
    );
    op.record.expires_at_ms = Some(1);
    op.persist().unwrap();
    op.touch().unwrap();
    assert!(op.record.expires_at_ms.unwrap() > now_ms());
    assert!(op.set_retention(Some(0)).is_err());
    assert!(op.set_retention(Some(8761)).is_err());
}
