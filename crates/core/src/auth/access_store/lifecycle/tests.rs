use std::fs;
use std::sync::{Arc, Barrier};

use crate::auth::{parse_credential, ServiceCapabilities};

use super::*;

fn spec(name: &str, kind: CredentialKind) -> GrantCreation {
    GrantCreation {
        name: name.into(),
        scope: ProjectScope::AllProjects,
        permissions: GrantPermissions::default(),
        kind,
        ttl: None,
        allowed_origins: Vec::new(),
    }
}

#[test]
fn creation_commits_kind_defaults_and_keeps_only_verifiers() {
    let home = tempfile::tempdir().unwrap();
    let store = AccessStore::open_in(home.path()).unwrap();
    assert!(!store.snapshot().unwrap().is_initialized());
    for (kind, name, lifetime) in [
        (CredentialKind::Token, "Token", Some(30 * 86400)),
        (CredentialKind::OAuth, "OAuth", None),
        (CredentialKind::Url, "URL", Some(8 * 3600)),
    ] {
        let issued = store.create_grant(spec(name, kind), 100).unwrap();
        let credential = &issued.record.credential;
        assert_eq!(credential.kind, kind);
        assert_eq!(credential.revision, 1);
        assert_eq!(credential.valid_for_secs, lifetime);
        assert_eq!(
            credential.expires_at_unix_secs,
            lifetime.map(|ttl| 100 + ttl)
        );
        assert_eq!(
            parse_credential(issued.secret.as_str(), kind).unwrap(),
            credential.id
        );
        assert!(credential.matches_secret(issued.secret.as_str()));
        assert_eq!(issued.record.grant.scope, ProjectScope::AllProjects);
        assert_eq!(issued.record.grant.permissions, GrantPermissions::default());
        let bytes = fs::read_to_string(&store.path).unwrap();
        assert!(!bytes.contains(issued.secret.as_str()));
        assert!(!format!("{issued:?}").contains(issued.secret.as_str()));
        assert!(store.snapshot().unwrap().is_initialized());
    }
    drop(store);
    let reopened = AccessStore::open_in(home.path()).unwrap();
    assert!(reopened.snapshot().unwrap().is_initialized());
    assert_eq!(reopened.snapshot().unwrap().records().len(), 3);
}

#[test]
fn rotation_uses_current_grant_and_keeps_caller_and_original_duration() {
    let home = tempfile::tempdir().unwrap();
    let store = AccessStore::open_in(home.path()).unwrap();
    let mut request = spec("Client", CredentialKind::Token);
    request.ttl = Some(CredentialTtl::Seconds(3600));
    let first = store.create_grant(request, 100).unwrap();
    let id = first.record.credential.id.clone();
    let before = store
        .reader()
        .authenticate_token("hub", &id, first.secret.as_str(), 100)
        .unwrap();
    let mut changed = first.record.clone();
    changed.grant.name = "Renamed".into();
    changed.grant.scope = ProjectScope::Selected(vec!["stable-project".into()]);
    changed.grant.permissions.write = true;
    store.update(changed.clone()).unwrap();
    let rotated = store.rotate_grant("RENAMED", None, 200).unwrap();
    assert_eq!(rotated.record.grant, changed.grant);
    assert_eq!(rotated.record.credential.id, id);
    assert_eq!(rotated.record.credential.revision, 2);
    assert_eq!(rotated.record.credential.valid_for_secs, Some(3600));
    assert_eq!(rotated.record.credential.expires_at_unix_secs, Some(3800));
    assert!(store
        .reader()
        .authenticate_token("hub", &id, first.secret.as_str(), 200)
        .is_err());
    let after = store
        .reader()
        .authenticate_token("hub", &id, rotated.secret.as_str(), 200)
        .unwrap();
    assert_eq!(before.id(), after.id());
    assert!(after.may_write_project());
    assert!(after.allows_project("stable-project"));
    assert!(!after.allows_project("unselected"));
    drop(store);
    let reopened = AccessStore::open_in(home.path()).unwrap();
    assert!(reopened
        .reader()
        .authenticate_token("hub", &id, rotated.secret.as_str(), 201)
        .is_ok());
    assert!(reopened
        .reader()
        .authenticate_token("hub", &id, first.secret.as_str(), 201)
        .is_err());
}

#[test]
fn rotation_applies_each_kind_lifetime_rules_without_changing_origins() {
    let home = tempfile::tempdir().unwrap();
    let store = AccessStore::open_in(home.path()).unwrap();
    let mut token = spec("Token", CredentialKind::Token);
    token.ttl = Some(CredentialTtl::Unlimited);
    store.create_grant(token, 100).unwrap();
    assert_eq!(
        store
            .rotate_grant("Token", None, 200)
            .unwrap()
            .record
            .credential
            .expires_at_unix_secs,
        None
    );
    let limited = store
        .rotate_grant("Token", Some(CredentialTtl::Seconds(42)), 300)
        .unwrap();
    assert_eq!(limited.record.credential.expires_at_unix_secs, Some(342));

    let mut url = spec("URL", CredentialKind::Url);
    url.allowed_origins = vec!["https://client.example".into()];
    url.ttl = Some(CredentialTtl::Seconds(600));
    let first = store.create_grant(url, 100).unwrap();
    let rotated = store.rotate_grant("URL", None, 800).unwrap();
    assert_eq!(
        rotated.record.credential.allowed_origins,
        first.record.credential.allowed_origins
    );
    assert_eq!(rotated.record.credential.expires_at_unix_secs, Some(1400));
    assert!(!rotated
        .record
        .credential
        .matches_secret(first.secret.as_str()));
    let before = store.snapshot().unwrap();
    assert!(store
        .rotate_grant("URL", Some(CredentialTtl::Unlimited), 900)
        .is_err());
    assert!(Arc::ptr_eq(&before, &store.snapshot().unwrap()));

    let first = store
        .create_grant(spec("OAuth", CredentialKind::OAuth), 100)
        .unwrap();
    let before = store.snapshot().unwrap();
    assert!(store
        .rotate_grant("OAuth", Some(CredentialTtl::Unlimited), 200)
        .is_err());
    assert!(Arc::ptr_eq(&before, &store.snapshot().unwrap()));
    let rotated = store.rotate_grant("OAuth", None, 200).unwrap();
    assert_eq!(rotated.record.credential.revision, 2);
    assert_eq!(rotated.record.credential.expires_at_unix_secs, None);
    assert!(!rotated
        .record
        .credential
        .matches_secret(first.secret.as_str()));
    assert!(rotated
        .record
        .credential
        .matches_secret(rotated.secret.as_str()));
    drop(store);
    let reopened = AccessStore::open_in(home.path()).unwrap();
    let current = reopened.snapshot().unwrap();
    let oauth = current.by_name("OAuth").unwrap();
    assert_eq!(oauth.credential.revision, 2);
    assert!(!oauth.credential.matches_secret(first.secret.as_str()));
    assert!(oauth.credential.matches_secret(rotated.secret.as_str()));
    let url = current.by_name("URL").unwrap();
    assert_eq!(url.credential.revision, 2);
    assert_eq!(
        url.credential.allowed_origins,
        vec!["https://client.example"]
    );
    assert_eq!(url.credential.expires_at_unix_secs, Some(1400));
}

#[test]
fn failed_creation_and_rotation_neither_publish_nor_initialize() {
    let home = tempfile::tempdir().unwrap();
    let store = AccessStore::open_in(home.path()).unwrap();
    let before = store.snapshot().unwrap();
    fs::create_dir(&store.path).unwrap();
    assert!(store
        .create_grant(spec("Client", CredentialKind::Token), 100)
        .is_err());
    assert!(Arc::ptr_eq(&before, &store.snapshot().unwrap()));
    assert!(!store.snapshot().unwrap().is_initialized());
    fs::remove_dir(&store.path).unwrap();

    let first = store
        .create_grant(spec("Client", CredentialKind::Token), 100)
        .unwrap();
    let before = store.snapshot().unwrap();
    let original = fs::read(&store.path).unwrap();
    let held = store.path.with_extension("held");
    fs::rename(&store.path, &held).unwrap();
    fs::create_dir(&store.path).unwrap();
    assert!(store.rotate_grant("Client", None, 200).is_err());
    assert!(Arc::ptr_eq(&before, &store.snapshot().unwrap()));
    assert_eq!(fs::read(&held).unwrap(), original);
    assert!(store
        .reader()
        .authenticate_token(
            "hub",
            &first.record.credential.id,
            first.secret.as_str(),
            200
        )
        .is_ok());
    fs::remove_dir(&store.path).unwrap();
    fs::rename(&held, &store.path).unwrap();
    assert_eq!(
        store
            .rotate_grant("Client", None, 200)
            .unwrap()
            .record
            .credential
            .revision,
        2
    );
}

#[test]
fn invalid_creation_revocation_and_overflow_leave_no_new_authority() {
    let home = tempfile::tempdir().unwrap();
    let store = AccessStore::open_in(home.path()).unwrap();
    for kind in [
        CredentialKind::Token,
        CredentialKind::OAuth,
        CredentialKind::Url,
    ] {
        let mut request = spec("Bad", kind);
        request.allowed_origins = vec!["*".into()];
        assert!(store.create_grant(request, 100).is_err());
    }
    let mut request = spec("Bad", CredentialKind::Url);
    request.permissions.service = ServiceCapabilities {
        user_prompts: true,
        machine_mcp: true,
        managed_workspaces: true,
    };
    assert!(store.create_grant(request, 100).is_err());
    assert!(!store.path.exists());
    let first = store
        .create_grant(spec("Client", CredentialKind::Token), 100)
        .unwrap();
    let before = store.snapshot().unwrap();
    assert!(store
        .create_grant(spec("client", CredentialKind::Token), 200)
        .is_err());
    assert!(store.rotate_grant("missing", None, 200).is_err());
    assert!(store
        .rotate_grant("Client", Some(CredentialTtl::Seconds(u64::MAX)), 200)
        .is_err());
    assert!(Arc::ptr_eq(&before, &store.snapshot().unwrap()));
    let mut exhausted = first.record.clone();
    exhausted.credential = CredentialRecord::prepare(
        exhausted.credential.id.clone(),
        CredentialKind::Token,
        first.secret.as_str(),
        u64::MAX,
        100,
        Some(3600),
        vec![],
    )
    .unwrap();
    store.update(exhausted).unwrap();
    let before = store.snapshot().unwrap();
    assert!(store.rotate_grant("Client", None, 200).is_err());
    assert!(Arc::ptr_eq(&before, &store.snapshot().unwrap()));
    store.revoke(&first.record.grant.id).unwrap();
    assert!(store
        .rotate_grant(&first.record.grant.id, None, 200)
        .is_err());
    let replacement = store
        .create_grant(spec("Client", CredentialKind::Token), 200)
        .unwrap();
    assert_ne!(replacement.record.grant.id, first.record.grant.id);
    assert_ne!(replacement.record.credential.id, first.record.credential.id);
    assert!(store
        .reader()
        .authenticate_token(
            "hub",
            &first.record.credential.id,
            first.secret.as_str(),
            200
        )
        .is_err());
}

#[test]
fn simultaneous_rotations_commit_distinct_revisions_and_only_the_last_secret() {
    let home = tempfile::tempdir().unwrap();
    let store = Arc::new(AccessStore::open_in(home.path()).unwrap());
    let first = store
        .create_grant(spec("Client", CredentialKind::Token), 100)
        .unwrap();
    let barrier = Arc::new(Barrier::new(4));
    let threads: Vec<_> = (0..4)
        .map(|_| {
            let store = store.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                store.rotate_grant("Client", None, 200).unwrap()
            })
        })
        .collect();
    let mut results: Vec<_> = threads
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect();
    results.sort_by_key(|issued| issued.record.credential.revision);
    assert_eq!(
        results
            .iter()
            .map(|issued| issued.record.credential.revision)
            .collect::<Vec<_>>(),
        vec![2, 3, 4, 5]
    );
    let current = store.snapshot().unwrap();
    assert_eq!(
        current
            .by_grant_id(&first.record.grant.id)
            .unwrap()
            .credential
            .revision,
        5
    );
    for issued in results {
        let accepted = store
            .reader()
            .authenticate_token(
                "hub",
                &first.record.credential.id,
                issued.secret.as_str(),
                200,
            )
            .is_ok();
        assert_eq!(accepted, issued.record.credential.revision == 5);
        assert_eq!(issued.record.grant, first.record.grant);
    }
}

#[test]
fn simultaneous_same_name_creation_has_one_committed_winner() {
    let home = tempfile::tempdir().unwrap();
    let store = Arc::new(AccessStore::open_in(home.path()).unwrap());
    let barrier = Arc::new(Barrier::new(2));
    let threads: Vec<_> = ["Client", "client"]
        .into_iter()
        .map(|name| {
            let store = store.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                store.create_grant(spec(name, CredentialKind::Token), 100)
            })
        })
        .collect();
    let mut winners: Vec<_> = threads
        .into_iter()
        .filter_map(|thread| thread.join().unwrap().ok())
        .collect();
    assert_eq!(winners.len(), 1);
    let issued = winners.pop().unwrap();
    assert_eq!(store.snapshot().unwrap().records().len(), 1);
    assert!(store
        .reader()
        .authenticate_token(
            "hub",
            &issued.record.credential.id,
            issued.secret.as_str(),
            100
        )
        .is_ok());
    drop(store);
    let reopened = AccessStore::open_in(home.path()).unwrap();
    assert_eq!(reopened.snapshot().unwrap().records().len(), 1);
    assert_eq!(
        reopened
            .snapshot()
            .unwrap()
            .by_name("CLIENT")
            .unwrap()
            .grant
            .id,
        issued.record.grant.id
    );
}

#[test]
fn revocation_racing_rotation_never_revives_the_grant() {
    let home = tempfile::tempdir().unwrap();
    let store = Arc::new(AccessStore::open_in(home.path()).unwrap());
    let first = store
        .create_grant(spec("Client", CredentialKind::Token), 100)
        .unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let rotating = store.clone();
    let start = barrier.clone();
    let rotation = std::thread::spawn(move || {
        start.wait();
        rotating.rotate_grant("Client", None, 200)
    });
    barrier.wait();
    store.revoke(&first.record.grant.id).unwrap();
    if let Ok(issued) = rotation.join().unwrap() {
        assert!(store
            .reader()
            .authenticate_token(
                "hub",
                &issued.record.credential.id,
                issued.secret.as_str(),
                200
            )
            .is_err());
    }
    assert_eq!(
        store
            .snapshot()
            .unwrap()
            .by_grant_id(&first.record.grant.id)
            .unwrap()
            .grant
            .status,
        GrantStatus::Revoked
    );
}
