use super::*;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::routing::post;
use axum::{Json, Router};
use serde_json::json;

/// 第四段是邀请里的 Hub 公钥，配对回包必须给出同一把。
const HUB_KEY: &str = "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";

fn token() -> String {
    format!(
        "{}.{}.{}.{HUB_KEY}",
        "a".repeat(32),
        "b".repeat(32),
        "c".repeat(64)
    )
}

fn projects(root: &std::path::Path) -> Vec<SharedProject> {
    let path = root.join("project");
    std::fs::create_dir_all(&path).unwrap();
    vec![SharedProject {
        id: "d".repeat(32),
        name: "api".into(),
        path: path.to_string_lossy().into_owned(),
        read_only: false,
    }]
}

fn options(root: &std::path::Path, hub_url: String) -> JoinOptions {
    JoinOptions {
        hub_url,
        config_path: root.join("node.json"),
        workspaces: vec!["api".into()],
        writable_workspaces: Vec::new(),
        ca_certificate: None,
    }
}

async fn server(router: Router) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (format!("http://{address}"), task)
}

#[test]
fn only_explicit_unique_local_projects_with_confined_reads_are_selected() {
    let mut data = AppData::default();
    let first = crate::workspace::WorkspaceProfile::new("api-dir".into(), Some("api".into()));
    let second =
        crate::workspace::WorkspaceProfile::new("private-dir".into(), Some("private".into()));
    data.profiles = vec![first.clone(), second.clone()];
    let selected = resolve_in(&data, &["api".into()]).unwrap();
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].id, first.id);
    assert!(resolve_in(&data, &[]).is_err());
    assert!(resolve_in(&data, &["missing".into()]).is_err());
    assert!(resolve_in(&data, &["api".into(), first.id.clone()]).is_err());
    data.profiles[1].name = "api".into();
    assert!(resolve_in(&data, &["api".into()]).is_err());
    assert_eq!(
        resolve_in(&data, std::slice::from_ref(&first.id)).unwrap()[0].id,
        first.id
    );
    data.profiles[1].name = first.id.clone();
    assert_eq!(
        resolve_in(&data, std::slice::from_ref(&first.id)).unwrap()[0].id,
        first.id,
        "an exact ID must not be shadowed by another project's name"
    );
    data.profiles[0].runtime.confine_reads = false;
    assert!(resolve_in(&data, &[first.id]).is_err());
}

#[test]
fn writable_projects_are_explicit_unique_and_confined_to_the_shared_selection() {
    let root = tempfile::tempdir().unwrap();
    let mut selected = projects(root.path());
    selected.push(SharedProject {
        id: "e".repeat(32),
        name: "web".into(),
        path: "other-test-project".into(),
        read_only: false,
    });
    let ids: Vec<_> = selected.iter().map(|project| project.id.clone()).collect();
    assert_eq!(
        select_writable_workspaces(&mut selected, &["web".into(), "api".into()]).unwrap(),
        ids,
        "write flags are normalized to the complete share order"
    );
    assert!(selected.iter().all(|project| !project.read_only));
    assert_eq!(
        select_writable_workspaces(&mut selected, &["api".into()]).unwrap(),
        vec![ids[0].clone()]
    );
    assert!(!selected[0].read_only);
    assert!(selected[1].read_only);
    for invalid in [
        vec!["private".into()],
        vec!["f".repeat(32)],
        vec!["api".into(), ids[0].clone()],
    ] {
        let before = selected.clone();
        assert!(select_writable_workspaces(&mut selected, &invalid).is_err());
        assert_eq!(selected, before);
    }
    selected[1].name = "api".into();
    assert!(select_writable_workspaces(&mut selected, &["api".into()]).is_err());
    selected[1].name = ids[0].clone();
    assert_eq!(
        select_writable_workspaces(&mut selected, &ids[..1]).unwrap(),
        ids[..1],
        "exact project IDs take precedence over another project's name"
    );
    assert!(selected[1].read_only);
    assert!(select_writable_workspaces(&mut selected, &[])
        .unwrap()
        .is_empty());
    assert!(selected.iter().all(|project| project.read_only));
}

#[test]
fn local_read_only_profiles_cannot_be_enabled_for_file_writes() {
    let mut profile = crate::workspace::WorkspaceProfile::new("api-dir".into(), Some("api".into()));
    profile.runtime.tool_profile = "read-only".into();
    let data = AppData {
        profiles: vec![profile],
        ..AppData::default()
    };
    let mut selected = resolve_in(&data, &["api".into()]).unwrap();
    assert!(select_writable_workspaces(&mut selected, &["api".into()])
        .unwrap_err()
        .contains("tool profile"));
    assert!(select_writable_workspaces(&mut selected, &[])
        .unwrap()
        .is_empty());
    assert!(selected[0].read_only);
}

#[test]
fn pairing_refuses_private_configuration_inside_any_shared_project_before_persistence() {
    let root = tempfile::tempdir().unwrap();
    for writable in [false, true] {
        let selected = projects(root.path());
        let mut settings = options(root.path(), "https://example.invalid/hub".into());
        settings.config_path = PathBuf::from(&selected[0].path).join("node.json");
        if writable {
            settings.writable_workspaces = vec!["api".into()];
        }
        let error = PreparedJoin::prepare_with_projects(settings, token(), selected)
            .err()
            .unwrap();
        assert!(error.contains("outside every shared project"));
        for file in ["node.json", "node.json.pending", "node.json.lock"] {
            assert!(!root.path().join("project").join(file).exists());
        }
    }
}

#[test]
fn pending_pairing_requires_the_original_file_write_permissions() {
    for writable in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let url = "https://example.invalid/hub";
        let mut initial = options(root.path(), url.into());
        if writable {
            initial.writable_workspaces = vec!["api".into()];
        }
        let prepared =
            PreparedJoin::prepare_with_projects(initial, token(), projects(root.path())).unwrap();
        assert_eq!(prepared.preview().workspaces[0].read_only, !writable);
        assert_eq!(
            prepared.preview().access,
            if writable {
                "project-file-write"
            } else {
                "read-only"
            }
        );
        prepared.storage.save_pending(&prepared.config).unwrap();
        let machine_id = prepared.config.machine_id.clone();
        drop(prepared);
        let pending_path = root.path().join("node.json.pending");
        let pending = std::fs::read(&pending_path).unwrap();
        let saved: serde_json::Value = serde_json::from_slice(&pending).unwrap();
        assert_eq!(saved.get("writable_workspaces").is_some(), writable);

        let mut changed = options(root.path(), url.into());
        if !writable {
            changed.writable_workspaces = vec!["api".into()];
        }
        assert!(
            PreparedJoin::prepare_with_projects(changed, token(), projects(root.path())).is_err()
        );
        assert_eq!(std::fs::read(&pending_path).unwrap(), pending);
        assert!(!root.path().join("node.json").exists());

        let mut original = options(root.path(), url.into());
        if writable {
            original.writable_workspaces = vec!["d".repeat(32)];
        }
        let resumed =
            PreparedJoin::prepare_with_projects(original, token(), projects(root.path())).unwrap();
        assert!(resumed.preview().resuming);
        assert_eq!(resumed.config.machine_id, machine_id);
        assert_eq!(resumed.preview().workspaces[0].read_only, !writable);
    }
}

#[test]
fn preparation_does_not_persist_pending_or_overwrite_existing_configuration() {
    let root = tempfile::tempdir().unwrap();
    let prepared = PreparedJoin::prepare_with_projects(
        options(root.path(), "https://example.invalid/hub/".into()),
        token(),
        projects(root.path()),
    )
    .unwrap();
    assert!(!root.path().join("node.json.pending").exists());
    let preview = serde_json::to_string(prepared.preview()).unwrap();
    assert!(!preview.contains(&prepared.config.credential));
    assert!(!preview.contains(&token()));
    assert!(!prepared.preview().desktop);
    prepared.storage.save_pending(&prepared.config).unwrap();
    let request_id = prepared.config.machine_id.clone();
    let credential = prepared.config.credential.clone();
    drop(prepared);
    let resumed = PreparedJoin::prepare_with_projects(
        options(root.path(), "https://example.invalid/hub".into()),
        token(),
        projects(root.path()),
    )
    .unwrap();
    assert!(resumed.preview().resuming);
    assert_eq!(resumed.config.machine_id, request_id);
    assert_eq!(resumed.config.credential, credential);
    drop(resumed);
    let wrong = options(root.path(), "https://other.invalid/hub".into());
    assert!(PreparedJoin::prepare_with_projects(wrong, token(), projects(root.path())).is_err());
    let store = LocalConfig::lock(&root.path().join("node.json")).unwrap();
    let pending = store.load_pending().unwrap().unwrap();
    store.finish(&pending).unwrap();
    drop(store);
    let original = std::fs::read(root.path().join("node.json")).unwrap();
    assert!(PreparedJoin::prepare_with_projects(
        options(root.path(), "https://example.invalid/hub".into()),
        token(),
        projects(root.path())
    )
    .is_err());
    assert_eq!(
        std::fs::read(root.path().join("node.json")).unwrap(),
        original
    );
}

#[tokio::test]
async fn failed_response_keeps_the_same_request_and_credential_for_retry() {
    type Requests = Arc<Mutex<Vec<(String, String)>>>;
    async fn pair(
        State(requests): State<Requests>,
        Json(request): Json<PairRequest>,
    ) -> axum::response::Response {
        let mut saved = requests.lock().unwrap();
        saved.push((request.request_id.clone(), request.credential));
        if saved.len() == 1 {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        Json(json!({"hub_id":"a".repeat(32),"hub_public_key":HUB_KEY,"machine_id":request.request_id,"name":"build-machine","revoked":false})).into_response()
    }
    let requests: Requests = Arc::new(Mutex::new(Vec::new()));
    let (base, task) = server(
        Router::new()
            .route("/hub/nodes/v1/pair", post(pair))
            .with_state(requests.clone()),
    )
    .await;
    let root = tempfile::tempdir().unwrap();
    let url = format!("{base}/hub");
    let first = PreparedJoin::prepare_with_projects(
        options(root.path(), url.clone()),
        token(),
        projects(root.path()),
    )
    .unwrap();
    assert!(first.execute().await.unwrap_err().contains("503"));
    assert!(!root.path().join("node.json").exists());
    let retry = PreparedJoin::prepare_with_projects(
        options(root.path(), url),
        token(),
        projects(root.path()),
    )
    .unwrap();
    let joined = retry.execute().await.unwrap();
    let saved = requests.lock().unwrap();
    assert_eq!(saved.len(), 2);
    assert_eq!(saved[0], saved[1]);
    assert_eq!(saved[0].0, joined.machine_id);
    assert!(!serde_json::to_string(&joined)
        .unwrap()
        .contains(&saved[0].1));
    assert!(root.path().join("node.json").is_file());
    assert!(!root.path().join("node.json.pending").exists());
    task.abort();
}

#[tokio::test]
async fn a_pairing_response_with_another_hub_key_is_not_accepted() {
    async fn pair(Json(request): Json<PairRequest>) -> axum::response::Response {
        // 冒充者能拿到域名证书、也能照抄 hub_id，但拿不出邀请里那把公钥对应的 Hub。
        Json(
            json!({"hub_id":"a".repeat(32),"hub_public_key":"e".repeat(64),
            "machine_id":request.request_id,"name":"build-machine","revoked":false}),
        )
        .into_response()
    }
    let (base, task) = server(Router::new().route("/nodes/v1/pair", post(pair))).await;
    let root = tempfile::tempdir().unwrap();
    let prepared = PreparedJoin::prepare_with_projects(
        options(root.path(), base),
        token(),
        projects(root.path()),
    )
    .unwrap();
    assert_eq!(prepared.config.hub_public_key, HUB_KEY);
    let error = prepared.execute().await.unwrap_err();
    assert!(error.contains("did not match"), "{error}");
    assert!(!root.path().join("node.json").exists());
    assert!(root.path().join("node.json.pending").is_file());
    task.abort();
}

#[tokio::test]
async fn redirects_are_rejected_without_forwarding_credentials() {
    let hits = Arc::new(AtomicUsize::new(0));
    let other_hits = hits.clone();
    let (other, other_task) = server(Router::new().route(
        "/stolen",
        post(move || {
            let hits = other_hits.clone();
            async move {
                hits.fetch_add(1, Ordering::SeqCst);
                StatusCode::OK
            }
        }),
    ))
    .await;
    let (base, task) = server(Router::new().route(
        "/nodes/v1/pair",
        post(move || {
            let location = format!("{other}/stolen");
            async move {
                (
                    StatusCode::TEMPORARY_REDIRECT,
                    [(header::LOCATION, location)],
                    "response-body-secret",
                )
            }
        }),
    ))
    .await;
    let root = tempfile::tempdir().unwrap();
    let prepared = PreparedJoin::prepare_with_projects(
        options(root.path(), base),
        token(),
        projects(root.path()),
    )
    .unwrap();
    let error = prepared.execute().await.unwrap_err();
    assert!(error.contains("307"));
    assert!(!error.contains("response-body-secret"));
    assert_eq!(hits.load(Ordering::SeqCst), 0);
    task.abort();
    other_task.abort();
}

#[tokio::test]
async fn wrong_identity_malformed_and_oversized_responses_keep_pending_state() {
    let cases = [
        json!({"hub_id":"e".repeat(32),"hub_public_key":HUB_KEY,"machine_id":"f".repeat(32),"name":"machine","revoked":false}).to_string(),
        "malformed-response-secret".into(),
        "x".repeat(MAX_RESPONSE_BYTES + 1),
    ];
    for response in cases {
        let (base, task) = server(Router::new().route(
            "/nodes/v1/pair",
            post(move || {
                let response = response.clone();
                async move { response }
            }),
        ))
        .await;
        let root = tempfile::tempdir().unwrap();
        let prepared = PreparedJoin::prepare_with_projects(
            options(root.path(), base),
            token(),
            projects(root.path()),
        )
        .unwrap();
        let credential = prepared.config.credential.clone();
        let error = prepared.execute().await.unwrap_err();
        assert!(!error.contains(&credential));
        assert!(!error.contains("malformed-response-secret"));
        assert!(!root.path().join("node.json").exists());
        assert!(root.path().join("node.json.pending").is_file());
        task.abort();
    }
}
