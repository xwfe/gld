use super::*;
use serde_json::{json, Value};

const NOW: u64 = 1_800_000_000;

fn request() -> PairRequest {
    PairRequest {
        request_id: "a".repeat(32),
        credential: "node-only-credential-".repeat(3),
    }
}

fn pair_at(
    data: &mut AppData,
    token: &str,
    request: &PairRequest,
    now: u64,
) -> Result<RegisteredMachine, PairError> {
    let mut changed = false;
    super::pair_in(data, token, request, now, &mut changed)
}

fn reload(data: &AppData) -> AppData {
    serde_json::from_slice(&serde_json::to_vec(data).unwrap()).unwrap()
}

/// 只改秘密段（第三段）；Hub 公钥是最后一段，改它测的是另一条拒绝路径。
fn wrong_secret(token: &str) -> String {
    let mut parts: Vec<String> = token.split('.').map(str::to_owned).collect();
    let secret = &mut parts[2];
    let last = secret.pop().unwrap();
    secret.push(if last == 'a' { 'b' } else { 'a' });
    parts.join(".")
}

#[test]
fn invitation_pins_the_hub_key_and_a_substituted_key_is_rejected_without_using_it() {
    let mut data = AppData::default();
    let invitation = invite_in(&mut data, "keyed-node", NOW).unwrap();
    let (_, pinned) = invitation_identity(&invitation.invitation).unwrap();
    let stored = data.node_hub_key.clone();
    assert!(!stored.is_empty() && !invitation.invitation.contains(&stored));

    let impostor = super::super::hub_key::HubKey::from_stored(
        &super::super::hub_key::generate_private_key().unwrap(),
    )
    .unwrap()
    .public_key();
    let (prefix, _) = invitation.invitation.rsplit_once('.').unwrap();
    let substituted = format!("{prefix}.{impostor}");
    assert_eq!(
        pair_at(&mut data, &substituted, &request(), NOW).unwrap_err(),
        PairError::InvalidInvitation
    );
    assert_eq!(data.node_invitations[0].failed_attempts, 0);

    let paired = pair_at(&mut data, &invitation.invitation, &request(), NOW).unwrap();
    assert_eq!(paired.hub_public_key, pinned);
    assert_eq!(
        data.node_hub_key, stored,
        "pairing must not rotate the Hub key"
    );
    let second = invite_in(&mut data, "second-node", NOW).unwrap();
    assert_eq!(invitation_identity(&second.invitation).unwrap().1, pinned);
}

#[test]
fn invitation_has_a_fixed_lifetime_and_only_its_digest_is_stored() {
    let mut data = AppData::default();
    let invitation = invite_in(&mut data, " build-node ", NOW).unwrap();
    assert_eq!(invitation.expires_at, NOW + 600);
    assert_eq!(invitation.name, "build-node");
    assert_eq!(
        invitation_identity(&invitation.invitation).unwrap(),
        (
            data.node_hub_id.clone(),
            config::hub_public_key(&data).unwrap()
        )
    );
    let parts: Vec<_> = invitation.invitation.split('.').collect();
    assert_eq!(
        parts.iter().map(|part| part.len()).collect::<Vec<_>>(),
        [32, 32, 64, 64]
    );
    assert_eq!(data.node_invitations.len(), 1);
    assert_eq!(
        data.node_invitations[0].invitation_sha256,
        config::credential_hash(&invitation.invitation)
    );
    let stored = serde_json::to_string(&data).unwrap();
    assert!(!stored.contains(&invitation.invitation));
    assert!(!stored.contains(parts[2]));
    assert!(!format!("{invitation:?}").contains(parts[2]));
    assert!(!format!("{:?}", data.node_invitations[0])
        .contains(&data.node_invitations[0].invitation_sha256));
    assert!(!format!("{:?}", request()).contains(&request().credential));
    assert!(config::validate_credential(&invitation.invitation).is_err());
}

#[test]
fn successful_pairing_is_single_use_and_recovers_the_same_registration() {
    let mut data = AppData::default();
    let invitation = invite_in(&mut data, "build-node", NOW).unwrap();
    let request = request();
    let first = pair_at(&mut data, &invitation.invitation, &request, NOW).unwrap();
    assert_eq!(first.machine_id, request.request_id);
    assert_eq!(first.hub_id, invitation.hub_id);
    assert_eq!(data.node_machines.len(), 1);
    assert!(data.node_invitations.is_empty());
    assert_eq!(
        data.node_machines[0].credential_sha256,
        config::credential_hash(&request.credential)
    );
    assert!(!serde_json::to_string(&data)
        .unwrap()
        .contains(&request.credential));

    let mut data = reload(&data);
    let before = serde_json::to_value(&data).unwrap();
    // 已完成登记的恢复用当前节点凭据，邀请过期或已移除不生成第二台机器。
    let recovered = pair_at(&mut data, &invitation.invitation, &request, NOW + 86_400).unwrap();
    assert_eq!(
        serde_json::to_value(first).unwrap(),
        serde_json::to_value(recovered).unwrap()
    );
    assert_eq!(before, serde_json::to_value(data).unwrap());
}

#[test]
fn consumed_invitation_cannot_add_a_machine_or_replace_its_credential() {
    let mut data = AppData::default();
    let invitation = invite_in(&mut data, "build-node", NOW).unwrap();
    let request = request();
    pair_at(&mut data, &invitation.invitation, &request, NOW).unwrap();
    let before = serde_json::to_value(&data).unwrap();
    let mut different_id = request.clone();
    different_id.request_id = "b".repeat(32);
    assert_eq!(
        pair_at(&mut data, &invitation.invitation, &different_id, NOW).unwrap_err(),
        PairError::InvalidInvitation
    );
    let mut different_credential = request;
    different_credential.credential = "new-node-credential-".repeat(3);
    assert_eq!(
        pair_at(
            &mut data,
            &invitation.invitation,
            &different_credential,
            NOW
        )
        .unwrap_err(),
        PairError::Conflict
    );
    assert_eq!(before, serde_json::to_value(data).unwrap());
}

#[test]
fn expired_invitation_and_another_hub_are_rejected_without_registration() {
    let mut data = AppData::default();
    let invitation = invite_in(&mut data, "build-node", NOW).unwrap();
    let before = serde_json::to_value(&data).unwrap();
    assert_eq!(
        pair_at(&mut data, &invitation.invitation, &request(), NOW + 600).unwrap_err(),
        PairError::InvalidInvitation
    );
    let other_hub = format!("{}{}", "0".repeat(32), &invitation.invitation[32..]);
    assert_eq!(
        pair_at(&mut data, &other_hub, &request(), NOW).unwrap_err(),
        PairError::InvalidInvitation
    );
    assert_eq!(before, serde_json::to_value(&data).unwrap());
    pair_at(&mut data, &invitation.invitation, &request(), NOW).unwrap();
    assert_eq!(
        pair_at(&mut data, &other_hub, &request(), NOW).unwrap_err(),
        PairError::InvalidInvitation
    );
}

#[test]
fn failed_secret_attempts_survive_reload_and_lock_the_invitation() {
    let mut data = AppData::default();
    let invitation = invite_in(&mut data, "build-node", NOW).unwrap();
    let wrong = wrong_secret(&invitation.invitation);
    for expected in 1..=MAX_FAILED_ATTEMPTS {
        assert_eq!(
            pair_at(&mut data, &wrong, &request(), NOW).unwrap_err(),
            PairError::InvalidInvitation
        );
        data = reload(&data);
        assert_eq!(data.node_invitations[0].failed_attempts, expected);
    }
    assert_eq!(
        pair_at(&mut data, &invitation.invitation, &request(), NOW).unwrap_err(),
        PairError::InvalidInvitation
    );
    assert!(data.node_machines.is_empty());
    assert_eq!(
        data.node_invitations[0].failed_attempts,
        MAX_FAILED_ATTEMPTS
    );
    let replacement = invite_in(&mut data, "build-node", NOW + 1).unwrap();
    assert_eq!(data.node_invitations.len(), 1);
    assert_eq!(data.node_invitations[0].failed_attempts, 0);
    assert_eq!(
        pair_at(&mut data, &invitation.invitation, &request(), NOW + 1).unwrap_err(),
        PairError::InvalidInvitation
    );
    pair_at(&mut data, &replacement.invitation, &request(), NOW + 1).unwrap();
}

#[test]
fn revoked_rotated_and_aba_credentials_cannot_recover_old_pairing() {
    let mut data = AppData::default();
    let invitation = invite_in(&mut data, "build-node", NOW).unwrap();
    let request = request();
    pair_at(&mut data, &invitation.invitation, &request, NOW).unwrap();
    for (revoked, generation, credential) in [
        (true, 1, request.credential.clone()),
        (false, 1, "rotated-node-credential-".repeat(3)),
        (false, 2, request.credential.clone()),
    ] {
        data.node_machines[0].revoked = revoked;
        data.node_machines[0].credential_generation = generation;
        data.node_machines[0].credential_sha256 = config::credential_hash(&credential);
        let before = serde_json::to_value(&data).unwrap();
        assert_eq!(
            pair_at(&mut data, &invitation.invitation, &request, NOW).unwrap_err(),
            PairError::Conflict
        );
        assert_eq!(before, serde_json::to_value(&data).unwrap());
    }
}

#[test]
fn credential_and_registration_conflicts_do_not_consume_the_invitation() {
    let mut data = AppData::default();
    let invitation = invite_in(&mut data, "build-node", NOW).unwrap();
    let mut request = request();
    data.shared_secrets
        .insert("bearer_token".into(), request.credential.clone());
    let before = serde_json::to_value(&data).unwrap();
    assert_eq!(
        pair_at(&mut data, &invitation.invitation, &request, NOW).unwrap_err(),
        PairError::Conflict
    );
    assert_eq!(before, serde_json::to_value(&data).unwrap());
    request.credential = "distinct-node-credential-".repeat(3);
    config::register_in(
        &mut data,
        "other",
        Some(&"b".repeat(32)),
        &request.credential,
    )
    .unwrap();
    assert_eq!(
        pair_at(&mut data, &invitation.invitation, &request, NOW).unwrap_err(),
        PairError::Conflict
    );
    assert_eq!(data.node_invitations.len(), 1);
    assert_eq!(data.node_machines.len(), 1);
    request.credential = "third-node-credential-".repeat(3);
    pair_at(&mut data, &invitation.invitation, &request, NOW).unwrap();
    assert_eq!(data.node_machines.len(), 2);
    assert!(data.node_invitations.is_empty());
}

#[test]
fn replacing_an_unused_name_invalidates_only_its_old_invitation() {
    let mut data = AppData::default();
    let old = invite_in(&mut data, "Build-Node", NOW).unwrap();
    let other = invite_in(&mut data, "other", NOW).unwrap();
    let replacement = invite_in(&mut data, "build-node", NOW + 30).unwrap();
    assert_eq!(data.node_invitations.len(), 2);
    assert_eq!(
        pair_at(&mut data, &old.invitation, &request(), NOW + 30).unwrap_err(),
        PairError::InvalidInvitation
    );
    pair_at(&mut data, &replacement.invitation, &request(), NOW + 30).unwrap();
    let second = PairRequest {
        request_id: "b".repeat(32),
        credential: "second-node-credential-".repeat(3),
    };
    pair_at(&mut data, &other.invitation, &second, NOW + 30).unwrap();
    assert!(invite_in(&mut data, "BUILD-NODE", NOW + 30).is_err());
    assert_eq!(data.node_machines.len(), 2);
}

#[test]
fn active_invitations_are_bounded_and_expired_entries_are_reclaimed() {
    let mut data = AppData::default();
    for index in 0..MAX_INVITATIONS {
        invite_in(&mut data, &format!("node-{index}"), NOW).unwrap();
    }
    let before = serde_json::to_value(&data).unwrap();
    assert!(invite_in(&mut data, "extra-node", NOW).is_err());
    assert_eq!(before, serde_json::to_value(&data).unwrap());
    invite_in(&mut data, "node-0", NOW).unwrap();
    assert_eq!(data.node_invitations.len(), MAX_INVITATIONS);
    invite_in(&mut data, "extra-node", NOW + 600).unwrap();
    assert_eq!(data.node_invitations.len(), 1);
}

#[test]
fn malformed_requests_and_old_configuration_are_handled_without_secret_errors() {
    let mut old = serde_json::to_value(AppData::default()).unwrap();
    old.as_object_mut().unwrap().remove("node_invitations");
    let mut data: AppData = serde_json::from_value(old).unwrap();
    assert!(data.node_invitations.is_empty());
    for name in ["", " /bad ", "bad\\name", "bad\nname"] {
        assert!(invite_in(&mut data, name, NOW).is_err());
    }
    let invitation = invite_in(&mut data, "build-node", NOW).unwrap();
    let before = serde_json::to_value(&data).unwrap();
    for invalid in [
        PairRequest {
            request_id: "short".into(),
            ..request()
        },
        PairRequest {
            credential: "short".into(),
            ..request()
        },
        PairRequest {
            credential: invitation.invitation.clone(),
            ..request()
        },
    ] {
        assert_eq!(
            pair_at(&mut data, &invitation.invitation, &invalid, NOW).unwrap_err(),
            PairError::InvalidRequest
        );
    }
    for token in [
        "".to_owned(),
        request().credential,
        format!("{}.extra", invitation.invitation),
        // 旧格式没有 Hub 公钥，不能退回只凭 TLS 认 Hub。
        invitation.invitation.rsplit_once('.').unwrap().0.to_owned(),
    ] {
        assert!(invitation_identity(&token).is_err());
    }
    assert!(serde_json::from_value::<PairRequest>(json!({
        "request_id": request().request_id, "credential": request().credential, "name": "untrusted"
    }))
    .is_err());
    assert_eq!(before, serde_json::to_value(data).unwrap());
}

#[test]
fn corrupt_invitation_registry_fails_closed() {
    let mut data = AppData::default();
    let invitation = invite_in(&mut data, "build-node", NOW).unwrap();
    data.node_invitations[0].failed_attempts = MAX_FAILED_ATTEMPTS + 1;
    let before = serde_json::to_value(&data).unwrap();
    assert_eq!(
        pair_at(&mut data, &invitation.invitation, &request(), NOW).unwrap_err(),
        PairError::Unavailable
    );
    assert!(invite_in(&mut data, "next-node", NOW).is_err());
    assert_eq!(before, serde_json::to_value(data).unwrap());
}

#[test]
fn persistent_pairing_serializes_duplicates_and_saves_failed_attempts() {
    const CHILD_PHASE: &str = "GLD_TEST_NODE_PAIRING_PHASE";
    if let Ok(phase) = std::env::var(CHILD_PHASE) {
        let home = std::path::PathBuf::from(std::env::var_os("GLD_HOME").unwrap());
        let fixture = home.join("pairing-test-fixture.json");
        if phase == "write" {
            let invitation = invite("build-node").unwrap();
            let request = request();
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
            let workers: Vec<_> = (0..2)
                .map(|_| {
                    let barrier = barrier.clone();
                    let token = invitation.invitation.clone();
                    let request = request.clone();
                    std::thread::spawn(move || {
                        barrier.wait();
                        pair(&token, request).unwrap()
                    })
                })
                .collect();
            for worker in workers {
                assert_eq!(worker.join().unwrap().machine_id, request.request_id);
            }
            let locked = invite("locked-node").unwrap();
            let wrong = wrong_secret(&locked.invitation);
            let other = PairRequest {
                request_id: "b".repeat(32),
                credential: "second-node-credential-".repeat(3),
            };
            for _ in 0..MAX_FAILED_ATTEMPTS {
                assert_eq!(
                    pair(&wrong, other.clone()).unwrap_err(),
                    PairError::InvalidInvitation
                );
            }
            std::fs::write(&fixture, serde_json::to_vec(&json!({
                "token": invitation.invitation, "request": request, "locked_token": locked.invitation,
                "locked_request": other,
            })).unwrap()).unwrap();
        } else {
            assert_eq!(phase, "read");
            let fixture: Value = serde_json::from_slice(&std::fs::read(fixture).unwrap()).unwrap();
            let request: PairRequest = serde_json::from_value(fixture["request"].clone()).unwrap();
            assert_eq!(
                pair(fixture["token"].as_str().unwrap(), request.clone())
                    .unwrap()
                    .machine_id,
                request.request_id
            );
            let locked: PairRequest =
                serde_json::from_value(fixture["locked_request"].clone()).unwrap();
            assert_eq!(
                pair(fixture["locked_token"].as_str().unwrap(), locked).unwrap_err(),
                PairError::InvalidInvitation
            );
            DataStore::read_file(|data| {
                assert_eq!(data.node_machines.len(), 1);
                assert_eq!(data.node_invitations.len(), 1);
                assert_eq!(
                    data.node_invitations[0].failed_attempts,
                    MAX_FAILED_ATTEMPTS
                );
                Ok(())
            })
            .unwrap();
            let data_file = home.join("data/profiles.json");
            let old_time = UNIX_EPOCH + std::time::Duration::from_secs(1);
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&data_file)
                .unwrap();
            file.set_times(std::fs::FileTimes::new().set_modified(old_time))
                .unwrap();
            let modified = file.metadata().unwrap().modified().unwrap();
            let unknown = format!(
                "{}.{}.{}",
                fixture["token"]
                    .as_str()
                    .unwrap()
                    .split('.')
                    .next()
                    .unwrap(),
                "f".repeat(32),
                "f".repeat(64)
            );
            let unknown_request = PairRequest {
                request_id: "c".repeat(32),
                credential: "unknown-node-credential-".repeat(3),
            };
            assert_eq!(
                pair(&unknown, unknown_request).unwrap_err(),
                PairError::InvalidInvitation
            );
            let locked: PairRequest =
                serde_json::from_value(fixture["locked_request"].clone()).unwrap();
            assert_eq!(
                pair(fixture["locked_token"].as_str().unwrap(), locked).unwrap_err(),
                PairError::InvalidInvitation
            );
            pair(fixture["token"].as_str().unwrap(), request).unwrap();
            assert_eq!(
                std::fs::metadata(data_file).unwrap().modified().unwrap(),
                modified
            );
        }
        return;
    }
    let isolated = tempfile::tempdir().unwrap();
    for phase in ["write", "read"] {
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "nodes::pairing::tests::persistent_pairing_serializes_duplicates_and_saves_failed_attempts", "--nocapture"])
            .env("GLD_HOME", isolated.path())
            .env(CHILD_PHASE, phase)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "isolated {phase} failed: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(
            isolated.path().join("pairing-test-fixture.json").is_file(),
            "isolated test filter did not run the pairing fixture"
        );
    }
}
