use std::sync::{Arc, Barrier};

use gld_policy::{CredentialKind, GrantPermissions, ProjectScope, ServiceCapabilities};
use serde_json::{json, Value};

use super::*;

fn record(id: &str, name: &str, secret: &str) -> AccessRecord {
    AccessRecord {
        grant: Grant {
            id: id.into(),
            name: name.into(),
            scope: ProjectScope::AllProjects,
            permissions: GrantPermissions {
                write: false,
                service: ServiceCapabilities {
                    user_prompts: false,
                    machine_mcp: false,
                    managed_workspaces: false,
                },
            },
            status: GrantStatus::Active,
        },
        credential: CredentialRecord::prepare(
            format!("credential-{id}"),
            CredentialKind::Token,
            secret,
            1,
            100,
            Some(3600),
            Vec::new(),
        )
        .unwrap(),
    }
}

fn saved(records: Vec<AccessRecord>) -> Value {
    serde_json::to_value(AccessFile {
        schema_version: SCHEMA_VERSION,
        records,
    })
    .unwrap()
}

fn write_private(path: &Path, bytes: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut file = create_private_file(path).unwrap();
    file.write_all(bytes).unwrap();
}

#[test]
fn new_store_ignores_legacy_data_and_persists_only_verifiers() {
    let home = tempfile::tempdir().unwrap();
    let legacy = home.path().join("data").join("profiles.json");
    let legacy_bytes = br#"{"grants":[{"bearer_token":"legacy-plain-secret"}]}"#;
    write_private(&legacy, legacy_bytes);
    let store = AccessStore::open_in(home.path()).unwrap();
    assert!(store.snapshot().unwrap().records().is_empty());
    store
        .insert(record("one", "Client", "one-time-secret"))
        .unwrap();
    let reader = store.reader();
    let current = reader.snapshot().unwrap();
    let found = current.by_credential_id("credential-one").unwrap();
    assert!(found.credential.matches_secret("one-time-secret"));
    assert!(!found.credential.matches_secret("incorrect"));
    assert!(found.credential.is_valid_at(100));
    assert!(!found.credential.is_valid_at(99));
    assert!(!found.credential.is_valid_at(3700));
    assert_eq!(current.by_name("CLIENT").unwrap().grant.id, "one");
    let bytes = fs::read_to_string(&store.path).unwrap();
    assert!(!bytes.contains("one-time-secret"));
    assert!(!format!("{found:?}").contains("one-time-secret"));
    let digest = serde_json::to_value(&found.credential).unwrap()["secret_sha256"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(!format!("{found:?}").contains(&digest));
    assert_eq!(fs::read(&legacy).unwrap(), legacy_bytes);
    let path = store.path.clone();
    drop(current);
    drop(reader);
    drop(store);
    let reopened = AccessStore::open_in(home.path()).unwrap();
    assert!(reopened.snapshot().unwrap().by_grant_id("one").is_some());
    check_private_file(&File::open(path).unwrap()).unwrap();
}

#[test]
fn malformed_or_old_files_are_rejected_without_overwriting_them() {
    let valid = saved(vec![record("one", "Client", "test-secret")]);
    let mut missing_version = valid.clone();
    missing_version
        .as_object_mut()
        .unwrap()
        .remove("schema_version");
    let mut wrong_version = valid.clone();
    wrong_version["schema_version"] = json!(2);
    let mut extra = valid.clone();
    extra["legacy_grants"] = json!([]);
    let mut missing_scope = valid.clone();
    missing_scope["records"][0]["grant"]
        .as_object_mut()
        .unwrap()
        .remove("scope");
    let mut missing_expiry = valid.clone();
    missing_expiry["records"][0]["credential"]
        .as_object_mut()
        .unwrap()
        .remove("expires_at_unix_secs");
    let mut missing_ttl = valid.clone();
    missing_ttl["records"][0]["credential"]
        .as_object_mut()
        .unwrap()
        .remove("valid_for_secs");
    let mut empty_selected = record("one", "Client", "test-secret");
    empty_selected.grant.scope = ProjectScope::Selected(Vec::new());
    let mut duplicate_credential = record("two", "Other", "other-secret");
    duplicate_credential.credential.id = "credential-one".into();
    let mut invalid_digest = valid.clone();
    invalid_digest["records"][0]["credential"]["secret_sha256"] = json!("not-a-digest");
    let invalid = [
        String::new(),
        "{truncated".into(),
        r#"{"grants":[{"bearer_token":"legacy-secret"}]}"#.into(),
        missing_version.to_string(),
        wrong_version.to_string(),
        extra.to_string(),
        missing_scope.to_string(),
        missing_expiry.to_string(),
        missing_ttl.to_string(),
        saved(vec![empty_selected]).to_string(),
        saved(vec![
            record("one", "Client", "test-secret"),
            record("one", "Other", "other-secret"),
        ])
        .to_string(),
        saved(vec![
            record("one", "Client", "test-secret"),
            duplicate_credential,
        ])
        .to_string(),
        invalid_digest.to_string(),
    ];
    for bytes in invalid {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("access/v1/grants.json");
        write_private(&path, bytes.as_bytes());
        assert!(AccessStore::open_in(home.path()).is_err());
        assert_eq!(fs::read(&path).unwrap(), bytes.as_bytes());
    }
}

#[test]
fn one_writer_lock_lives_until_the_last_reader_is_dropped() {
    let home = tempfile::tempdir().unwrap();
    let first = AccessStore::open_in(home.path()).unwrap();
    let reader = first.reader();
    assert!(AccessStore::open_in(home.path()).is_err());
    drop(first);
    assert!(AccessStore::open_in(home.path()).is_err());
    drop(reader);
    let _reopened = AccessStore::open_in(home.path())
        .expect("the final reader must release the writer lock immediately");
}

#[cfg(unix)]
#[test]
fn a_duplicated_descriptor_cannot_extend_the_writer_lifetime() {
    let home = tempfile::tempdir().unwrap();
    let store = AccessStore::open_in(home.path()).unwrap();
    // fork 后、exec 关闭 CLOEXEC 描述符前也会保留同一个 open file description。
    // try_clone 能确定性地复现这个引用，避免依赖另一测试正好创建子进程。
    let duplicate = store.shared._lock.file.try_clone().unwrap();
    let reader = store.reader();
    drop(store);
    assert!(AccessStore::open_in(home.path()).is_err());
    drop(reader);
    let _reopened = AccessStore::open_in(home.path())
        .expect("an inherited descriptor must not retain a released writer lock");
    drop(duplicate);
}

#[test]
fn failed_writes_and_rename_keep_the_old_snapshot_and_allow_retry() {
    let home = tempfile::tempdir().unwrap();
    let store = AccessStore::open_in(home.path()).unwrap();
    store.insert(record("one", "Client", "old-secret")).unwrap();
    let original = fs::read(&store.path).unwrap();
    let before = store.snapshot().unwrap();
    let reader = store.reader();
    let parent = store.path.parent().unwrap();
    let held_directory = parent.with_file_name("held-v1");

    // 用非目录阻止临时文件创建；不依赖测试是否以管理员身份运行。
    fs::rename(parent, &held_directory).unwrap();
    fs::write(parent, "blocked").unwrap();
    assert!(store.insert(record("two", "New", "new-secret")).is_err());
    assert!(Arc::ptr_eq(&before, &reader.snapshot().unwrap()));
    assert_eq!(
        fs::read(held_directory.join("grants.json")).unwrap(),
        original
    );
    fs::remove_file(parent).unwrap();
    fs::rename(&held_directory, parent).unwrap();

    // 目标目录使真实 rename 失败，原文件先保留到旁边，恢复后应能继续写。
    let held_file = store.path.with_extension("held");
    fs::rename(&store.path, &held_file).unwrap();
    fs::create_dir(&store.path).unwrap();
    assert!(store.revoke("one").is_err());
    assert!(Arc::ptr_eq(&before, &reader.snapshot().unwrap()));
    assert_eq!(fs::read(&held_file).unwrap(), original);
    assert!(!fs::read_dir(parent).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".gld-access-")
    }));
    fs::remove_dir(&store.path).unwrap();
    fs::rename(&held_file, &store.path).unwrap();
    store.insert(record("two", "New", "new-secret")).unwrap();
    assert!(reader.snapshot().unwrap().by_grant_id("two").is_some());
}

#[test]
fn successful_updates_publish_new_revision_and_revoke_releases_only_the_name() {
    let home = tempfile::tempdir().unwrap();
    let store = AccessStore::open_in(home.path()).unwrap();
    store.insert(record("one", "Client", "old-secret")).unwrap();
    let reader = store.reader();
    let old = reader.snapshot().unwrap();
    let mut rotated = old.by_grant_id("one").unwrap().clone();
    rotated.credential = CredentialRecord::prepare(
        "credential-one".into(),
        CredentialKind::Token,
        "replacement-secret",
        2,
        200,
        Some(3600),
        Vec::new(),
    )
    .unwrap();
    store.update(rotated.clone()).unwrap();
    let current = reader.snapshot().unwrap();
    let credential = &current
        .by_credential_id("credential-one")
        .unwrap()
        .credential;
    assert_eq!(credential.revision, 2);
    assert!(credential.matches_secret("replacement-secret"));
    assert!(!credential.matches_secret("old-secret"));
    assert_eq!(old.by_grant_id("one").unwrap().credential.revision, 1);
    assert!(store.update(record("one", "Client", "old-secret")).is_err());
    store.revoke("one").unwrap();
    let current = reader.snapshot().unwrap();
    assert!(matches!(
        current.by_grant_id("one").unwrap().grant.status,
        GrantStatus::Revoked
    ));
    assert!(current.by_name("Client").is_none());
    assert!(store.update(rotated).is_err());
    assert!(store.insert(record("one", "Client", "reused-id")).is_err());
    store.insert(record("two", "Client", "new-grant")).unwrap();
    assert_eq!(
        reader
            .snapshot()
            .unwrap()
            .by_name("Client")
            .unwrap()
            .grant
            .id,
        "two"
    );
}

#[test]
fn credential_kind_lifetime_and_origin_are_validated_before_publish() {
    let home = tempfile::tempdir().unwrap();
    let store = AccessStore::open_in(home.path()).unwrap();
    let before = store.snapshot().unwrap();
    let mut bad = record("one", "Client", "test-secret");
    bad.credential.kind = CredentialKind::OAuth;
    assert!(store.insert(bad).is_err());
    assert!(Arc::ptr_eq(&before, &store.snapshot().unwrap()));
    assert!(CredentialRecord::prepare(
        "url-id".into(),
        CredentialKind::Url,
        "url-secret",
        1,
        100,
        None,
        Vec::new(),
    )
    .is_err());
    assert!(CredentialRecord::prepare(
        "url-id".into(),
        CredentialKind::Url,
        "url-secret",
        1,
        100,
        Some(24 * 3600 + 1),
        Vec::new(),
    )
    .is_err());
    assert!(CredentialRecord::prepare(
        "url-id".into(),
        CredentialKind::Url,
        "url-secret",
        1,
        100,
        Some(3600),
        vec!["*".into()],
    )
    .is_err());
    let key = CredentialRecord::prepare(
        "url-id".into(),
        CredentialKind::Url,
        "url-secret",
        1,
        100,
        Some(3600),
        vec!["https://client.example".into()],
    )
    .unwrap();
    assert!(key.allows_origin(None));
    assert!(key.allows_origin(Some("https://client.example")));
    assert!(!key.allows_origin(Some("https://other.example")));
}

#[test]
fn concurrent_mutations_publish_every_committed_record() {
    let home = tempfile::tempdir().unwrap();
    let store = Arc::new(AccessStore::open_in(home.path()).unwrap());
    let barrier = Arc::new(Barrier::new(4));
    let threads: Vec<_> = (0..4)
        .map(|index| {
            let store = store.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                store
                    .insert(record(
                        &format!("g{index}"),
                        &format!("Client {index}"),
                        "test-secret",
                    ))
                    .unwrap();
            })
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }
    assert_eq!(store.snapshot().unwrap().records().len(), 4);
    drop(store);
    assert_eq!(
        AccessStore::open_in(home.path())
            .unwrap()
            .snapshot()
            .unwrap()
            .records()
            .len(),
        4
    );
}
