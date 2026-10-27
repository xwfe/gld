use super::*;

#[test]
fn saved_versions_are_idempotent_verified_and_survive_the_source() {
    let home = tempfile::tempdir().unwrap();
    let store = Store::new(home.path().into());
    let (record, _) = store.start("a", "one", "").unwrap();
    fs::write(record.root.join("result.txt"), "第一版\n").unwrap();
    let paths = vec!["result.txt".into()];
    let mut op = store.operation("a", &record.id).unwrap();
    let first = op.preserve(&paths).unwrap().remove(0);
    assert_eq!(op.preserve(&paths).unwrap()[0].id, first.id);
    assert_eq!(op.record.artifacts.len(), 1);
    fs::write(record.root.join("result.txt"), "second").unwrap();
    let second = op.preserve(&paths).unwrap().remove(0);
    assert_ne!(first.id, second.id);
    assert_eq!(op.artifact(&first.id, true).unwrap()["content"], "第一版\n");
    assert!(record.root.join("result.txt").exists());
    drop(op);
    fs::remove_dir_all(&record.root).unwrap();
    let op = store.operation("a", &record.id).unwrap();
    assert_eq!(op.artifact(&second.id, true).unwrap()["content"], "second");
    assert_eq!(
        store.operation("b", &record.id).err().unwrap().code(),
        "WORK_SESSION_NOT_FOUND"
    );
}

#[test]
fn corrupt_destination_is_never_overwritten_and_source_is_kept() {
    let home = tempfile::tempdir().unwrap();
    let store = Store::new(home.path().into());
    let (record, _) = store.start("a", "one", "").unwrap();
    fs::write(record.root.join("out.txt"), "only source").unwrap();
    let mut op = store.operation("a", &record.id).unwrap();
    let item = op.preserve(&["out.txt".into()]).unwrap().remove(0);
    fs::write(op.artifact_root(false).unwrap().join(&item.id), "corrupted").unwrap();
    assert_eq!(
        op.preserve(&["out.txt".into()]).unwrap_err().code(),
        "ARTIFACT_INTEGRITY"
    );
    assert!(op.artifact(&item.id, true).is_err());
    assert_eq!(
        fs::read_to_string(record.root.join("out.txt")).unwrap(),
        "only source"
    );
}

#[test]
fn invalid_files_and_invalid_ids_are_bounded_and_never_select_other_paths() {
    let home = tempfile::tempdir().unwrap();
    let store = Store::new(home.path().into());
    let (record, _) = store.start("a", "one", "").unwrap();
    fs::write(record.root.join("out.txt"), "source").unwrap();
    let mut op = store.operation("a", &record.id).unwrap();
    for path in [
        "/tmp",
        "../out.txt",
        "a/../out.txt",
        ".env",
        "C:/x",
        "",
        "a\\b",
        "a//b",
    ] {
        assert!(op.preserve(&[path.into()]).is_err(), "{path}");
    }
    for id in [
        "../x".to_string(),
        "x".repeat(101),
        format!("art-{}x", "中".repeat(32)),
    ] {
        assert!(artifact_workspace(&id).is_err());
    }
    let file = File::create(record.root.join("huge.bin")).unwrap();
    file.set_len(MAX_ARTIFACT_BYTES + 1).unwrap();
    // 先查文件大小，超大文件不应被整段复制或读进内存。
    assert_eq!(
        op.preserve(&["huge.bin".into()]).unwrap_err().code(),
        "ARTIFACT_TOO_LARGE"
    );
    assert!(record.root.join("huge.bin").exists());
    assert!(op.record.artifacts.is_empty());
}

#[cfg(unix)]
#[test]
fn symlinks_hardlinks_and_redirected_artifact_roots_do_not_leak_files() {
    use std::os::unix::fs::symlink;
    let home = tempfile::tempdir().unwrap();
    let external = tempfile::tempdir().unwrap();
    let store = Store::new(home.path().into());
    let (record, _) = store.start("a", "one", "").unwrap();
    fs::write(external.path().join("secret"), "secret").unwrap();
    symlink(external.path().join("secret"), record.root.join("link.txt")).unwrap();
    fs::hard_link(external.path().join("secret"), record.root.join("hard.txt")).unwrap();
    let mut op = store.operation("a", &record.id).unwrap();
    for path in ["link.txt", "hard.txt"] {
        assert_eq!(
            op.preserve(&[path.into()]).unwrap_err().code(),
            "ARTIFACT_PATH_INVALID"
        );
    }
    fs::write(record.root.join("out.txt"), "source").unwrap();
    // hardlink 检查发生在创建空产物目录之后；只移除这个测试自己的空目录。
    if home.path().join("artifacts").exists() {
        fs::remove_dir_all(home.path().join("artifacts")).unwrap();
    }
    symlink(external.path(), home.path().join("artifacts")).unwrap();
    assert!(op.preserve(&["out.txt".into()]).is_err());
    assert_eq!(fs::read_dir(external.path()).unwrap().count(), 1);
    assert!(record.root.join("out.txt").exists());
}

#[test]
fn metadata_queries_work_for_binary_and_large_text_without_returning_body() {
    let home = tempfile::tempdir().unwrap();
    let store = Store::new(home.path().into());
    let (record, _) = store.start("a", "one", "").unwrap();
    fs::write(record.root.join("binary.bin"), [0xff, 0xfe]).unwrap();
    fs::write(
        record.root.join("large.txt"),
        vec![b'x'; MAX_TEXT_BYTES as usize + 1],
    )
    .unwrap();
    let mut op = store.operation("a", &record.id).unwrap();
    let items = op
        .preserve(&["binary.bin".into(), "large.txt".into()])
        .unwrap();
    assert_eq!(op.artifact(&items[0].id, false).unwrap()["verified"], true);
    assert!(op.artifact(&items[0].id, true).is_err());
    assert_eq!(
        op.artifact(&items[1].id, true).unwrap_err().code(),
        "ARTIFACT_CONTENT_TOO_LARGE"
    );
}
