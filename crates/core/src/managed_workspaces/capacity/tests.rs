use super::*;
use crate::settings::WorkSessionLimits;

fn limits(active: u32, records: u32, bytes: u64) -> WorkSessionLimits {
    WorkSessionLimits {
        max_active: active,
        max_records: records,
        max_bytes: bytes,
    }
}

#[test]
fn active_slots_are_reused_but_receipt_limit_preserves_all_retries() {
    let home = tempfile::tempdir().unwrap();
    let store = Store::new(home.path().into()).with_limits(limits(1, 2, 1024));
    let (first, _) = store.start("a", "one", "").unwrap();
    assert_eq!(
        store.start("b", "two", "").unwrap_err().code(),
        "WORK_SESSION_LIMIT"
    );
    assert_eq!(store.start("a", "one", "").unwrap().0.id, first.id);
    store
        .operation("a", &first.id)
        .unwrap()
        .finish(&[], None)
        .unwrap();
    let (second, _) = store.start("b", "two", "").unwrap();
    store
        .operation("b", &second.id)
        .unwrap()
        .finish(&[], None)
        .unwrap();
    assert_eq!(
        store.start("a", "three", "").unwrap_err().code(),
        "WORK_SESSION_LIMIT"
    );
    assert_eq!(store.start("a", "one", "").unwrap().0.state, State::Closed);
    let usage = store.capacity().unwrap();
    assert_eq!(usage["usage"]["active"], 0);
    assert_eq!(usage["usage"]["closed"], 2);
    assert_eq!(usage["usage"]["records"], 2);
    let lowered = store.with_limits(limits(1, 1, 1));
    assert_eq!(
        lowered.inspect("a", &first.id).unwrap().state,
        State::Closed
    );
}

#[test]
fn byte_budget_keeps_sources_and_allows_idempotent_saves_and_cleanup_after_lowering() {
    let home = tempfile::tempdir().unwrap();
    let store = Store::new(home.path().into()).with_limits(limits(2, 4, 7));
    let (record, _) = store.start("a", "one", "").unwrap();
    fs::write(record.root.join("result.txt"), "1234").unwrap();
    let mut op = store.operation("a", &record.id).unwrap();
    assert_eq!(
        op.preserve(&["result.txt".into()]).unwrap_err().code(),
        "WORK_SESSION_STORAGE_LIMIT"
    );
    assert_eq!(
        fs::read_to_string(record.root.join("result.txt")).unwrap(),
        "1234"
    );
    assert!(op.record.artifacts.is_empty());
    drop(op);
    let larger = store.clone().with_limits(limits(2, 4, 8));
    let saved = larger
        .operation("a", &record.id)
        .unwrap()
        .preserve(&["result.txt".into()])
        .unwrap()
        .remove(0);
    assert_eq!(larger.capacity().unwrap()["usage"]["bytes"], 8);
    assert_eq!(
        larger.start("a", "new", "").unwrap_err().code(),
        "WORK_SESSION_STORAGE_LIMIT"
    );
    let lowered = store.with_limits(limits(2, 4, 1));
    assert_eq!(lowered.start("a", "one", "").unwrap().0.id, record.id);
    let mut op = lowered.operation("a", &record.id).unwrap();
    assert_eq!(op.preserve(&["result.txt".into()]).unwrap()[0].id, saved.id);
    assert_eq!(
        op.finish(&["result.txt".into()], None).unwrap()["closed"],
        true
    );
    assert_eq!(op.artifact(&saved.id, true).unwrap()["content"], "1234");
    assert_eq!(lowered.capacity().unwrap()["usage"]["bytes"], 4);
}

#[test]
fn concurrent_artifact_copies_share_one_budget() {
    let home = tempfile::tempdir().unwrap();
    let store = Store::new(home.path().into()).with_limits(limits(2, 4, 14));
    let first = store.start("a", "one", "").unwrap().0;
    let second = store.start("a", "two", "").unwrap().0;
    for record in [&first, &second] {
        fs::write(record.root.join("result.txt"), "1234").unwrap();
    }
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let threads: Vec<_> = [first, second]
        .into_iter()
        .map(|record| {
            let store = store.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let mut op = store.operation("a", &record.id).unwrap();
                barrier.wait();
                op.preserve(&["result.txt".into()])
            })
        })
        .collect();
    let results: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .find_map(|r| r.as_ref().err())
            .unwrap()
            .code(),
        "WORK_SESSION_STORAGE_LIMIT"
    );
    assert_eq!(store.capacity().unwrap()["usage"]["bytes"], 12);
}

#[cfg(unix)]
#[test]
fn usage_never_follows_links_or_counts_service_credentials() {
    let home = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let store = Store::new(home.path().into());
    let record = store.start("a", "one", "").unwrap().0;
    fs::write(record.root.join("own.txt"), "abc").unwrap();
    fs::write(outside.path().join("private.txt"), "outside content").unwrap();
    std::os::unix::fs::symlink(outside.path(), record.root.join("linked")).unwrap();
    fs::create_dir(home.path().join("data")).unwrap();
    fs::write(home.path().join("data/profiles.json"), "fixture only").unwrap();
    assert_eq!(store.capacity().unwrap()["usage"]["bytes"], 3);
    std::os::unix::fs::symlink(outside.path(), home.path().join("artifacts")).unwrap();
    assert!(
        store.capacity().is_err(),
        "redirected storage is unknown, never zero"
    );
}

#[test]
fn defaults_and_invalid_limits_have_explicit_boundaries() {
    let old: crate::settings::HubConfig = serde_json::from_value(json!({})).unwrap();
    assert_eq!(old.work_session_limits, WorkSessionLimits::default());
    for bad in [
        limits(0, 2, 1),
        limits(3, 2, 1),
        limits(1, 8193, 1),
        limits(1, 1, 0),
    ] {
        assert!(bad.validate().is_err());
    }
    assert!(limits(1, 1, 1).validate().is_ok());
}
