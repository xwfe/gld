use super::*;
use crate::auth::{AccessRecord, AccessStore, CredentialRecord};

fn add_token(store: &AccessStore, id: &str, scope: ProjectScope, write: bool) {
    store
        .insert(AccessRecord {
            grant: Grant {
                id: id.into(),
                name: id.into(),
                scope,
                permissions: GrantPermissions {
                    write,
                    service: ServiceCapabilities::default(),
                },
                status: GrantStatus::Active,
            },
            credential: CredentialRecord::prepare(
                format!("credential-{id}"),
                CredentialKind::Token,
                "scope-test-secret",
                1,
                100,
                Some(3600),
                Vec::new(),
            )
            .unwrap(),
        })
        .unwrap();
}

fn request_auth(store: &AccessStore, id: &str) -> AuthContext {
    store
        .reader()
        .authenticate_token(
            HUB_SCOPE,
            &format!("credential-{id}"),
            "scope-test-secret",
            200,
        )
        .unwrap()
}

fn count(hub: &Hub, auth: &AuthContext) -> u64 {
    call_as(hub, auth, LIST_WORKSPACES, json!({}))["count"]
        .as_u64()
        .unwrap()
}

fn hidden(hub: &Hub, auth: &AuthContext, profile: &WorkspaceProfile) {
    for selector in [&profile.id, &profile.name] {
        for tool in ["read_file", WORKSPACE_CONTEXT] {
            let args = if tool == "read_file" {
                json!({"workspace":selector, "path":"only-api.txt"})
            } else {
                json!({"workspace":selector})
            };
            let reply = call_as(hub, auth, tool, args);
            assert_eq!(reply["error"]["code"], "WORKSPACE_NOT_IN_HUB", "{reply}");
            let available = reply["error"]["details"]["available"].to_string();
            assert!(!available.contains(&profile.id), "{reply}");
        }
    }
    let initialized = rpc(hub, auth, "initialize", json!({}));
    assert!(!initialized["result"]["instructions"]
        .to_string()
        .contains(&profile.id));
}

#[test]
fn all_tracks_current_members_while_selected_keeps_original_ids() {
    let f = fixture();
    let home = tempfile::tempdir().unwrap();
    let store = AccessStore::open_in(home.path()).unwrap();
    add_token(&store, "all", ProjectScope::AllProjects, false);
    add_token(
        &store,
        "selected",
        ProjectScope::Selected(vec![f.api.id.clone()]),
        false,
    );
    update_fixed(&f.hub, |_, settings| settings.hub.members.clear());
    let original_all = request_auth(&store, "all");
    let original_selected = request_auth(&store, "selected");
    for id in ["all", "selected"] {
        let auth = request_auth(&store, id);
        assert_eq!(count(&f.hub, &auth), 0);
        hidden(&f.hub, &auth, &f.api);
    }

    update_fixed(&f.hub, |_, settings| {
        settings.hub.members.push(f.api.id.clone());
    });
    for id in ["all", "selected"] {
        let auth = request_auth(&store, id);
        assert_eq!(count(&f.hub, &auth), 1);
        let read = call_as(
            &f.hub,
            &auth,
            "read_file",
            json!({"workspace":f.api.id, "path":"only-api.txt"}),
        );
        assert_eq!(read["ok"], true, "{read}");
    }

    update_fixed(&f.hub, |_, settings| {
        settings.hub.members.push(f.web.id.clone());
    });
    assert_eq!(count(&f.hub, &request_auth(&store, "all")), 2);
    assert_eq!(count(&f.hub, &request_auth(&store, "selected")), 1);
    hidden(&f.hub, &request_auth(&store, "selected"), &f.web);
    // 已登记但不是 Hub 成员，也不因 AllProjects 自动进入范围。
    hidden(&f.hub, &request_auth(&store, "all"), &f.outsider);

    update_fixed(&f.hub, |_, settings| {
        settings.hub.members.retain(|id| id != &f.api.id);
    });
    assert_eq!(count(&f.hub, &request_auth(&store, "all")), 1);
    assert_eq!(count(&f.hub, &request_auth(&store, "selected")), 0);
    hidden(&f.hub, &request_auth(&store, "all"), &f.api);
    hidden(&f.hub, &request_auth(&store, "selected"), &f.api);

    // 同一稳定 ID 重新加入仍属于 Selected；改名不改变这个授权。
    update_fixed(&f.hub, |profiles, settings| {
        profiles
            .iter_mut()
            .find(|profile| profile.id == f.api.id)
            .unwrap()
            .name = "renamed-api".into();
        settings.hub.members.push(f.api.id.clone());
    });
    let rejoined = request_auth(&store, "selected");
    assert_eq!(rejoined.id(), original_selected.id());
    assert_eq!(count(&f.hub, &rejoined), 1);
    let read = call_as(
        &f.hub,
        &rejoined,
        "read_file",
        json!({"workspace":"renamed-api", "path":"only-api.txt"}),
    );
    assert_eq!(read["ok"], true, "{read}");
    update_fixed(&f.hub, |_, settings| {
        settings.hub.members.retain(|id| id != &f.api.id);
    });

    let (_replacement_dir, replacement) = workspace("api", &[("only-api.txt", "replacement")]);
    assert_ne!(replacement.id, f.api.id);
    update_fixed(&f.hub, |profiles, settings| {
        profiles.retain(|profile| profile.id != f.api.id);
        profiles.push(replacement.clone());
        settings.hub.members.push(replacement.id.clone());
    });
    let all = request_auth(&store, "all");
    let selected = request_auth(&store, "selected");
    assert_eq!(all.id(), original_all.id());
    assert_eq!(selected.id(), original_selected.id());
    assert_eq!(count(&f.hub, &all), 2);
    assert_eq!(count(&f.hub, &selected), 0);
    hidden(&f.hub, &selected, &replacement);
    let read = call_as(
        &f.hub,
        &all,
        "read_file",
        json!({"workspace":"api", "path":"only-api.txt"}),
    );
    assert_eq!(read["ok"], true, "{read}");
    assert!(read.to_string().contains("replacement"), "{read}");
    let old_id = call_as(
        &f.hub,
        &all,
        "read_file",
        json!({"workspace":f.api.id, "path":"only-api.txt"}),
    );
    assert_eq!(old_id["error"]["code"], "WORKSPACE_NOT_IN_HUB", "{old_id}");
    assert_eq!(
        store
            .snapshot()
            .unwrap()
            .by_grant_id("selected")
            .unwrap()
            .grant
            .scope,
        ProjectScope::Selected(vec![f.api.id]),
    );
}

#[test]
fn permission_changes_apply_to_discovery_context_and_hard_calls_on_the_next_request() {
    let f = fixture();
    let home = tempfile::tempdir().unwrap();
    let store = AccessStore::open_in(home.path()).unwrap();
    add_token(&store, "client", ProjectScope::AllProjects, true);
    let writer = request_auth(&store, "client");
    let written = call_as(
        &f.hub,
        &writer,
        "apply_patch",
        json!({"workspace":"api","patch":"*** Begin Patch\n*** Add File: accepted.txt\n+accepted\n*** End Patch"}),
    );
    assert_eq!(written["ok"], true, "{written}");
    assert!(std::path::Path::new(&f.api.path)
        .join("accepted.txt")
        .exists());

    let mut changed = store
        .snapshot()
        .unwrap()
        .by_grant_id("client")
        .unwrap()
        .clone();
    changed.grant.scope = ProjectScope::Selected(vec![f.api.id.clone()]);
    changed.grant.permissions.write = false;
    store.update(changed).unwrap();
    let reader = request_auth(&store, "client");
    assert_eq!(writer.id(), reader.id());
    assert_eq!(count(&f.hub, &reader), 1);
    hidden(&f.hub, &reader, &f.web);
    assert!(!tool_names(&f.hub, &reader)
        .iter()
        .any(|name| name == "apply_patch"));
    let context = call_as(
        &f.hub,
        &reader,
        WORKSPACE_CONTEXT,
        json!({"workspace":"api"}),
    );
    assert!(
        !context["tools"].to_string().contains("apply_patch"),
        "{context}"
    );
    let denied = rpc(
        &f.hub,
        &reader,
        "tools/call",
        json!({"name":"apply_patch","arguments":{"workspace":"api",
            "patch":"*** Begin Patch\n*** Add File: denied.txt\n+denied\n*** End Patch"}}),
    );
    assert_eq!(
        denied["error"]["data"]["reason"], "unknown_tool",
        "{denied}"
    );
    assert!(!std::path::Path::new(&f.api.path)
        .join("denied.txt")
        .exists());
    let read = call_as(
        &f.hub,
        &reader,
        "read_file",
        json!({"workspace":"api","path":"accepted.txt"}),
    );
    assert_eq!(read["ok"], true, "{read}");
}

#[test]
fn a_new_unconfined_member_does_not_disable_other_all_projects_members() {
    let f = fixture();
    let home = tempfile::tempdir().unwrap();
    let store = AccessStore::open_in(home.path()).unwrap();
    add_token(&store, "all", ProjectScope::AllProjects, false);
    let before = request_auth(&store, "all");
    update_fixed(&f.hub, |profiles, settings| {
        profiles
            .iter_mut()
            .find(|profile| profile.id == f.outsider.id)
            .unwrap()
            .runtime
            .confine_reads = false;
        settings.hub.members.push(f.outsider.id.clone());
    });
    let after = request_auth(&store, "all");
    assert_eq!(before.id(), after.id());
    assert_eq!(count(&f.hub, &after), 3);
    let denied = call_as(
        &f.hub,
        &after,
        "list_dir",
        json!({"workspace":f.outsider.id, "path":"."}),
    );
    assert_eq!(
        denied["error"]["code"], "GRANT_NEEDS_CONFINED_READS",
        "{denied}"
    );
    let allowed = call_as(
        &f.hub,
        &after,
        "read_file",
        json!({"workspace":f.api.id, "path":"only-api.txt"}),
    );
    assert_eq!(allowed["ok"], true, "{allowed}");
}
