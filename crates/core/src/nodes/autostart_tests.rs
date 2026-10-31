use super::*;

fn spec() -> Spec {
    Spec {
        label: "dev.gld.node.0123456789ab".into(),
        exe: PathBuf::from("/opt/my tools/gld"),
        config: PathBuf::from("/home/a & b/nodes/hub.json"),
        env: vec![
            ("PATH".into(), "/usr/bin:/opt/my tools".into()),
            ("HTTPS_PROXY".into(), "http://u:p%40ss@proxy:8080".into()),
        ],
    }
}

#[test]
fn the_label_is_stable_for_a_machine_and_safe_for_every_manager() {
    let id = "0123456789abcdef0123456789abcdef";
    assert_eq!(label(id), "dev.gld.node.0123456789ab");
    assert_eq!(label(id), label(id));
    assert_ne!(label(id), label("ffffffffffffcdef0123456789abcdef"));
}

#[test]
fn the_launchd_definition_restarts_only_after_a_crash_and_escapes_paths() {
    let plist = launchd_plist(&spec());
    assert!(plist.contains("<string>/opt/my tools/gld</string>"));
    assert!(plist.contains("<string>/home/a &amp; b/nodes/hub.json</string>"));
    assert!(plist.contains("<string>--service</string>"));
    assert!(plist.contains("<key>SuccessfulExit</key><false/>"));
    assert!(plist.contains("<key>ThrottleInterval</key><integer>60</integer>"));
    assert!(plist.contains("<key>PATH</key><string>/usr/bin:/opt/my tools</string>"));
    assert!(plist.contains(
        "<key>StandardErrorPath</key><string>/home/a &amp; b/nodes/hub.json.log</string>"
    ));
    assert!(!plist.contains("a & b"));
}

#[test]
fn the_systemd_unit_quotes_everything_systemd_would_expand() {
    let unit = systemd_unit(&spec());
    assert!(unit.contains(
        "ExecStart=\"/opt/my tools/gld\" \"node\" \"run\" \"--config\" \"/home/a & b/nodes/hub.json\" \"--service\"\n"
    ));
    // `%` 不转义的话 systemd 把 %40 当说明符，代理密码就错了。
    assert!(unit.contains("Environment=\"HTTPS_PROXY=http://u:p%%40ss@proxy:8080\"\n"));
    assert!(unit.contains("Restart=on-failure\n"));
    assert!(unit.contains("WantedBy=default.target\n"));
    assert_eq!(systemd_quote(r#"a"b\c$d"#), r#""a\"b\\c$$d""#);
    // Environment 不展开变量，这里多转义一个 `$` 就改了值。
    assert_eq!(systemd_environment("p=a$b%c"), r#""p=a$b%%c""#);
}

#[test]
fn the_windows_task_runs_as_this_account_without_elevation() {
    let xml = task_xml(&spec(), "HOST\\me & you");
    assert!(xml.contains("<UserId>HOST\\me &amp; you</UserId>"));
    assert!(xml.contains("<RunLevel>LeastPrivilege</RunLevel>"));
    assert!(xml.contains("<LogonType>InteractiveToken</LogonType>"));
    assert!(xml.contains("<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>"));
    assert!(xml.contains("<Command>/opt/my tools/gld</Command>"));
    assert!(xml.contains("&quot;/home/a &amp; b/nodes/hub.json&quot;"));
    assert!(xml.contains("&quot;--service&quot;"));
}

#[cfg(unix)]
#[test]
fn definitions_are_private_and_replace_the_old_one_atomically() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("agents/unit.plist");
    write_definition(Some(&path), "first").unwrap();
    write_definition(Some(&path), "second").unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "second");
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(!dir.path().join("agents/unit.plist.tmp").exists());
    remove_definition(Some(&path)).unwrap();
    remove_definition(Some(&path)).unwrap();
}
