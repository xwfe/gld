use super::*;

fn closed(store: &Store, request: &str) -> Record {
    let (record, _) = store.start("owner", request, "").unwrap();
    store
        .operation("owner", &record.id)
        .unwrap()
        .finish(&[], None)
        .unwrap();
    store.inspect("owner", &record.id).unwrap()
}

#[test]
fn archived_results_keep_request_identity_ownership_and_artifacts() {
    let home = tempfile::tempdir().unwrap();
    let store = Store::new(home.path().into());
    let (record, _) = store.start("owner", "one", "").unwrap();
    fs::write(record.root.join("result.txt"), "saved").unwrap();
    let result = store
        .operation("owner", &record.id)
        .unwrap()
        .finish(&["result.txt".into()], None)
        .unwrap();
    assert!(!home
        .path()
        .join(REGISTRY)
        .join(format!("{}.json", record.id))
        .exists());
    assert!(store.live().unwrap().is_empty());
    assert_eq!(store.all().unwrap().len(), 1);
    let reopened = Store::new(home.path().into());
    let (again, created) = reopened.start("owner", "one", "").unwrap();
    assert!(!created);
    assert_eq!(again.id, record.id);
    assert_eq!(again.state, State::Closed);
    assert!(!record.root.exists());
    let id = result["artifacts"][0]["id"].as_str().unwrap();
    assert_eq!(
        reopened
            .operation("owner", &record.id)
            .unwrap()
            .artifact(id, true)
            .unwrap()["content"],
        "saved"
    );
    assert_eq!(
        reopened.inspect("other", &record.id).unwrap_err().code(),
        "WORK_SESSION_NOT_FOUND"
    );
    assert_ne!(reopened.start("other", "one", "").unwrap().0.id, record.id);
    assert_eq!(
        reopened
            .start("owner", "one", "changed")
            .unwrap_err()
            .code(),
        "WORK_SESSION_REQUEST_CONFLICT"
    );
}

#[test]
fn index_before_move_and_legacy_closed_records_resume_without_new_work() {
    let home = tempfile::tempdir().unwrap();
    let store = Store::new(home.path().into());
    let record = closed(&store, "one");
    let receipt = home
        .path()
        .join(REGISTRY)
        .join(CLOSED)
        .join(format!("{}.json", key("owner", "one")));
    let live = home
        .path()
        .join(REGISTRY)
        .join(format!("{}.json", record.id));
    // 索引已写、移动尚未发生：恢复该落盘状态，不借 sleep 猜时间。
    fs::rename(&receipt, &live).unwrap();
    assert_eq!(
        store.inspect("owner", &record.id).unwrap().state,
        State::Closed
    );
    let (again, created) = store.start("owner", "one", "").unwrap();
    assert!(!created);
    assert_eq!(again.id, record.id);
    assert!(receipt.exists());
    assert!(!live.exists());
    // 旧版本没有索引，重复 start 仍从完整回执恢复原身份。
    fs::remove_file(home.path().join(REGISTRY).join(IDS).join(&record.id)).unwrap();
    assert_eq!(store.start("owner", "one", "").unwrap().0.id, record.id);
    assert_eq!(store.inspect("owner", &record.id).unwrap().id, record.id);
}

#[test]
fn more_than_512_closed_receipts_do_not_consume_active_slots() {
    let home = tempfile::tempdir().unwrap();
    let store = Store::new(home.path().into());
    let template = closed(&store, "template");
    let canonical = home.path().canonicalize().unwrap();
    for i in 0..MAX_ACTIVE {
        let mut record = template.clone();
        record.id = format!("work-{}", Uuid::new_v4().simple());
        record.root = canonical.join("work").join(&record.id);
        record.client_request_id = format!("old-{i}");
        save(&canonical, &record).unwrap();
    }
    assert_eq!(store.all().unwrap().len(), 513);
    assert!(store.live().unwrap().is_empty());
    assert!(store.start("owner", "new", "").unwrap().1);
    assert_eq!(store.live().unwrap().len(), 1);
    assert_eq!(
        store.start("owner", "old-0", "").unwrap().0.state,
        State::Closed
    );
}

#[test]
fn corrupt_indices_and_conflicting_receipts_are_not_overwritten() {
    let home = tempfile::tempdir().unwrap();
    let store = Store::new(home.path().into());
    let record = closed(&store, "one");
    let canonical = home.path().canonicalize().unwrap();
    let pointer = canonical.join(REGISTRY).join(IDS).join(&record.id);
    fs::write(&pointer, "../secrets").unwrap();
    assert_eq!(
        store.inspect("owner", &record.id).unwrap_err().code(),
        "WORK_SESSION_ARCHIVE_INVALID"
    );
    assert!(store.start("owner", "one", "").is_err());
    assert_eq!(fs::read_to_string(&pointer).unwrap(), "../secrets");
    fs::write(&pointer, key("owner", "one")).unwrap();
    let mut conflicting = record.clone();
    conflicting.purpose = "different".into();
    let live = canonical.join(REGISTRY).join(format!("{}.json", record.id));
    fs::write(&live, serde_json::to_vec(&conflicting).unwrap()).unwrap();
    assert!(store.all().is_err());
    assert!(close(&canonical, &conflicting).is_err());
    assert!(live.exists());
    assert_eq!(
        by_request(&canonical, "owner", "one").unwrap().unwrap(),
        record
    );
}

#[cfg(unix)]
#[test]
fn archive_directory_links_are_rejected_without_touching_their_targets() {
    let home = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let store = Store::new(home.path().into());
    let (record, _) = store.start("owner", "one", "").unwrap();
    std::os::unix::fs::symlink(outside.path(), home.path().join(REGISTRY).join(CLOSED)).unwrap();
    assert_eq!(
        store
            .operation("owner", &record.id)
            .unwrap()
            .finish(&[], None)
            .unwrap_err()
            .code(),
        "WORK_SESSION_PATH_INVALID"
    );
    assert!(home
        .path()
        .join(REGISTRY)
        .join(format!("{}.json", record.id))
        .exists());
    assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
}
