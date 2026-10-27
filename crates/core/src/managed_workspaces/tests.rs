use super::*;

#[test]
fn creation_is_idempotent_scoped_to_owner_and_survives_reopening() {
    let home = tempfile::tempdir().unwrap();
    let store = Store::new(home.path().into());
    assert!(store.all().unwrap().is_empty());
    let (one, created) = store.start("oauth:hub:a", "request-1", "转换文件").unwrap();
    assert!(created);
    assert!(!one.root.join(".git").exists());
    fs::write(one.root.join("output.txt"), "kept").unwrap();
    let reopened = Store::new(home.path().into());
    let (again, created) = reopened
        .start("oauth:hub:a", "request-1", "转换文件")
        .unwrap();
    assert!(!created);
    assert_eq!(one.id, again.id);
    assert_eq!(
        fs::read_to_string(again.root.join("output.txt")).unwrap(),
        "kept"
    );
    let (other, _) = store.start("oauth:hub:b", "request-1", "转换文件").unwrap();
    assert_ne!(one.root, other.root);
    assert_eq!(store.owned("oauth:hub:a").unwrap().len(), 1);
    assert_eq!(
        store.get("oauth:hub:b", &one.id, false).unwrap_err().code(),
        "WORK_SESSION_NOT_FOUND"
    );
    assert_eq!(
        store
            .start("oauth:hub:a", "request-1", "别的用途")
            .unwrap_err()
            .code(),
        "WORK_SESSION_REQUEST_CONFLICT"
    );
}

#[test]
fn concurrent_retries_create_one_directory_and_one_record() {
    let home = tempfile::tempdir().unwrap();
    let store = Store::new(home.path().into());
    let threads: Vec<_> = (0..8)
        .map(|_| {
            let store = store.clone();
            std::thread::spawn(move || store.start("owner", "same", "").unwrap())
        })
        .collect();
    let results: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|(_, created)| *created).count(), 1);
    assert!(results.iter().all(|(r, _)| r.id == results[0].0.id));
    assert_eq!(store.all().unwrap().len(), 1);
    assert_eq!(fs::read_dir(home.path().join("work")).unwrap().count(), 1);
}

#[test]
fn creating_record_is_resumed_but_active_missing_directory_is_not_recreated() {
    let home = tempfile::tempdir().unwrap();
    let store = Store::new(home.path().into());
    let (mut record, _) = store.start("owner", "resume", "").unwrap();
    record.state = State::Creating;
    save(&home.path().canonicalize().unwrap(), &record).unwrap();
    fs::remove_dir(&record.root).unwrap();
    let (resumed, created) = store.start("owner", "resume", "").unwrap();
    assert_eq!(resumed.state, State::Active);
    assert_eq!(resumed.id, record.id);
    assert!(!created);
    fs::remove_dir(&record.root).unwrap();
    assert_eq!(
        store.start("owner", "resume", "").unwrap_err().code(),
        "WORK_SESSION_DIRECTORY_UNAVAILABLE"
    );
    assert!(!record.root.exists());
    assert_eq!(
        store.all().unwrap().len(),
        1,
        "missing roots still protect retained records"
    );
}

#[test]
fn corrupt_or_redirected_metadata_is_not_silently_dropped() {
    let home = tempfile::tempdir().unwrap();
    let store = Store::new(home.path().into());
    let (mut record, _) = store.start("owner", "request", "").unwrap();
    record.root = home.path().join("data");
    save(&home.path().canonicalize().unwrap(), &record).unwrap();
    assert_eq!(
        store.all().unwrap_err().code(),
        "WORK_SESSION_REGISTRY_INVALID"
    );
    let path = home
        .path()
        .join(REGISTRY)
        .join(format!("{}.json", record.id));
    fs::write(&path, "{broken").unwrap();
    assert_eq!(
        store.start("owner", "next", "").unwrap_err().code(),
        "WORK_SESSION_REGISTRY_INVALID"
    );
    assert_eq!(fs::read_to_string(path).unwrap(), "{broken");
}

#[test]
fn invalid_identifiers_cannot_select_paths() {
    let home = tempfile::tempdir().unwrap();
    let store = Store::new(home.path().into());
    for id in ["../data", "/tmp", "work-../data", "", "work-xyz"] {
        assert_eq!(
            store.get("owner", id, false).unwrap_err().code(),
            "WORK_SESSION_NOT_FOUND"
        );
    }
    assert!(store.start("owner", "", "").is_err());
    assert!(store.start("owner", "x\n", "").is_err());
    assert!(store.start("owner", &"x".repeat(129), "").is_err());
    assert!(!home.path().join(REGISTRY).exists());
}

#[cfg(unix)]
#[test]
fn linked_work_and_registry_paths_are_rejected() {
    use std::os::unix::fs::symlink;
    let home = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let store = Store::new(home.path().into());
    let (record, _) = store.start("owner", "one", "").unwrap();
    fs::remove_dir(&record.root).unwrap();
    symlink(outside.path(), &record.root).unwrap();
    assert_eq!(
        store.get("owner", &record.id, true).unwrap_err().code(),
        "WORK_SESSION_PATH_INVALID"
    );
    assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
    let path = home
        .path()
        .join(REGISTRY)
        .join(format!("{}.json", record.id));
    fs::remove_file(&path).unwrap();
    symlink(outside.path().join("secret.json"), &path).unwrap();
    assert_eq!(store.all().unwrap_err().code(), "WORK_SESSION_PATH_INVALID");
}

#[cfg(unix)]
#[test]
fn operator_selected_data_home_alias_resolves_to_the_same_registry() {
    let home = tempfile::tempdir().unwrap();
    let aliases = tempfile::tempdir().unwrap();
    let alias = aliases.path().join("data-home");
    std::os::unix::fs::symlink(home.path(), &alias).unwrap();
    let selected = Store::new(alias);
    assert!(selected.all().unwrap().is_empty());
    let (record, _) = selected.start("owner", "request", "").unwrap();
    let actual = Store::new(home.path().into());
    assert_eq!(
        actual.get("owner", &record.id, false).unwrap().root,
        record.root
    );
    assert!(record.root.starts_with(home.path().canonicalize().unwrap()));
}
