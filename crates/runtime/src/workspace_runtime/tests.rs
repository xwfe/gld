use super::*;
use fs2::FileExt;
use std::fs::File;
use std::time::{Duration, Instant};
use tempfile::tempdir;

#[test]
fn the_same_directory_hands_out_the_same_runtime() {
    let dir = tempdir().expect("tempdir");
    let root = dir.path().canonicalize().expect("canonical");
    assert!(
        Arc::ptr_eq(&runtime_for(&root), &runtime_for(&root)),
        "同一个目录必须是同一个资源所有者，否则就是两个互不知晓的写者"
    );
}

#[test]
fn different_directories_do_not_share_a_lock() {
    let a = tempdir().expect("a");
    let b = tempdir().expect("b");
    let (a, b) = (
        a.path().canonicalize().expect("canonical a"),
        b.path().canonicalize().expect("canonical b"),
    );
    assert!(
        !Arc::ptr_eq(&runtime_for(&a), &runtime_for(&b)),
        "两个目录共用一把锁，hub 里一个项目打补丁会把另一个项目堵住"
    );
}

/// 大小写不敏感的文件系统上，`.../foo` 和 `.../FOO` 打开的是同一个目录。
///
/// 这条钉的是"按规范化路径发锁够不够"：实测 macOS 和 Windows 的
/// `canonicalize` 会把拼法纠正成磁盘上的真名，所以够。哪天哪个平台原样
/// 返回大小写了，这条会红——那时就得换成文件系统给的目录身份（Unix 的
/// `dev`+`ino`），并接受它在目录删了重建时会分裂的代价。
///
/// 大小写敏感的文件系统（多数 Linux）上没有别名可言，自动跳过。
#[test]
fn a_different_spelling_of_the_same_directory_is_not_a_second_writer() {
    let dir = tempdir().expect("tempdir");
    let root = dir.path().canonicalize().expect("canonical");
    let lower = root.join("casecheck");
    std::fs::create_dir(&lower).expect("建目录");
    let upper = root.join("CASECHECK");
    if !upper.is_dir() {
        return;
    }

    assert_eq!(
        upper.canonicalize().expect("canonical upper"),
        lower,
        "canonicalize 没把大小写纠正回来，路径当身份就不够用了"
    );
    assert!(
        Arc::ptr_eq(&runtime_for(&lower), &runtime_for(&upper)),
        "同一个目录的两种拼法拿到了两把锁——这正是要避免的隐藏第二写者"
    );
}

/// 跨进程那一层：拿着写权的时候，别人打开同一个锁文件就该被挡在外面。
///
/// 这里用第二个 `File` 句柄冒充另一个进程——`flock` 认的是打开的文件本身
/// （open file description），不是进程，所以同一个进程里的第二个句柄一样
/// 会被挡住，测得出真实行为。
#[test]
fn a_second_process_cannot_write_while_the_lock_is_held() {
    let dir = tempdir().expect("tempdir");
    let root = dir.path().canonicalize().expect("canonical");
    let runtime = runtime_for(&root);
    let path = runtime
        .lock_file_path()
        .expect("数据目录在测试里是隔离的临时目录，锁文件应该建得出来")
        .to_path_buf();

    let held = runtime.lock_commits();
    let other = File::options()
        .read(true)
        .write(true)
        .open(&path)
        .expect("打开锁文件");
    assert!(
        FileExt::try_lock_exclusive(&other).is_err(),
        "写权被占着，另一个进程却拿到了锁——两个 gld 会同时往一个目录里写"
    );

    drop(held);
    assert!(
        FileExt::try_lock_exclusive(&other).is_ok(),
        "写权放开了，别的进程还是拿不到锁"
    );
    let _ = FileExt::unlock(&other);
}

/// 同一个主体不管从哪个上下文进来，拿到的是同一张会话表；换个主体就是
/// 另一张。前半条是"上下文重建不丢 session_id"，后半条是"别人的命令你
/// 看不见"。
#[test]
fn one_caller_keeps_its_session_table_and_another_caller_gets_a_different_one() {
    let dir = tempdir().expect("tempdir");
    let root = dir.path().canonicalize().expect("canonical");
    let runtime = runtime_for(&root);
    let mine = caller("alpha");
    let yours = caller("beta");

    assert!(
        Arc::ptr_eq(&runtime.sessions_for(&mine), &runtime.sessions_for(&mine)),
        "同一个主体拿到了两张表——上下文一重建，他的 session_id 就查不到了"
    );
    assert!(
        !Arc::ptr_eq(&runtime.sessions_for(&mine), &runtime.sessions_for(&yours)),
        "两个主体共用一张表，A 拿着 B 的 session_id 就能读 B 的命令输出"
    );
}

/// 同一个主体在两个目录上也是两张表：会话跟着目录走，不是跟着人走。
#[test]
fn one_caller_on_two_directories_gets_two_session_tables() {
    let a = tempdir().expect("a");
    let b = tempdir().expect("b");
    let a = runtime_for(&a.path().canonicalize().expect("canonical a"));
    let b = runtime_for(&b.path().canonicalize().expect("canonical b"));
    let who = caller("alpha");

    assert!(
        !Arc::ptr_eq(&a.sessions_for(&who), &b.sessions_for(&who)),
        "两个目录共用一张会话表，在 A 里起的命令会出现在 B 的会话列表里"
    );
}

/// 停一个入口只停那个入口的命令：hub 停掉了，命令行起的命令还得跑着。
///
/// 真起两个 `sleep`，不然测不出区别——`terminate_all` 对空表本来就无事可做，
/// 挑错表也看不出来。
#[cfg(unix)]
#[test]
fn stopping_one_entry_leaves_the_other_entries_alone() {
    let dir = tempdir().expect("tempdir");
    let root = dir.path().canonicalize().expect("canonical");
    let runtime = runtime_for(&root);
    let (hub, local) = (caller("alpha"), Caller::local());
    let hub_table = runtime.sessions_for(&hub);
    let local_table = runtime.sessions_for(&local);
    let hub_session = sleeping_session(&hub_table);
    let local_session = sleeping_session(&local_table);

    assert_eq!(
        runtime.terminate_sessions_in_scope("hub"),
        1,
        "停 hub 这个入口，停掉的不是 hub 那条命令"
    );
    assert!(hub_table.get(&hub_session).is_err(), "hub 的命令没被停掉");
    assert!(
        local_table.get(&local_session).is_ok(),
        "停 hub 把命令行起的命令一起杀了"
    );

    assert_eq!(runtime.terminate_all_sessions(), 1, "剩下那条命令没被停掉");
    assert!(local_table.get(&local_session).is_err());
}

/// 往表里塞一条真在跑的命令，返回它的 `session_id`。
#[cfg(unix)]
fn sleeping_session(store: &SessionStore) -> String {
    use crate::session::ExecSession;
    let child = crate::async_rt::block_on(async {
        tokio::process::Command::new("sleep")
            .arg("30")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("起 sleep")
    });
    store.insert(ExecSession::new(child)).session_id.clone()
}

/// 空表会被顺手清掉，不然按 OAuth client_id 分表的守护进程跑几天就攒一堆
/// 空壳。清的只能是空表——还挂着会话的表扔了，那些命令就没人能停了。
#[test]
fn empty_session_tables_are_reclaimed() {
    let dir = tempdir().expect("tempdir");
    let root = dir.path().canonicalize().expect("canonical");
    let runtime = runtime_for(&root);

    for id in ["alpha", "beta", "gamma"] {
        runtime.sessions_for(&caller(id));
    }
    assert_eq!(
        runtime.session_tables(),
        0,
        "三张表都是空的，一张都不该留着"
    );
}

/// 测试里的网络主体：一个 hub 上的 OAuth 客户端。
fn caller(client_id: &str) -> Caller {
    Caller::from_auth(&gld_policy::AuthContext::new(
        gld_policy::Principal::OAuthClient {
            client_id: client_id.into(),
        },
        "hub",
    ))
}

/// 写权被占着的时候，后来的人等到上限就得到 `None`，不是一直挂着。
///
/// 这条是 `exec_command` 占写权之后才要紧的：命令能跑十分钟，补丁挂在那儿
/// 等的话，客户端早超时断开了，这边还占着线程。
#[test]
fn waiting_for_a_busy_write_lock_gives_up_instead_of_hanging() {
    let dir = tempdir().expect("tempdir");
    let root = dir.path().canonicalize().expect("canonical");
    let runtime = runtime_for(&root);

    let held = runtime.lock_commits_within(Duration::from_millis(50));
    assert!(held.is_some(), "没人占着却拿不到写权");

    let started = Instant::now();
    assert!(
        runtime
            .lock_commits_within(Duration::from_millis(50))
            .is_none(),
        "写权明明被占着，第二个人却拿到了"
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "等了 {:?} 才放弃，说明根本没按上限来",
        started.elapsed()
    );

    drop(held);
    assert!(
        runtime
            .lock_commits_within(Duration::from_millis(50))
            .is_some(),
        "放开之后还是拿不到"
    );
}

/// 带过来的计数接着往下数；带来的版本表认原来那个调用方，换个人不认。
#[test]
fn adopting_a_handoff_continues_the_counter_and_the_own_writes() {
    let (old_dir, new_dir) = (tempdir().expect("old"), tempdir().expect("new"));
    let old = runtime_for(&old_dir.path().canonicalize().expect("canonical"));
    let new = runtime_for(&new_dir.path().canonicalize().expect("canonical"));
    let who = caller("alpha");
    old.record_write();
    old.record_write();
    old.record_own_write(&who, "a.txt", Some("v1"), Some("v2"));

    let cwds = BTreeMap::from([("mcp".to_string(), PathBuf::from("/somewhere"))]);
    new.adopt_handoff(old.handoff_state(cwds)).expect("接手");

    assert_eq!(new.write_counter().load(Ordering::Acquire), 2);
    assert!(new
        .counted_by
        .lock()
        .expect("lock")
        .contains_key(crate::runs::instance_id()));
    assert!(new.wrote_since(&who, "a.txt", "v1", "v2"));
    assert!(!new.wrote_since(&caller("beta"), "a.txt", "v1", "v2"));
    assert_eq!(
        new.take_adopted_cwd("mcp"),
        Some(PathBuf::from("/somewhere"))
    );
    assert_eq!(new.take_adopted_cwd("mcp"), None, "只该取到一次");
}

/// 自己写过的目录不能再接手，否则计数被盖掉，之前的命令就显得"期间没写过"。
/// 上次等回复超时、其实接住了的那一份可以再接一次。
#[test]
fn a_runtime_that_did_its_own_work_refuses_a_handoff() {
    let dir = tempdir().expect("dir");
    let runtime = runtime_for(&dir.path().canonicalize().expect("canonical"));
    let state = || WorkspaceHandoff {
        writes: 5,
        ..WorkspaceHandoff::default()
    };
    runtime.adopt_handoff(state()).expect("第一次");
    runtime.adopt_handoff(state()).expect("超时重来");
    assert!(
        runtime
            .adopt_handoff(WorkspaceHandoff {
                writes: 3,
                ..WorkspaceHandoff::default()
            })
            .is_err(),
        "迟到的旧交接不能让计数倒退"
    );
    runtime.record_write();
    assert!(runtime.adopt_handoff(state()).is_err());
    assert_eq!(runtime.write_counter().load(Ordering::Acquire), 6);
}

/// 两个不同的目录各有各的锁文件，不会互相挡。
#[test]
fn two_directories_do_not_block_each_other_across_processes() {
    let a = tempdir().expect("a");
    let b = tempdir().expect("b");
    let a = runtime_for(&a.path().canonicalize().expect("canonical a"));
    let b = runtime_for(&b.path().canonicalize().expect("canonical b"));
    assert_ne!(
        a.lock_file_path(),
        b.lock_file_path(),
        "两个目录共用一个锁文件，一个项目落盘会把另一个项目堵在外面"
    );

    let _held = a.lock_commits();
    // b 的写权照样拿得到——拿不到这里会一直等，测试超时就是答案。
    let _also = b.lock_commits();
}

/// 显式存储目录是首次创建时的输入；不会因后续上下文换配置而产生第二个写者。
fn runtime_for(root: &Path) -> Arc<WorkspaceRuntime> {
    super::runtime_for(
        root,
        RuntimePaths {
            write_lock_dir: Some(root.join("runtime-locks")),
            runs: None,
        },
    )
}
