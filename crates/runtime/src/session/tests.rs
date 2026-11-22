use super::control::{write_stdin_bounded, STDIN_WRITE_TIMEOUT};
use super::*;
use serde_json::json;
use uuid::Uuid;

/// 造一条已经跑完的会话。用 `true` 是因为它立刻退出，不用等、也不用 sleep。
fn finished_session(store: &SessionStore) -> String {
    let child = crate::async_rt::block_on(async {
        tokio::process::Command::new("true")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("起 true")
    });
    let session = store.insert(ExecSession::new(child));
    // 等它真的退出并记下结束时刻：保留期从这一刻起算。
    crate::async_rt::block_on(async {
        let _ = session.child.lock().await.wait().await;
    });
    crate::async_rt::block_on(session.refresh_status());
    session.session_id.clone()
}

/// 过期的句柄和瞎编的句柄不是一回事：前者命令确实跑过、只是输出没了，
/// 后者是自己记错了。
///
/// 过期时不能一句"重跑"了事：命令已经执行过，有副作用的原样重跑就是做第二遍
/// （审查 D05）。下一步要先核对现状。
///
/// 保留期设成 0，所以不用真的等 5 分钟，也没有 sleep（验收 A16）。
#[test]
fn an_expired_handle_is_not_the_same_as_an_unknown_one() {
    let store = SessionStore::with_retention(Duration::ZERO);
    let id = finished_session(&store);

    let Err(expired) = store.get(&id) else {
        panic!("保留期是 0，这条该没了");
    };
    assert_eq!(expired.code(), "SESSION_EXPIRED", "{}", expired.message());
    assert!(
        expired.message().contains("already ran")
            && expired.message().contains("check the current state"),
        "要说清命令已经跑过、先核对现状再决定重跑：{}",
        expired.message()
    );
    let details = &expired.to_error_value()["details"];
    assert_eq!(details["executed"], true);
    assert_eq!(details["output_recoverable"], false);

    let Err(unknown) = store.get("00000000-0000-0000-0000-000000000000") else {
        panic!("这个 id 从来没存在过");
    };
    assert_eq!(unknown.code(), "SESSION_NOT_FOUND");
}

/// 还在跑的会话不会被保留期扫掉——保留期从**进程结束**才开始算。
#[test]
fn a_running_session_is_never_swept() {
    let store = SessionStore::with_retention(Duration::ZERO);
    let child = crate::async_rt::block_on(async {
        tokio::process::Command::new("sleep")
            .arg("30")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("起 sleep")
    });
    let id = store.insert(ExecSession::new(child)).session_id.clone();

    assert!(store.get(&id).is_ok(), "在跑的会话被当成过期的清掉了");
    let session = store.get(&id).expect("还在");
    assert!(session.finished_ago().is_none());
    assert!(session.expires_in(Duration::from_secs(300)).is_none());

    crate::async_rt::block_on(session.kill_and_wait());
}

/// 结束的会话超过配额时，**结束得最早的**先走，还在跑的一条都不动。
#[test]
fn finished_sessions_are_capped_and_the_oldest_goes_first() {
    let store = SessionStore::with_retention(Duration::from_secs(3600));
    let mut ids = Vec::new();
    for _ in 0..(MAX_FINISHED_SESSIONS + 4) {
        ids.push(finished_session(&store));
    }
    let alive = ids.iter().filter(|id| store.get(id).is_ok()).count();
    assert!(
        alive <= MAX_FINISHED_SESSIONS,
        "配额没生效，还留着 {alive} 条"
    );
    // 最早那几条应当已经被回收，而且报的是"过期"而不是"没见过"。
    let Err(first) = store.get(&ids[0]) else {
        panic!("最早那条该被挤掉");
    };
    assert_eq!(first.code(), "SESSION_EXPIRED", "{}", first.message());
    assert!(
        store.get(ids.last().expect("最后一条")).is_ok(),
        "刚结束的那条不该被挤掉"
    );
}

/// 命令不读 stdin 时，写不进去要**有个头**，而且要说清写进去了多少。
///
/// 不封顶的话这次调用永远不返回（它跑在 block_on 里），调用方连"对面没在
/// 读"都不知道（验收 A17 的输入背压）。这里直接调有界写，传一个比管道
/// 缓冲大得多的块，对面是个从不读 stdin 的 `sleep`。
#[test]
fn writing_to_a_command_that_never_reads_stdin_gives_up_and_says_how_far_it_got() {
    let mut child = crate::async_rt::block_on(async {
        tokio::process::Command::new("sleep")
            .arg("30")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("起 sleep")
    });
    let mut stdin = child.stdin.take().expect("stdin");
    // 8 MiB：任何平台的管道缓冲都装不下，所以一定会卡在中途。
    let payload = vec![b'x'; 8 * 1024 * 1024];
    let started = Instant::now();
    let error = crate::async_rt::block_on(async {
        write_stdin_bounded(&mut stdin, &payload)
            .await
            .expect_err("对面不读，这一写不该成功")
    });
    assert_eq!(error.code(), "STDIN_WRITE_TIMEOUT", "{}", error.message());
    assert!(
        error.message().contains("not reading stdin"),
        "{}",
        error.message()
    );
    assert!(
        started.elapsed() < STDIN_WRITE_TIMEOUT + Duration::from_secs(5),
        "超时没起作用，等了 {:?}",
        started.elapsed()
    );
    crate::async_rt::block_on(async {
        let _ = child.kill().await;
    });
}

/// 从记录读结果时，"起跑之后 gld 写过几次"按计数器现算：同一个进程里算得出（命令结束后
/// 再写也算进去）；别的进程（gld 重启前）留下的算不出，给 null，不拿结束那一刻的 0 充数。
#[test]
fn a_record_from_another_process_does_not_claim_the_workspace_was_untouched() {
    let home = tempfile::tempdir().expect("home");
    let runs = RunLog::new(home.path().join("runs"), "p", "local");
    let counter = Arc::new(AtomicU64::new(5));
    let store = SessionStore::with_runs(runs.clone(), Arc::clone(&counter));
    let id = Uuid::new_v4().to_string();
    let writer = runs.start(&id, "true", None, 5, None).expect("建记录");
    writer.finish("exited", Some(0), 0);
    counter.fetch_add(2, Ordering::AcqRel);
    let args = json!({ "output_ref": format!("session:{id}:stdout") });

    let same = read_output(&store, &args).expect("read_output");
    assert_eq!(same["source"], "run_record");
    assert_eq!(same["workspace_writes_since_start"], 2, "{same}");

    crate::runs::orphan_record(&runs, &id);
    let other = read_output(&store, &args).expect("read_output");
    assert_eq!(other["termination_reason"], "exited", "{other}");
    assert!(other["workspace_writes_since_start"].is_null(), "{other}");
}

/// 工作区换代之后，上一代起的命令算得出之后写过几次；但只认交接之前起的：之后起的说明
/// 交接没成，它期间的写入这边没数到。计数倒退（乱序的交接）也不能算成"没写过"。
#[test]
fn a_command_from_the_previous_generation_counts_only_if_it_started_before_the_handoff() {
    let home = tempfile::tempdir().expect("home");
    let runs = RunLog::new(home.path().join("runs"), "p", "local");
    let counter = Arc::new(AtomicU64::new(5));
    let exported = Arc::new(Mutex::new(HashMap::new()));
    let store = SessionStore::with_runs(runs.clone(), Arc::clone(&counter))
        .counted_by(Arc::clone(&exported));
    let id = Uuid::new_v4().to_string();
    runs.start(&id, "true", None, 5, None)
        .expect("建记录")
        .finish("exited", Some(0), 0);
    crate::runs::orphan_record(&runs, &id);
    let record = runs.load_detached(&id).expect("读记录");

    assert_eq!(store.writes_since_start(&record), None, "名单外的进程不算");
    exported
        .lock()
        .expect("lock")
        .insert(record.owner.clone(), record.started_at_ms - 1);
    assert_eq!(store.writes_since_start(&record), None, "交接之后起的不算");
    exported
        .lock()
        .expect("lock")
        .insert(record.owner.clone(), record.started_at_ms);
    assert_eq!(store.writes_since_start(&record), Some(0));
    counter.fetch_add(2, Ordering::AcqRel);
    assert_eq!(store.writes_since_start(&record), Some(2));
    counter.store(1, Ordering::Release);
    assert_eq!(
        store.writes_since_start(&record),
        None,
        "计数倒退不能算成没写过"
    );
}

/// 本进程起的、记录停在 running、内存里却没有：不是真在跑（结束时没记下来），按 unknown
/// 说，别说成"另一个进程还在跑它"——那个进程就是自己。
#[test]
fn my_own_running_record_that_is_not_in_memory_is_unknown() {
    let home = tempfile::tempdir().expect("home");
    let runs = RunLog::new(home.path().join("runs"), "p", "local");
    let store = SessionStore::with_runs(runs.clone(), Arc::new(AtomicU64::new(0)));
    let id = Uuid::new_v4().to_string();
    let _writer = runs
        .start(&id, "sleep 30", Some(1), 0, None)
        .expect("建记录");
    let out = read_output(
        &store,
        &json!({ "output_ref": format!("session:{id}:stdout") }),
    )
    .expect("read_output");
    assert_eq!(out["termination_reason"], "unknown", "{out}");
    assert_eq!(out["running"], false, "{out}");
}

/// `read_output` 要说清这份输出还能读多久。
#[test]
fn read_output_says_how_long_the_output_still_lives() {
    let store = SessionStore::with_retention(Duration::from_secs(300));
    let id = finished_session(&store);
    let out = read_output(
        &store,
        &json!({ "output_ref": format!("session:{id}:stdout") }),
    )
    .expect("read_output");
    assert_eq!(out["retention_ms"], 300_000u64);
    let left = out["expires_in_ms"]
        .as_u64()
        .expect("已结束的会话要给这个数");
    assert!(left > 0 && left <= 300_000, "{out}");
}
