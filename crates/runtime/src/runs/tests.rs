use super::*;
use tempfile::tempdir;

fn id() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[test]
fn reopening_legacy_runs_preserves_bytes_without_adopting_the_new_caller() {
    use gld_policy::{
        AuthContext, CredentialKind, Grant, GrantPermissions, GrantStatus, McpSubject, ProjectScope,
    };

    let home = tempdir().expect("home");
    let root = home.path().join("runs");
    let session = id();
    let legacy = "oauth:hub:alpha";
    let log = RunLog::new(root.clone(), "p", legacy);
    let writer = log.start(&session, "true", None, 0, None).expect("记录");
    writer.append(true, b"legacy output\n");
    writer.finish("exited", Some(0), 0);
    drop(writer);
    let path = log.dir().join(&session).join(RECORD);
    let before = fs::read(&path).expect("旧记录");
    drop(log);

    let caller = AuthContext::verified_mcp(
        "hub",
        Grant {
            id: "grant-alpha".into(),
            name: "alpha".into(),
            scope: ProjectScope::AllProjects,
            permissions: GrantPermissions::default(),
            status: GrantStatus::Active,
        },
        CredentialKind::OAuth,
        McpSubject::OAuth {
            client_id: "alpha".into(),
        },
    )
    .expect("新身份");
    let current = RunLog::new(root.clone(), "p", caller.id().key());
    assert!(current.load(&session).is_none());
    assert!(current.list().is_empty());
    assert_eq!(fs::read(&path).expect("仍然留存"), before);

    let reopened = RunLog::new(root, "p", legacy);
    let record = reopened.load(&session).expect("原归属可重读");
    assert_eq!(record.caller, legacy);
    assert_eq!(reopened.list(), vec![record.clone()]);
    assert_eq!(
        reopened.stream_bytes(&record, "stdout").0,
        b"legacy output\n"
    );
}

/// 输出超过两段时只留最新两段，偏移和整条流对得上：读出来的就是流的最后那部分。
#[test]
fn a_long_stream_keeps_its_last_two_segments_with_true_offsets() {
    let home = tempdir().expect("home");
    let log = RunLog::new(home.path().join("runs"), "p", "local");
    let session = id();
    let writer = log.start(&session, "flood", None, 0, None).expect("建记录");
    // 3.5 段：每个字节是它在流里位置的低 8 位，读出来的每个字节都能对回位置。
    let total = SEGMENT_BYTES as usize * 7 / 2;
    let bytes: Vec<u8> = (0..total).map(|at| at as u8).collect();
    for chunk in bytes.chunks(4096) {
        writer.append(true, chunk);
    }
    let record = log.load(&session).expect("记录");
    let (data, seen_total) = log.stream_bytes(&record, "stdout");
    assert_eq!(seen_total, total);
    assert_eq!(
        segments(log.dir().join(&session).as_path(), "stdout").len(),
        2
    );
    assert!(data.len() > SEGMENT_BYTES as usize && data.len() <= 2 * SEGMENT_BYTES as usize);
    let from = total - data.len();
    assert_eq!(from % SEGMENT_BYTES as usize, 0, "起点应落在段边界上");
    assert_eq!(&data[..], &bytes[from..]);
    let (stderr, stderr_total) = log.stream_bytes(&record, "stderr");
    assert!(stderr.is_empty() && stderr_total == 0);
}

/// 别的主体读不到；不像 session_id 的（路径穿越）读不到、也建不出来。
///
/// 穿越的目标上真放一份像样的记录：不查 id 的话，`../x` 拼出来正好读得到它。
#[test]
fn a_record_is_only_visible_to_its_caller_and_ids_cannot_escape() {
    let home = tempdir().expect("home");
    let root = home.path().join("runs");
    let mine = RunLog::new(root.clone(), "p", "oauth:hub:alpha");
    let theirs = RunLog::new(root.clone(), "p", "oauth:hub:beta");
    let session = id();
    drop(mine.start(&session, "true", None, 0, None).expect("建记录"));
    assert!(mine.load(&session).is_some());
    assert!(theirs.load(&session).is_none(), "别人的记录被读到了");

    let mut bait = read_record(&mine.dir().join(&session)).expect("记录");
    let outside = root.join("x");
    create_private_dir(&outside).expect("穿越目标");
    bait.session_id = "../x".into();
    write_record(&outside, &bait).expect("放一份记录");
    assert!(
        read_record(&mine.dir().join("../x")).is_some(),
        "诱饵没放对"
    );
    for bad in ["../x", "../../etc", "a/b", ".."] {
        assert!(mine.load(bad).is_none(), "{bad} 读到了项目目录外的记录");
        assert!(mine.start(bad, "true", None, 0, None).is_none());
    }
    assert!(!root.join("etc").exists() && !root.join("p/a").exists());
}

/// 跨项目、跨主体列出某个进程起的、还在跑的命令；结束了的、别的进程起的、起它的进程
/// 已经不在的都不算。
#[test]
fn running_under_lists_live_commands_of_one_owner_across_projects() {
    let home = tempdir().expect("home");
    let root = home.path().join("runs");
    let alpha = RunLog::new(root.clone(), "alpha", "local");
    let beta = RunLog::new(root.clone(), "beta", "oauth:hub:someone");
    let (first, second, done, orphan) = (id(), id(), id(), id());
    let _first = alpha
        .start(&first, "sleep 30", Some(1), 0, None)
        .expect("建记录");
    let _second = beta
        .start(&second, "pnpm dev", Some(2), 0, Some(5173))
        .expect("建记录");
    alpha
        .start(&done, "true", Some(3), 0, None)
        .expect("建记录")
        .finish("exited", Some(0), 0);
    let _orphan = alpha
        .start(&orphan, "sleep 40", Some(4), 0, None)
        .expect("建记录");
    orphan_record(&alpha, &orphan);

    let me = std::process::id();
    let listed = running_under(&root, me);
    let mut seen: Vec<(String, String)> = listed
        .iter()
        .map(|(project, record)| (project.clone(), record.session_id.clone()))
        .collect();
    seen.sort();
    let mut expected = vec![("alpha".to_string(), first), ("beta".to_string(), second)];
    expected.sort();
    assert_eq!(seen, expected);
    assert!(
        listed
            .windows(2)
            .all(|pair| pair[0].1.started_at_ms <= pair[1].1.started_at_ms),
        "要按起跑时间从早到晚"
    );
    assert!(
        running_under(&root, me.wrapping_add(1)).is_empty(),
        "别的进程起的不该算"
    );
    assert_eq!(
        read_record(&alpha.dir().join(&orphan))
            .expect("记录")
            .status,
        "unknown",
        "起它的进程不在了，读到时就该改成 unknown"
    );
    assert!(running_under(&home.path().join("nowhere"), me).is_empty());
}

/// 起它的进程还在（就是自己）：还是 running。起它的进程不在了：unknown，并写回盘上。
#[test]
fn a_running_record_whose_owner_is_gone_becomes_unknown() {
    let home = tempdir().expect("home");
    let log = RunLog::new(home.path().join("runs"), "p", "local");
    let session = id();
    let _writer = log
        .start(&session, "sleep 30", Some(42), 0, None)
        .expect("建记录");
    assert_eq!(log.load(&session).expect("记录").status, "running");

    orphan_record(&log, &session);
    let settled = log.load(&session).expect("记录");
    assert_eq!(settled.status, "unknown");
    assert!(settled.finished_at_ms.is_some());
    assert_eq!(settled.exit_code, None);
    assert_eq!(settled.workspace_writes_during_run, None);
    let again = read_record(&log.dir().join(&session)).expect("盘上的");
    assert_eq!(again.status, "unknown", "结论没写回去");
}

/// 另一个进程拿着锁时算活着；锁放了（进程退出）就算不在了，锁文件也顺手删掉。
#[test]
fn an_owner_is_alive_exactly_while_its_lock_is_held() {
    let home = tempdir().expect("home");
    let root = home.path().join("runs");
    let dir = root.join(OWNERS);
    create_private_dir(&dir).expect("owners");
    let path = dir.join("feedbeef.lock");
    let held = create_private_file(&path).expect("锁文件");
    // 同一个进程里另开一个 fd 也会被 flock 挡住，所以能在单测里模拟"别的进程拿着"。
    held.try_lock_exclusive().expect("拿锁");
    assert!(owner_alive(&root, "feedbeef"));
    drop(held);
    // 放锁不一定立刻生效：同一个测试进程里别的测试正好在起子进程时，fork 出来的子进程在
    // exec 之前拿着这个 fd 的副本（exec 时 CLOEXEC 才关掉），flock 跟着它多留一会儿。
    // 全量并发跑时偶发过一次失败。真进程退出时锁一定会放，这里给它最多 2 秒。
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while owner_alive(&root, "feedbeef") && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!owner_alive(&root, "feedbeef"));
    assert!(!path.exists(), "不在了的进程的锁文件该删掉");
    assert!(!owner_alive(&root, "neverexisted"));
}

/// 结束的记录超过配额时，结束得最早的先走；太旧的走；还在跑的一条不动。
#[test]
fn finished_records_are_capped_by_count_and_age_and_running_ones_stay() {
    let home = tempdir().expect("home");
    let log = RunLog::new(home.path().join("runs"), "p", "local");
    let running = id();
    let _running = log
        .start(&running, "sleep 30", None, 0, None)
        .expect("建记录");
    // 把这条跑着的记录的起跑时间改到很久以前：它还在跑，按年龄也不能清。
    {
        let dir = log.dir().join(&running);
        let mut record = read_record(&dir).expect("记录");
        record.started_at_ms = 1;
        write_record(&dir, &record).expect("写回");
    }
    let mut finished = Vec::new();
    for index in 0..(MAX_FINISHED_RUNS + 3) {
        let session = id();
        let writer = log.start(&session, "true", None, 0, None).expect("建记录");
        writer.finish("exited", Some(0), 0);
        // 结束时间拉开，排序才有先后。
        let dir = log.dir().join(&session);
        let mut record = read_record(&dir).expect("记录");
        record.finished_at_ms = Some(now_ms() - 60_000 + index as u64);
        write_record(&dir, &record).expect("写回");
        finished.push(session);
    }
    let stale = id();
    let writer = log.start(&stale, "true", None, 0, None).expect("建记录");
    writer.finish("exited", Some(0), 0);
    {
        let dir = log.dir().join(&stale);
        let mut record = read_record(&dir).expect("记录");
        record.finished_at_ms = Some(now_ms() - MAX_AGE.as_millis() as u64 - 1);
        write_record(&dir, &record).expect("写回");
    }
    log.sweep();
    assert!(log.load(&running).is_some(), "还在跑的被清掉了");
    assert!(log.load(&stale).is_none(), "超过七天的没清");
    let kept = finished.iter().filter(|id| log.load(id).is_some()).count();
    assert_eq!(kept, MAX_FINISHED_RUNS);
    assert!(log.load(&finished[0]).is_none(), "最早结束的该先走");
    assert!(log.load(finished.last().expect("最后一条")).is_some());
}

/// 不再用的项目也会被清：进程第一次拿到 owner 锁时把 `runs/` 下所有项目扫一遍，
/// 空了的项目目录一并删掉。只在"同一个项目起新命令"时清的话，删掉的项目永远没人清。
#[test]
fn records_of_projects_nobody_uses_any_more_are_swept_too() {
    let home = tempdir().expect("home");
    let root = home.path().join("runs");
    let now = now_ms();
    let max_age = MAX_AGE.as_millis() as u64;
    let finished_at = |finished: u64| RunRecord {
        session_id: String::new(),
        caller: "local".into(),
        command: "true".into(),
        pid: None,
        owner: "gone".into(),
        owner_pid: 1,
        started_at_ms: finished - 10,
        status: "exited".into(),
        exit_code: Some(0),
        finished_at_ms: Some(finished),
        workspace_writes_during_run: Some(0),
        writes_at_start: Some(0),
        incomplete_logs: Vec::new(),
        service_port: None,
    };
    let stale = root.join("removed-project").join(id());
    create_private_dir(&stale).expect("dir");
    write_record(&stale, &finished_at(now - max_age - 1)).expect("旧记录");
    let fresh = root.join("idle-project").join(id());
    create_private_dir(&fresh).expect("dir");
    write_record(&fresh, &finished_at(now - 1000)).expect("新记录");

    drop(
        RunLog::new(root.clone(), "p", "local")
            .start(&id(), "true", None, 0, None)
            .expect("建记录"),
    );
    assert!(
        !root.join("removed-project").exists(),
        "没人用的项目里过期的记录和空目录都该清掉"
    );
    assert!(fresh.join(RECORD).exists(), "还新的记录不该清");
}

/// 自己的锁文件一出现在正式名字下，就已经被锁着：别的进程这时来判活，看到的是"还在"，
/// 不会把它删掉、也不会把这个进程的命令判成 unknown。临时名字不留下。
#[test]
fn the_owner_lock_is_held_before_it_appears_under_its_name() {
    let home = tempdir().expect("home");
    let root = home.path().join("runs");
    let log = RunLog::new(root.clone(), "p", "local");
    drop(log.start(&id(), "true", None, 0, None).expect("建记录"));
    let dir = root.join(OWNERS);
    let lock = dir.join(format!("{}.lock", *INSTANCE));
    let other = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&lock)
        .expect("锁文件在正式名字下");
    assert!(other.try_lock_exclusive().is_err(), "锁文件在，锁却没拿着");
    let leftovers: Vec<_> = fs::read_dir(&dir)
        .expect("owners")
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "临时锁文件没改名");
}

/// 写盘失败（这里用只读目录让换段失败，和盘满一样）之后这个流不再落盘，
/// 结束时记录里注明哪个流不全；已经写下的那部分照样读得到。
#[cfg(unix)]
#[test]
fn a_log_that_could_not_be_written_is_marked_incomplete() {
    use std::os::unix::fs::PermissionsExt;
    let home = tempdir().expect("home");
    let log = RunLog::new(home.path().join("runs"), "p", "local");
    let session = id();
    let writer = log.start(&session, "flood", None, 0, None).expect("建记录");
    let dir = log.dir().join(&session);
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o500)).expect("只读");
    writer.append(true, &vec![b'x'; SEGMENT_BYTES as usize + 10]);
    writer.append(true, b"after");
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).expect("恢复");
    writer.finish("exited", Some(0), 0);
    let record = log.load(&session).expect("记录");
    assert_eq!(record.incomplete_logs, vec!["stdout".to_string()]);
    let (data, total) = log.stream_bytes(&record, "stdout");
    assert_eq!(total, SEGMENT_BYTES as usize, "写下的那部分应当还在");
    assert_eq!(data.len(), total);
}

/// 目录 0700、文件 0600：输出不脱敏，别让同机的其他账号读到。
#[cfg(unix)]
#[test]
fn records_and_logs_are_private_to_the_owner() {
    use std::os::unix::fs::PermissionsExt;
    let home = tempdir().expect("home");
    let log = RunLog::new(home.path().join("runs"), "p", "local");
    let session = id();
    let writer = log
        .start(&session, "echo secret", None, 0, None)
        .expect("建记录");
    writer.append(true, b"token=abc\n");
    writer.finish("exited", Some(0), 0);
    let dir = log.dir().join(&session);
    let mode = |path: &Path| fs::metadata(path).expect("stat").permissions().mode() & 0o777;
    assert_eq!(mode(log.dir()), 0o700);
    assert_eq!(mode(&dir), 0o700);
    assert_eq!(mode(&dir.join(RECORD)), 0o600);
    assert_eq!(mode(&segment_path(&dir, "stdout", 0)), 0o600);
}
