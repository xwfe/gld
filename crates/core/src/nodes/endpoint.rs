//! 两个已授权入口共用节点 Upgrade，不能把节点凭据交给 MCP 的认证分支。
use std::sync::Arc;

use axum::extract::ws::WebSocketUpgrade;
use axum::http::{header::AUTHORIZATION, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

use super::hub_key::{Binding, MessageSigner, NONCE_HEADER, PROOF_HEADER};
use super::registry::Nodes;

/// 邀请只用于这条独立的登记入口。正文有界，解析失败不回显秘密字段。
pub async fn pair(request: axum::extract::Request) -> Response {
    if request.headers().get_all(AUTHORIZATION).iter().count() != 1 {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let Some(invitation) = request
        .headers()
        .get(AUTHORIZATION)
        .and_then(|header| header.to_str().ok())
        .and_then(crate::auth::bearer_token)
        .map(str::to_owned)
    else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let body = match tokio::time::timeout(
        std::time::Duration::from_secs(5),
        axum::body::to_bytes(request.into_body(), 4096),
    )
    .await
    {
        Ok(Ok(body)) => body,
        Ok(Err(_)) => return StatusCode::BAD_REQUEST.into_response(),
        Err(_) => return StatusCode::REQUEST_TIMEOUT.into_response(),
    };
    let request = match serde_json::from_slice::<super::pairing::PairRequest>(&body) {
        Ok(request) => request,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    let result =
        tokio::task::spawn_blocking(move || super::pairing::pair(&invitation, request)).await;
    match result {
        Ok(Ok(registered)) => (
            [(axum::http::header::CACHE_CONTROL, "no-store")],
            Json(registered),
        )
            .into_response(),
        Ok(Err(error)) => {
            use super::pairing::PairError;
            let status = match error {
                PairError::InvalidRequest => StatusCode::BAD_REQUEST,
                PairError::InvalidInvitation => StatusCode::UNAUTHORIZED,
                PairError::Conflict => StatusCode::CONFLICT,
                PairError::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
            };
            (status, Json(json!({"error": error.to_string()}))).into_response()
        }
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

pub async fn upgrade(
    nodes: Arc<Nodes>,
    machine_id: String,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    if headers.get_all(AUTHORIZATION).iter().count() != 1 {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let Some(credential) = headers
        .get(AUTHORIZATION)
        .and_then(|header| header.to_str().ok())
        .and_then(crate::auth::bearer_token)
        .map(str::to_owned)
    else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let mut nonces = headers.get_all(NONCE_HEADER).iter();
    let nonce = match (nonces.next(), nonces.next()) {
        (Some(value), None) => value.to_str().ok().map(str::to_owned),
        _ => None,
    };
    let admitted = tokio::task::spawn_blocking(move || {
        let authenticated = super::config::authenticate(&machine_id, &credential)
            .map_err(|_| "INVALID_NODE_CREDENTIALS")?;
        // 先认凭据再看 nonce：没凭据的请求一律 401，不透露节点协议细节。
        let binding = nonce
            .and_then(|nonce| Binding::new(&authenticated.hub_id, &machine_id, &nonce).ok())
            .ok_or("NODE_NONCE_REQUIRED")?;
        let key = super::config::signing_key().map_err(|_| "NODE_HUB_KEY_UNAVAILABLE")?;
        let hub_id = authenticated.hub_id.clone();
        let reservation = nodes.reserve(authenticated)?;
        let proof = key.handshake_proof(&binding);
        Ok((
            hub_id,
            proof,
            MessageSigner::new(Arc::new(key), binding),
            reservation,
        ))
    })
    .await;
    let (hub_id, proof, signer, reservation) = match admitted {
        Ok(Ok(admitted)) => admitted,
        Ok(Err("INVALID_NODE_CREDENTIALS")) => return StatusCode::UNAUTHORIZED.into_response(),
        Ok(Err("HUB_STOPPING")) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        Ok(Err(code @ "NODE_NONCE_REQUIRED")) => {
            return (StatusCode::BAD_REQUEST, Json(json!({"error":code}))).into_response()
        }
        Ok(Err(code @ "NODE_HUB_KEY_UNAVAILABLE")) => {
            return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({"error":code}))).into_response()
        }
        Ok(Err(code)) => {
            return (StatusCode::CONFLICT, Json(json!({"error":code}))).into_response()
        }
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let mut response = ws
        .max_message_size(super::transport::MAX_MESSAGE_BYTES)
        .max_frame_size(super::transport::MAX_MESSAGE_BYTES)
        .on_upgrade(move |socket| reservation.accept(socket, signer));
    response.headers_mut().insert(
        "x-gld-hub-id",
        hub_id.parse().expect("validated hub identity"),
    );
    response
        .headers_mut()
        .insert(PROOF_HEADER, proof.parse().expect("hexadecimal signature"));
    response
}
