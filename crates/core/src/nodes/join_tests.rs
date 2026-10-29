use super::*;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::routing::post;
use axum::{Json, Router};
use serde_json::json;

fn token() -> String {
    format!("{}.{}.{}", "a".repeat(32), "b".repeat(32), "c".repeat(64))
}

fn projects() -> Vec<SharedProject> {
    vec![SharedProject {
        id: "d".repeat(32),
        name: "api".into(),
        path: "test-project".into(),
    }]
}

fn options(root: &std::path::Path, hub_url: String) -> JoinOptions {
    JoinOptions {
        hub_url,
        config_path: root.join("node.json"),
        workspaces: vec!["api".into()],
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
fn preparation_does_not_persist_pending_or_overwrite_existing_configuration() {
    let root = tempfile::tempdir().unwrap();
    let prepared = PreparedJoin::prepare_with_projects(
        options(root.path(), "https://example.invalid/hub/".into()),
        token(),
        projects(),
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
        projects(),
    )
    .unwrap();
    assert!(resumed.preview().resuming);
    assert_eq!(resumed.config.machine_id, request_id);
    assert_eq!(resumed.config.credential, credential);
    drop(resumed);
    let wrong = options(root.path(), "https://other.invalid/hub".into());
    assert!(PreparedJoin::prepare_with_projects(wrong, token(), projects()).is_err());
    let store = LocalConfig::lock(&root.path().join("node.json")).unwrap();
    let pending = store.load_pending().unwrap().unwrap();
    store.finish(&pending).unwrap();
    drop(store);
    let original = std::fs::read(root.path().join("node.json")).unwrap();
    assert!(PreparedJoin::prepare_with_projects(
        options(root.path(), "https://example.invalid/hub".into()),
        token(),
        projects()
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
        Json(json!({"hub_id":"a".repeat(32),"machine_id":request.request_id,"name":"build-machine","revoked":false})).into_response()
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
    let first =
        PreparedJoin::prepare_with_projects(options(root.path(), url.clone()), token(), projects())
            .unwrap();
    assert!(first.execute().await.unwrap_err().contains("503"));
    assert!(!root.path().join("node.json").exists());
    let retry = PreparedJoin::prepare_with_projects(options(root.path(), url), token(), projects())
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
    let prepared =
        PreparedJoin::prepare_with_projects(options(root.path(), base), token(), projects())
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
        json!({"hub_id":"e".repeat(32),"machine_id":"f".repeat(32),"name":"machine","revoked":false}).to_string(),
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
        let prepared =
            PreparedJoin::prepare_with_projects(options(root.path(), base), token(), projects())
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
