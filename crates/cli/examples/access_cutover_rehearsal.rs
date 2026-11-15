//! Historical acceptance tool. Run this Cargo example with OLD_BIN NEW_BIN arguments.
//! Both binaries must be absolute paths. All configuration and captured output are temporary.
use std::collections::BTreeMap;
use std::error::Error;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use gld_core::auth::{
    AccessRecord, AccessStore, CredentialKind, CredentialRecord, Grant, GrantPermissions,
    GrantStatus, ProjectScope,
};
use serde_json::Value;
use sha2::{Digest, Sha256};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

#[derive(Debug)]
struct CheckError(&'static str);

impl std::fmt::Display for CheckError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.0)
    }
}

impl Error for CheckError {}

fn require(condition: bool, message: &'static str) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(Box::new(CheckError(message)))
    }
}

fn command(binary: &Path, home: &Path, project: &Path) -> Command {
    let mut command = Command::new(binary);
    command
        .env("GLD_HOME", home)
        .env("NO_COLOR", "1")
        .current_dir(project);
    // Do not inherit service credentials, routing, or generation control from the caller.
    for (key, _) in std::env::vars_os() {
        let name = key.to_string_lossy();
        if (name.starts_with("GLD_") && name != "GLD_HOME")
            || matches!(
                name.as_ref(),
                "TUNNEL_TOKEN"
                    | "CONTROL_PLANE_API_KEY"
                    | "HTTP_PROXY"
                    | "HTTPS_PROXY"
                    | "ALL_PROXY"
                    | "http_proxy"
                    | "https_proxy"
                    | "all_proxy"
            )
        {
            command.env_remove(key);
        }
    }
    command
}

fn run(binary: &Path, home: &Path, project: &Path, args: &[&str]) -> Result<Output> {
    // Files avoid filling a pipe while waiting for a child. They are never printed.
    let stdout = tempfile::tempfile()?;
    let stderr = tempfile::tempfile()?;
    let mut child = command(binary, home, project)
        .args(args)
        .stdin(Stdio::null())
        .stdout(stdout.try_clone()?)
        .stderr(stderr.try_clone()?)
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(45);
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err("command timed out".into());
        }
        thread::sleep(Duration::from_millis(25));
    };
    use std::io::{Read, Seek, SeekFrom};
    let read = |mut file: File| -> Result<Vec<u8>> {
        file.seek(SeekFrom::Start(0))?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        Ok(bytes)
    };
    Ok(Output {
        status,
        stdout: read(stdout)?,
        stderr: read(stderr)?,
    })
}

fn ok(binary: &Path, home: &Path, project: &Path, args: &[&str]) -> Result<Output> {
    let output = run(binary, home, project, args)?;
    require(output.status.success(), "command failed")?;
    Ok(output)
}

fn json(binary: &Path, home: &Path, project: &Path, args: &[&str]) -> Result<Value> {
    Ok(serde_json::from_slice(
        &ok(binary, home, project, args)?.stdout,
    )?)
}

fn data(home: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(
        home.join("data/profiles.json"),
    )?)?)
}

fn digest(path: &Path) -> Result<Vec<u8>> {
    Ok(Sha256::digest(fs::read(path)?).to_vec())
}

fn tree(root: &Path) -> Result<BTreeMap<PathBuf, Vec<u8>>> {
    fn visit(root: &Path, path: &Path, found: &mut BTreeMap<PathBuf, Vec<u8>>) -> Result<()> {
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            require(!kind.is_symlink(), "unexpected symlink in fixture")?;
            if kind.is_dir() {
                visit(root, &entry.path(), found)?;
            } else {
                found.insert(
                    entry.path().strip_prefix(root)?.to_path_buf(),
                    digest(&entry.path())?,
                );
            }
        }
        Ok(())
    }
    let mut found = BTreeMap::new();
    visit(root, root, &mut found)?;
    Ok(found)
}

fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        require(
            !entry.file_type()?.is_symlink(),
            "unexpected fixture symlink",
        )?;
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &to.join(entry.file_name()))?;
        } else {
            fs::copy(entry.path(), to.join(entry.file_name()))?;
        }
    }
    Ok(())
}

fn record(id: &str, name: &str, revision: u64) -> Result<AccessRecord> {
    Ok(AccessRecord {
        grant: Grant {
            id: id.into(),
            name: name.into(),
            scope: ProjectScope::AllProjects,
            permissions: GrantPermissions::default(),
            status: GrantStatus::Active,
        },
        credential: CredentialRecord::prepare(
            format!("credential-{id}"),
            CredentialKind::Token,
            if revision == 1 {
                "synthetic-original-secret"
            } else {
                "synthetic-replacement-secret"
            },
            revision,
            100,
            Some(3600),
            Vec::new(),
        )?,
    })
}

fn no_daemon(binary: &Path, home: &Path, project: &Path) -> Result<()> {
    require(
        run(binary, home, project, &["--json", "daemon", "status"])?
            .status
            .code()
            == Some(3),
        "fixture unexpectedly has a daemon",
    )?;
    require(
        !home.join("daemon.json").exists(),
        "unexpected daemon record",
    )
}

fn storage(old: &Path, new: &Path, root: &Path, project: &Path) -> Result<()> {
    let home = root.join("storage");
    fs::create_dir(&home)?;
    no_daemon(old, &home, project)?;
    ok(
        old,
        &home,
        project,
        &["--no-autostart", "add", ".", "--name", "fixture"],
    )?;
    ok(
        old,
        &home,
        project,
        &["--no-autostart", "grant", "add", "legacy", "fixture"],
    )?;
    let legacy = data(&home)?["grants"].clone();
    require(
        legacy.as_array().is_some_and(|rows| rows.len() == 1),
        "old grant was not created",
    )?;
    let backup = root.join("backup");
    copy_tree(&home, &backup)?;
    let backup_hash = tree(&backup)?;
    let store = AccessStore::open_in(&home)?;
    require(
        store.snapshot()?.records().is_empty(),
        "old grants were imported",
    )?;
    store.insert(record("one", "Current", 1)?)?;
    store.insert(record("two", "Revoked", 1)?)?;
    let reader = store.reader();
    require(
        reader
            .authenticate_token("hub", "credential-one", "synthetic-original-secret", 200)
            .is_ok(),
        "original token did not authenticate",
    )?;
    let before = reader.snapshot()?;
    let access = home.join("access/v1/grants.json");
    let bytes = fs::read(&access)?;
    ok(
        old,
        &home,
        project,
        &["--no-autostart", "settings", "proxy", "--mode", "none"],
    )?;
    require(
        data(&home)?["proxy"]["mode"] == "none",
        "old writer did not save",
    )?;
    require(
        data(&home)?["grants"] == legacy,
        "old writer changed legacy grants",
    )?;
    require(
        fs::read(&access)? == bytes,
        "old writer changed new storage",
    )?;
    require(
        std::sync::Arc::ptr_eq(&before, &reader.snapshot()?),
        "old writer changed reader",
    )?;
    store.update(record("one", "Current", 2)?)?;
    store.revoke("two")?;
    require(
        reader
            .authenticate_token("hub", "credential-one", "synthetic-original-secret", 200)
            .is_err(),
        "old token survived rotation",
    )?;
    require(
        reader
            .authenticate_token("hub", "credential-one", "synthetic-replacement-secret", 200)
            .is_ok(),
        "replacement token did not authenticate",
    )?;
    require(
        reader
            .authenticate_token("hub", "credential-two", "synthetic-original-secret", 200)
            .is_err(),
        "revoked token still authenticates",
    )?;
    let committed = fs::read(&access)?;
    ok(
        old,
        &home,
        project,
        &["--no-autostart", "settings", "proxy", "--mode", "system"],
    )?;
    require(
        data(&home)?["proxy"]["mode"] == "system",
        "second old write did not save",
    )?;
    require(
        fs::read(&access)? == committed,
        "old writer changed committed state",
    )?;
    drop(before);
    drop(reader);
    drop(store);
    let store = AccessStore::open_in(&home)?;
    let reopened = store.reader();
    require(
        reopened
            .authenticate_token("hub", "credential-one", "synthetic-original-secret", 200)
            .is_err(),
        "old token revived after restart",
    )?;
    require(
        reopened
            .authenticate_token("hub", "credential-one", "synthetic-replacement-secret", 200)
            .is_ok(),
        "replacement token failed after restart",
    )?;
    require(
        reopened
            .authenticate_token("hub", "credential-two", "synthetic-original-secret", 200)
            .is_err(),
        "revoked token revived after restart",
    )?;
    let snapshot = store.snapshot()?;
    require(
        snapshot
            .by_grant_id("one")
            .is_some_and(|r| r.credential.revision == 2),
        "rotation did not survive restart",
    )?;
    require(
        snapshot
            .by_grant_id("two")
            .is_some_and(|r| r.grant.status == GrantStatus::Revoked),
        "revocation did not survive restart",
    )?;
    drop(snapshot);
    drop(reopened);
    drop(store);
    fs::write(&access, b"{truncated")?;
    ok(
        old,
        &home,
        project,
        &["--no-autostart", "settings", "proxy", "--mode", "none"],
    )?;
    require(
        AccessStore::open_in(&home).is_err(),
        "corrupt new storage fell back",
    )?;
    require(
        fs::read(&access)? == b"{truncated",
        "corrupt storage was rewritten",
    )?;
    no_daemon(old, &home, project)?;
    require(tree(&backup)? == backup_hash, "backup original changed")?;
    println!("PASS W1-W4: real legacy writer, isolated store, restart and corruption");

    // A backup predating a revocation must be reconciled before it is eligible to start.
    let revoked_id = legacy[0]["id"].as_str().ok_or("missing legacy grant ID")?;
    ok(
        old,
        &home,
        project,
        &["--no-autostart", "grant", "rm", "legacy"],
    )?;
    let restored = root.join("restored");
    copy_tree(&backup, &restored)?;
    require(
        data(&restored)?["grants"] == legacy,
        "backup did not restore old grants",
    )?;
    require(
        data(&restored)?["grants"][0]["id"] == revoked_id,
        "revocation manifest does not match restored grant",
    )?;
    ok(
        old,
        &restored,
        project,
        &["--no-autostart", "grant", "rm", "legacy"],
    )?;
    let remaining = data(&restored)?["grants"].clone();
    require(
        remaining.as_array().is_some_and(Vec::is_empty),
        "revoked grant survived restore",
    )?;
    ok(
        old,
        &restored,
        project,
        &["--no-autostart", "settings", "proxy", "--mode", "none"],
    )?;
    require(
        data(&restored)?["profiles"] == data(&backup)?["profiles"],
        "restored projects changed",
    )?;
    let restored_data_hash = digest(&restored.join("data/profiles.json"))?;
    ok(new, &restored, project, &["--no-autostart", "grant", "ls"])?;
    require(
        digest(&restored.join("data/profiles.json"))? == restored_data_hash,
        "new metadata read rewrote restored data",
    )?;
    require(
        !restored.join("access/v1/grants.json").exists(),
        "restored grants were imported",
    )?;
    require(
        tree(&backup)? == backup_hash,
        "backup original changed during restore",
    )?;
    no_daemon(old, &restored, project)?;
    println!("PASS R1-R2: immutable backup, isolated restore, revocation reconciliation");
    Ok(())
}

struct Daemon<'a> {
    binary: &'a Path,
    home: PathBuf,
    project: &'a Path,
}
impl Drop for Daemon<'_> {
    fn drop(&mut self) {
        let _ = run(
            self.binary,
            &self.home,
            self.project,
            &["daemon", "stop", "--force", "--wait", "5"],
        );
    }
}

fn unchanged(
    binary: &Path,
    home: &Path,
    project: &Path,
    pid: &Value,
    current: &Value,
    config: &[u8],
) -> Result<()> {
    let status = json(binary, home, project, &["--json", "daemon", "status"])?;
    require(
        status["info"]["pid"] == *pid && status["running"] == true,
        "daemon process changed",
    )?;
    let generation = json(
        binary,
        home,
        project,
        &["--json", "daemon", "upgrade", "status"],
    )?;
    require(
        generation["current"] == current["current"] && generation["prepared"].is_null(),
        "active or prepared generation changed",
    )?;
    require(
        generation["generations"].as_array().is_some_and(|rows| {
            rows.iter()
                .any(|row| row["id"] == current["id"] && row["pid"] == current["pid"])
        }),
        "current generation process changed",
    )?;
    require(
        digest(&home.join("data/profiles.json"))? == config,
        "rejected command changed configuration",
    )
}

fn boundary(owner: &Path, other: &Path, protocol: u64, root: &Path, project: &Path) -> Result<()> {
    let home = root.join(format!("daemon-{protocol}"));
    fs::create_dir(&home)?;
    ok(
        owner,
        &home,
        project,
        &["--no-autostart", "settings", "proxy", "--mode", "none"],
    )?;
    let daemon = Daemon {
        binary: owner,
        home,
        project,
    };
    ok(owner, &daemon.home, project, &["--json", "daemon", "start"])?;
    let status = json(
        owner,
        &daemon.home,
        project,
        &["--json", "daemon", "status"],
    )?;
    require(
        status["version_matches_cli"] == true && status["info"]["protocol"] == protocol,
        "wrong daemon protocol or binary",
    )?;
    let pid = status["info"]["pid"].clone();
    let generation = json(
        owner,
        &daemon.home,
        project,
        &["--json", "daemon", "upgrade", "status"],
    )?;
    require(
        !generation["current"].is_null(),
        "daemon has no current generation",
    )?;
    let active = generation["generations"]
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["id"] == generation["current"]))
        .ok_or("current generation row missing")?;
    let current = serde_json::json!({"current": generation["current"],
        "id": active["id"], "pid": active["pid"]});
    let config = digest(&daemon.home.join("data/profiles.json"))?;
    let ordinary = run(
        other,
        &daemon.home,
        project,
        &["--no-autostart", "settings", "proxy", "--mode", "system"],
    )?;
    require(
        ordinary.status.code() == Some(4),
        "cross-version command was not rejected",
    )?;
    unchanged(owner, &daemon.home, project, &pid, &current, &config)?;
    let exe = other.to_str().ok_or("binary path must be UTF-8")?;
    let cross = run(
        other,
        &daemon.home,
        project,
        &["daemon", "upgrade", "prepare", exe, "--timeout", "5"],
    )?;
    require(
        !cross.status.success() && String::from_utf8_lossy(&cross.stderr).contains("协议"),
        "cross-version upgrade was not rejected",
    )?;
    unchanged(owner, &daemon.home, project, &pid, &current, &config)?;
    // Matching CLI reaches the host, which must reject the incompatible generation handshake.
    let prepare = run(
        owner,
        &daemon.home,
        project,
        &["daemon", "upgrade", "prepare", exe, "--timeout", "5"],
    )?;
    require(
        !prepare.status.success(),
        "incompatible generation was prepared",
    )?;
    let failure = String::from_utf8_lossy(&prepare.stderr);
    require(
        failure.contains("protocol") || failure.contains("协议"),
        "generation failed without a protocol rejection",
    )?;
    unchanged(owner, &daemon.home, project, &pid, &current, &config)?;
    ok(
        owner,
        &daemon.home,
        project,
        &["daemon", "stop", "--wait", "5"],
    )?;
    no_daemon(owner, &daemon.home, project)?;
    println!("PASS daemon {protocol}: cross CLI and incompatible generation rejected");
    Ok(())
}

fn rehearsal() -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    require(args.len() == 2, "expected OLD_BIN NEW_BIN")?;
    let old = PathBuf::from(&args[0]);
    let new = PathBuf::from(&args[1]);
    require(
        old.is_absolute() && new.is_absolute() && old.is_file() && new.is_file(),
        "both binaries must be existing absolute paths",
    )?;
    require(
        old.canonicalize()? != new.canonicalize()?,
        "binaries must be distinct",
    )?;
    let temp = tempfile::tempdir()?;
    let project = temp.path().join("project");
    fs::create_dir(&project)?;
    fs::write(project.join("README.md"), "synthetic cutover fixture\n")?;
    storage(&old, &new, temp.path(), &project)?;
    boundary(&old, &new, 18, temp.path(), &project)?;
    boundary(&new, &old, 19, temp.path(), &project)?;
    println!("PASS access cutover rehearsal");
    Ok(())
}

fn main() -> ExitCode {
    match rehearsal() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            // Child output can contain credentials and local paths. Never echo it.
            let reason = error
                .downcast_ref::<CheckError>()
                .map(|error| error.0)
                .unwrap_or("operation failed; external error details suppressed");
            eprintln!("FAIL access cutover rehearsal: {reason}");
            ExitCode::FAILURE
        }
    }
}
