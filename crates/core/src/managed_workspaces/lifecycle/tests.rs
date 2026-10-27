use super::*;

#[test]
fn finish_keeps_verified_outputs_and_retries_without_reopening_directory() {
    let home = tempfile::tempdir().unwrap();
    let store = Store::new(home.path().into());
    let (record, _) = store.start("owner", "one", "").unwrap();
    fs::write(record.root.join("result.txt"), "important").unwrap();
    fs::write(record.root.join("scratch.tmp"), "discard").unwrap();
    let mut op = store.operation("owner", &record.id).unwrap();
    let result = op.finish(&["result.txt".into()], None).unwrap();
    assert_eq!(result["closed"], true);
    assert!(!record.root.exists());
    let id = result["artifacts"][0]["id"].as_str().unwrap();
    assert_eq!(op.artifact(id, true).unwrap()["content"], "important");
    assert_eq!(
        op.finish(&["result.txt".into()], None).unwrap()["already_closed"],
        true
    );
    assert_eq!(
        op.finish(&[], None).unwrap_err().code(),
        "WORK_SESSION_FINISH_CONFLICT"
    );
    assert_eq!(op.touch().unwrap_err().code(), "WORK_SESSION_NOT_ACTIVE");
    drop(op);
    let (again, new) = store.start("owner", "one", "").unwrap();
    assert!(!new);
    assert_eq!(again.state, State::Closed);
    assert!(!record.root.exists());
}

#[test]
fn invalid_save_or_corrupt_artifact_keeps_the_only_source() {
    let home = tempfile::tempdir().unwrap();
    let store = Store::new(home.path().into());
    let (record, _) = store.start("owner", "one", "").unwrap();
    fs::write(record.root.join("result.txt"), "source").unwrap();
    let mut op = store.operation("owner", &record.id).unwrap();
    assert!(op.finish(&["missing.txt".into()], None).is_err());
    assert_eq!(op.record.state, State::Active);
    let item = op.preserve(&["result.txt".into()]).unwrap().remove(0);
    fs::write(
        home.path()
            .join("artifacts")
            .join(&record.id)
            .join(&item.id),
        "bad",
    )
    .unwrap();
    assert_eq!(
        op.finish(&["result.txt".into()], None).unwrap_err().code(),
        "ARTIFACT_INTEGRITY"
    );
    assert_eq!(
        fs::read_to_string(record.root.join("result.txt")).unwrap(),
        "source"
    );
}

#[test]
fn persisted_unknown_intent_blocks_cleanup_even_when_command_logs_are_gone() {
    let home = tempfile::tempdir().unwrap();
    let store = Store::new(home.path().into());
    let (record, _) = store.start("owner", "one", "").unwrap();
    let mut op = store.operation("owner", &record.id).unwrap();
    op.begin_command().unwrap();
    drop(op);
    let mut op = Store::new(home.path().into())
        .operation("owner", &record.id)
        .unwrap();
    assert_eq!(
        op.finish(&[], None).unwrap_err().code(),
        "WORK_SESSION_RESOURCES_BUSY"
    );
    assert!(record.root.exists());
    assert_eq!(op.resources(None).unwrap()["unknown_commands"], 1);
}

#[test]
fn retries_resume_from_saved_close_plan_after_partial_cleanup() {
    let home = tempfile::tempdir().unwrap();
    let store = Store::new(home.path().into());
    let (record, _) = store.start("owner", "one", "").unwrap();
    fs::write(record.root.join("result.txt"), "kept").unwrap();
    let mut op = store.operation("owner", &record.id).unwrap();
    let item = op.preserve(&["result.txt".into()]).unwrap().remove(0);
    op.record.finish_paths = Some(vec!["result.txt".into()]);
    op.record.state = State::Closing;
    op.persist().unwrap();
    // 模拟删除成功，但在提交 closed 回执之前进程中断。
    fs::remove_dir_all(&record.root).unwrap();
    drop(op);
    let mut op = store.operation("owner", &record.id).unwrap();
    assert_eq!(
        op.finish(&["result.txt".into()], None).unwrap()["closed"],
        true
    );
    assert_eq!(op.artifact(&item.id, true).unwrap()["content"], "kept");
}

#[test]
fn old_records_without_resource_history_are_not_automatically_trusted() {
    let home = tempfile::tempdir().unwrap();
    let store = Store::new(home.path().into());
    let (record, _) = store.start("owner", "one", "").unwrap();
    let mut op = store.operation("owner", &record.id).unwrap();
    op.record.resource_tracking = false;
    op.persist().unwrap();
    assert_eq!(
        op.finish(&[], None).unwrap_err().code(),
        "WORK_SESSION_RESOURCES_BUSY"
    );
    assert!(record.root.exists());
}
