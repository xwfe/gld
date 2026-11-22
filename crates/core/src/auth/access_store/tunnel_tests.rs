//! 隧道绑定的存储不变量和写入规则；认证判定的测试在 authorize.rs。

use gld_policy::CredentialKind;
use serde_json::{json, Value};

use super::tests::{record, write_private};
use super::*;

const TUNNEL: &str = "openai:tunnel_0123456789abcdef0123456789abcdef";
const OTHER_TUNNEL: &str = "openai:tunnel_fedcba9876543210fedcba9876543210";

fn kind_record(id: &str, kind: CredentialKind) -> AccessRecord {
    let mut value = record(id, id, "kind-secret");
    let ttl = match kind {
        CredentialKind::OAuth => None,
        _ => Some(3600),
    };
    value.credential = CredentialRecord::prepare(
        format!("credential-{id}"),
        kind,
        "kind-secret",
        1,
        100,
        ttl,
        Vec::new(),
    )
    .unwrap();
    value
}

fn rotate(store: &AccessStore, id: &str, revision: u64) {
    let mut next = store.snapshot().unwrap().by_grant_id(id).unwrap().clone();
    next.credential = CredentialRecord::prepare(
        format!("credential-{id}"),
        CredentialKind::Token,
        &format!("secret-{revision}"),
        revision,
        200,
        Some(3600),
        Vec::new(),
    )
    .unwrap();
    store.update(next).unwrap();
}

fn file(store: &AccessStore) -> Value {
    serde_json::from_slice(&fs::read(&store.path).unwrap()).unwrap()
}

#[test]
fn bindings_round_trip_and_store_only_references() {
    let home = tempfile::tempdir().unwrap();
    let store = AccessStore::open_in(home.path()).unwrap();
    store
        .insert(record("one", "Client", "token-secret"))
        .unwrap();
    // 没有绑定时也写出空数组：字段是必填的。
    assert_eq!(file(&store)["tunnel_bindings"], json!([]));

    let binding = store.bind_tunnel(TUNNEL, "one", 150).unwrap();
    assert_eq!(binding.tunnel, TUNNEL);
    assert_eq!(binding.grant_id, "one");
    assert_eq!(binding.credential_revision, 1);
    assert_eq!(binding.bound_at_unix_secs, 150);
    assert_eq!(binding.id.len(), 32);

    let saved = file(&store);
    assert_eq!(
        saved["tunnel_bindings"],
        json!([{
            "id": binding.id,
            "tunnel": TUNNEL,
            "grant_id": "one",
            "credential_revision": 1,
            "bound_at_unix_secs": 150,
        }])
    );
    assert_eq!(saved["schema_version"], json!(1));
    assert!(!saved.to_string().contains("token-secret"));

    let path = store.path.clone();
    drop(store);
    let reopened = AccessStore::open_in(home.path()).unwrap();
    let current = reopened.snapshot().unwrap();
    assert_eq!(current.by_tunnel(TUNNEL), Some(&binding));
    assert_eq!(current.tunnel_bindings(), std::slice::from_ref(&binding));
    assert!(current.by_tunnel(OTHER_TUNNEL).is_none());
    check_private_file(&File::open(path).unwrap()).unwrap();
}

#[test]
fn files_with_missing_or_inconsistent_bindings_are_rejected_untouched() {
    let valid = || {
        json!({
            "schema_version": 1,
            "records": [
                serde_json::to_value(record("one", "Client", "test-secret")).unwrap(),
                serde_json::to_value(kind_record("oauth", CredentialKind::OAuth)).unwrap(),
                serde_json::to_value(kind_record("url", CredentialKind::Url)).unwrap(),
            ],
            "tunnel_bindings": [{
                "id": "0123456789abcdef0123456789abcdef",
                "tunnel": TUNNEL,
                "grant_id": "one",
                "credential_revision": 1,
                "bound_at_unix_secs": 150,
            }],
        })
    };
    // 基准文件本身能加载，否则下面的拒绝说明不了问题。
    let home = tempfile::tempdir().unwrap();
    write_private(
        &home.path().join("access/v1/grants.json"),
        valid().to_string().as_bytes(),
    );
    assert!(AccessStore::open_in(home.path())
        .unwrap()
        .snapshot()
        .unwrap()
        .by_tunnel(TUNNEL)
        .is_some());

    let edit = |change: &dyn Fn(&mut Value)| {
        let mut value = valid();
        change(&mut value);
        value.to_string()
    };
    let second = |change: &dyn Fn(&mut Value)| {
        edit(&|value| {
            let mut extra = value["tunnel_bindings"][0].clone();
            extra["id"] = json!("fedcba9876543210fedcba9876543210");
            extra["tunnel"] = json!(OTHER_TUNNEL);
            change(&mut extra);
            value["tunnel_bindings"].as_array_mut().unwrap().push(extra);
        })
    };
    let invalid = [
        (
            "缺字段",
            edit(&|v| {
                v.as_object_mut().unwrap().remove("tunnel_bindings");
            }),
        ),
        ("null", edit(&|v| v["tunnel_bindings"] = Value::Null)),
        (
            "绑定里的未知字段",
            edit(&|v| v["tunnel_bindings"][0]["tunnel_key"] = json!("x")),
        ),
        (
            "缺修订号",
            edit(&|v| {
                v["tunnel_bindings"][0]
                    .as_object_mut()
                    .unwrap()
                    .remove("credential_revision");
            }),
        ),
        (
            "修订号 0",
            edit(&|v| v["tunnel_bindings"][0]["credential_revision"] = json!(0)),
        ),
        (
            "修订号超前",
            edit(&|v| v["tunnel_bindings"][0]["credential_revision"] = json!(2)),
        ),
        (
            "悬空 grant",
            edit(&|v| v["tunnel_bindings"][0]["grant_id"] = json!("missing")),
        ),
        (
            "OAuth grant",
            edit(&|v| v["tunnel_bindings"][0]["grant_id"] = json!("oauth")),
        ),
        (
            "URL grant",
            edit(&|v| v["tunnel_bindings"][0]["grant_id"] = json!("url")),
        ),
        (
            "绑定 ID 格式",
            edit(&|v| v["tunnel_bindings"][0]["id"] = json!("short")),
        ),
        (
            "绑定 ID 大写",
            edit(&|v| v["tunnel_bindings"][0]["id"] = json!("0123456789ABCDEF0123456789ABCDEF")),
        ),
        (
            "隧道没有前缀",
            edit(&|v| {
                v["tunnel_bindings"][0]["tunnel"] = json!("tunnel_0123456789abcdef0123456789abcdef")
            }),
        ),
        (
            "隧道 ID 格式",
            edit(&|v| v["tunnel_bindings"][0]["tunnel"] = json!("openai:sk-proj-abc")),
        ),
        (
            "别的隧道类型",
            edit(&|v| v["tunnel_bindings"][0]["tunnel"] = json!("cf:tunnel_abc")),
        ),
        (
            "同一隧道两个绑定",
            second(&|extra| extra["tunnel"] = json!(TUNNEL)),
        ),
        (
            "绑定 ID 重复",
            second(&|extra| extra["id"] = json!("0123456789abcdef0123456789abcdef")),
        ),
    ];
    for (what, bytes) in invalid {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("access/v1/grants.json");
        write_private(&path, bytes.as_bytes());
        assert!(AccessStore::open_in(home.path()).is_err(), "{what}");
        assert_eq!(fs::read(&path).unwrap(), bytes.as_bytes(), "{what}");
    }
    // 一个 grant 可以被两条隧道绑定。
    let home = tempfile::tempdir().unwrap();
    write_private(
        &home.path().join("access/v1/grants.json"),
        second(&|_| {}).as_bytes(),
    );
    assert_eq!(
        AccessStore::open_in(home.path())
            .unwrap()
            .snapshot()
            .unwrap()
            .tunnel_bindings()
            .len(),
        2
    );
}

#[test]
fn only_active_token_grants_can_be_bound_and_bad_tunnel_names_are_refused() {
    let home = tempfile::tempdir().unwrap();
    let store = AccessStore::open_in(home.path()).unwrap();
    store
        .insert(record("one", "Client", "token-secret"))
        .unwrap();
    store
        .insert(kind_record("oauth", CredentialKind::OAuth))
        .unwrap();
    store
        .insert(kind_record("url", CredentialKind::Url))
        .unwrap();
    store.insert(record("gone", "Gone", "gone-secret")).unwrap();
    store.revoke("gone").unwrap();
    let before = store.snapshot().unwrap();
    let bytes = fs::read(&store.path).unwrap();

    for selector in ["oauth", "url", "gone", "Gone", "missing", ""] {
        assert!(
            store.bind_tunnel(TUNNEL, selector, 150).is_err(),
            "{selector}"
        );
    }
    for tunnel in [
        "",
        "tunnel_0123456789abcdef0123456789abcdef",
        "openai:",
        "openai:sk-proj-abc",
        "openai:tunnel_ABC",
        "cf:tunnel_abc",
    ] {
        assert!(store.bind_tunnel(tunnel, "one", 150).is_err(), "{tunnel}");
    }
    // 拒绝的绑定不发布新快照，也不改文件。
    assert!(Arc::ptr_eq(&before, &store.snapshot().unwrap()));
    assert_eq!(fs::read(&store.path).unwrap(), bytes);
    assert_eq!(openai_tunnel_name("sk-proj-abc"), None);
    assert_eq!(
        openai_tunnel_name("tunnel_0123456789abcdef0123456789abcdef").as_deref(),
        Some(TUNNEL)
    );

    // 按名字（不分大小写）和按 ID 选到的是同一个 grant。
    let by_name = store.bind_tunnel(TUNNEL, " client ", 150).unwrap();
    assert_eq!(by_name.grant_id, "one");
}

#[test]
fn rebinding_keeps_the_id_for_the_same_grant_and_changes_it_for_another() {
    let home = tempfile::tempdir().unwrap();
    let store = AccessStore::open_in(home.path()).unwrap();
    store
        .insert(record("one", "Client", "token-secret"))
        .unwrap();
    store
        .insert(record("two", "Other", "other-secret"))
        .unwrap();
    let first = store.bind_tunnel(TUNNEL, "one", 150).unwrap();

    // 轮换不改绑定记录：旧修订号留在文件里，由认证时的比较拒绝。
    rotate(&store, "one", 2);
    assert_eq!(store.snapshot().unwrap().by_tunnel(TUNNEL), Some(&first));

    let rebound = store.bind_tunnel(TUNNEL, "one", 250).unwrap();
    assert_eq!(rebound.id, first.id);
    assert_eq!(rebound.credential_revision, 2);
    assert_eq!(rebound.bound_at_unix_secs, 250);

    let moved = store.bind_tunnel(TUNNEL, "two", 300).unwrap();
    assert_ne!(moved.id, first.id);
    assert_eq!(moved.grant_id, "two");
    // 一条隧道始终只有一个绑定。
    assert_eq!(store.snapshot().unwrap().tunnel_bindings().len(), 1);

    // 同一个 grant 绑到第二条隧道是另一个绑定、另一个 ID。
    let second = store.bind_tunnel(OTHER_TUNNEL, "two", 300).unwrap();
    assert_ne!(second.id, moved.id);
    assert_eq!(store.snapshot().unwrap().tunnel_bindings().len(), 2);

    assert!(store.unbind_tunnel(TUNNEL).unwrap());
    assert!(!store.unbind_tunnel(TUNNEL).unwrap());
    let current = store.snapshot().unwrap();
    assert!(current.by_tunnel(TUNNEL).is_none());
    assert_eq!(current.by_tunnel(OTHER_TUNNEL), Some(&second));
    // 解绑后再绑同一个 grant 是新的绑定，不沿用旧身份。
    assert_ne!(store.bind_tunnel(TUNNEL, "two", 400).unwrap().id, moved.id);
}

#[test]
fn revoking_keeps_the_binding_and_the_file_still_loads() {
    let home = tempfile::tempdir().unwrap();
    let store = AccessStore::open_in(home.path()).unwrap();
    store
        .insert(record("one", "Client", "token-secret"))
        .unwrap();
    store
        .insert(record("two", "Other", "other-secret"))
        .unwrap();
    let binding = store.bind_tunnel(TUNNEL, "one", 150).unwrap();
    store.revoke("one").unwrap();
    assert_eq!(store.snapshot().unwrap().by_tunnel(TUNNEL), Some(&binding));
    // 撤销后的 grant 不能重新绑定；这条隧道可以改绑到别的有效 grant。
    assert!(store.bind_tunnel(TUNNEL, "one", 200).is_err());
    drop(store);

    let reopened = AccessStore::open_in(home.path()).unwrap();
    assert_eq!(
        reopened.snapshot().unwrap().by_tunnel(TUNNEL),
        Some(&binding)
    );
    // 文件仍可写：撤销留下的绑定不挡住后续管理操作。
    reopened
        .insert(record("three", "Third", "third-secret"))
        .unwrap();
    let moved = reopened.bind_tunnel(TUNNEL, "two", 300).unwrap();
    assert_ne!(moved.id, binding.id);
}

#[test]
fn a_write_that_would_leave_a_dangling_binding_is_not_committed() {
    let home = tempfile::tempdir().unwrap();
    let store = AccessStore::open_in(home.path()).unwrap();
    store
        .insert(record("one", "Client", "token-secret"))
        .unwrap();
    store
        .insert(record("two", "Other", "other-secret"))
        .unwrap();
    store.bind_tunnel(TUNNEL, "one", 150).unwrap();
    let before = store.snapshot().unwrap();
    let bytes = fs::read(&store.path).unwrap();

    // 现在没有删除记录的公开写入；直接走内部提交口证明：只删记录、留下绑定的状态提交不了。
    assert!(store
        .change_all(|records, _| {
            records.retain(|record| record.grant.id != "one");
            Ok(())
        })
        .is_err());
    assert!(Arc::ptr_eq(&before, &store.snapshot().unwrap()));
    assert_eq!(fs::read(&store.path).unwrap(), bytes);

    // 同一次提交里一起删掉绑定就可以。
    store
        .change_all(|records, bindings| {
            records.retain(|record| record.grant.id != "one");
            bindings.retain(|binding| binding.grant_id != "one");
            Ok(())
        })
        .unwrap();
    drop(before);
    drop(store);
    let reopened = AccessStore::open_in(home.path()).unwrap();
    let current = reopened.snapshot().unwrap();
    assert!(current.by_grant_id("one").is_none());
    assert!(current.tunnel_bindings().is_empty());
}

#[test]
fn a_failed_save_does_not_publish_the_binding() {
    let home = tempfile::tempdir().unwrap();
    let store = AccessStore::open_in(home.path()).unwrap();
    store
        .insert(record("one", "Client", "token-secret"))
        .unwrap();
    let before = store.snapshot().unwrap();
    let held = store.path.with_extension("held");
    fs::rename(&store.path, &held).unwrap();
    fs::create_dir(&store.path).unwrap();
    assert!(store.bind_tunnel(TUNNEL, "one", 150).is_err());
    assert!(Arc::ptr_eq(&before, &store.snapshot().unwrap()));
    fs::remove_dir(&store.path).unwrap();
    fs::rename(&held, &store.path).unwrap();
    store.bind_tunnel(TUNNEL, "one", 150).unwrap();
    assert!(store.snapshot().unwrap().by_tunnel(TUNNEL).is_some());
}
