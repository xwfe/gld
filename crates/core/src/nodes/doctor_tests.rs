use super::*;

#[test]
fn every_failure_says_how_to_fix_it() {
    for outcome in [
        HubProbe::Rejected(401),
        HubProbe::Rejected(404),
        HubProbe::Rejected(502),
        HubProbe::Rejected(403),
        HubProbe::WrongHub,
        HubProbe::Tls,
        HubProbe::Proxy("node proxy p:1 refused".into()),
        HubProbe::Timeout,
    ] {
        let label = format!("{outcome:?}");
        let check = probe_check(outcome, false);
        assert_eq!(check.level, DoctorLevel::Fail, "{label}");
        assert!(!check.fix.is_empty(), "{label}");
    }
}

#[test]
fn the_common_causes_point_at_the_real_fix() {
    assert!(probe_check(HubProbe::Rejected(401), false)
        .fix
        .contains("gld leave"));
    assert!(probe_check(HubProbe::Rejected(404), false)
        .fix
        .contains("/nodes/v1/"));
    assert!(probe_check(HubProbe::Tls, false)
        .fix
        .contains("--ca-certificate"));
}

#[test]
fn a_busy_connection_is_only_fine_when_this_machine_holds_it() {
    assert_eq!(probe_check(HubProbe::Busy, true).level, DoctorLevel::Ok);
    assert_eq!(probe_check(HubProbe::Busy, false).level, DoctorLevel::Warn);
    assert_eq!(
        probe_check(HubProbe::Connected, false).level,
        DoctorLevel::Ok
    );
}

#[test]
fn a_missing_log_is_not_reported_and_only_the_tail_is_shown() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("node.json");
    assert!(recent_log(&config).is_none());
    let lines: String = (0..20).map(|n| format!("line {n}\n")).collect();
    std::fs::write(log_path(&config), lines).unwrap();
    let shown = recent_log(&config).unwrap();
    assert!(shown.contains("line 19"));
    assert!(shown.contains("line 15"));
    assert!(!shown.contains("line 14"));
}
