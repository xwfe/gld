use super::*;

/// 工作区和保护目录都来自显式输入，测试不读取进程的 GLD_HOME。
fn workspace_with_fake_secret_store() -> (tempfile::TempDir, Workspace, PathBuf) {
    let dir = tempfile::tempdir().expect("workspace and protected root");
    let root = dir.path().join("project");
    let home = dir.path().join("data-home");
    std::fs::create_dir_all(&root).expect("workspace directory");
    std::fs::create_dir_all(&home).expect("protected directory");
    let secrets = home.join("profiles.json");
    std::fs::write(&secrets, r#"{"workspace_secrets":{}}"#).expect("write");
    std::fs::write(root.join("main.rs"), "fn main() {}\n").expect("file");
    let workspace = Workspace::new(root, vec![home]).expect("workspace");
    (dir, workspace, secrets)
}

/// gld 自己的数据目录必须挡住，**哪怕越界读已经被放开**：
/// 那里是所有工作区的 bearer_token / oauth_password 明文。
///
/// 特意关掉 confine_reads 才是这条的意义所在——开着的话它被外层拦掉，
/// 测不到数据目录这道独立的门。
#[test]
fn reading_the_gld_data_home_is_denied_even_with_confinement_off() {
    let (_dir, workspace, secrets) = workspace_with_fake_secret_store();
    let workspace = workspace.with_confined_reads(false);

    let error = workspace
        .resolve_read_path(&secrets.display().to_string())
        .expect_err("必须拒绝");

    assert!(
        matches!(error, WorkspaceError::Tool { code, .. } if code == "GLD_DATA_HOME_DENIED"),
        "错误码不对: {error:?}"
    );
}

/// 默认（confine_reads=true）下，读 Workspace 外面就被挡在门口。
#[test]
fn confined_reads_are_the_default_and_block_absolute_paths() {
    let (_dir, workspace, _) = workspace_with_fake_secret_store();
    let outside = tempfile::tempdir().expect("outside");
    let file = outside.path().join("neighbour.txt");
    std::fs::write(&file, "ok\n").expect("write");

    let error = workspace
        .resolve_read_path(&file.display().to_string())
        .expect_err("默认就该拒绝");
    let message = error.message();

    assert!(message.contains("只允许读 Workspace 内的文件"), "{message}");
    // 报错要给出关掉的办法：读隔壁仓库是正当需求。
    assert!(message.contains("confine-reads=false"), "{message}");
}

/// Workspace 里指向外面的软链也算越界——比的是 canonicalize 之后的路径。
#[cfg(unix)]
#[test]
fn a_symlink_pointing_outside_does_not_slip_past_confinement() {
    let (_dir, workspace, _) = workspace_with_fake_secret_store();
    let outside = tempfile::tempdir().expect("outside");
    let target = outside.path().join("secret.txt");
    std::fs::write(&target, "outside\n").expect("write");
    std::os::unix::fs::symlink(&target, workspace.root().join("link.txt")).expect("symlink");

    assert!(workspace.resolve_read_path("link.txt").is_err());
}

/// 遍历类工具从外面走进数据目录也要挡住——
/// resolve_read_path 只校验起点，WalkDir 之后会自己走下去。
#[test]
fn walking_into_the_gld_data_home_is_skipped() {
    let (_dir, workspace, secrets) = workspace_with_fake_secret_store();

    assert!(
        workspace.is_ignored_path(&secrets, true, true),
        "include_hidden=true 也不能把数据目录搜出来"
    );
}

/// 关掉 confine_reads 之后，Workspace 外的普通路径要读得到——
/// 这个开关得真的有用，不能只是把"一律拒绝"换个错误码。
#[test]
fn turning_confinement_off_restores_reads_outside_the_workspace() {
    let (_dir, workspace, _) = workspace_with_fake_secret_store();
    let workspace = workspace.with_confined_reads(false);
    let outside = tempfile::tempdir().expect("outside");
    let file = outside.path().join("neighbour.txt");
    std::fs::write(&file, "ok\n").expect("write");

    assert!(workspace
        .resolve_read_path(&file.display().to_string())
        .is_ok());
    assert!(!workspace.is_ignored_path(&file, false, false));
}

#[cfg(unix)]
#[test]
fn protected_roots_block_both_original_and_canonical_paths() {
    let (dir, workspace, secrets) = workspace_with_fake_secret_store();
    let home = secrets.parent().unwrap();
    let alias = dir.path().join("protected-alias");
    std::os::unix::fs::symlink(home, &alias).expect("symlink");
    let workspace = Workspace::new(workspace.root().to_path_buf(), vec![alias.clone()])
        .expect("workspace")
        .with_confined_reads(false);
    for path in [alias.join("profiles.json"), secrets.canonicalize().unwrap()] {
        assert!(workspace.is_ignored_path(&path, true, true));
        assert_eq!(
            workspace
                .resolve_read_path(&path.to_string_lossy())
                .unwrap_err()
                .code(),
            "GLD_DATA_HOME_DENIED"
        );
    }
}

#[test]
fn an_explicit_workspace_inside_a_protected_root_remains_readable() {
    let (_dir, _workspace, secrets) = workspace_with_fake_secret_store();
    let home = secrets.parent().unwrap().to_path_buf();
    let workspace = Workspace::new(home.clone(), vec![home]).expect("workspace");
    let file = workspace
        .resolve_read_path("profiles.json")
        .expect("explicit workspace");
    assert!(!workspace.is_ignored_path(&file.path, true, true));
}
