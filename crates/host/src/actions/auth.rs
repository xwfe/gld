use std::sync::Arc;

use axum::{
    extract::Request,
    http::{header::AUTHORIZATION, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Extension,
};

use crate::auth::{
    external_base_url, verify_oauth_bearer_header, ActionsSubject, AuthContext, OAuthRuntime,
};

use super::bearer::constant_time_eq;

#[derive(Clone)]
pub struct AuthConfig {
    pub auth_type: String,
    pub workspace_id: String,
    pub write: bool,
    pub api_key: Option<String>,
    pub oauth: Option<Arc<OAuthRuntime>>,
    pub bind_port: u16,
    pub configured_public_url: String,
}

impl AuthConfig {
    pub fn new(
        auth_type: String,
        api_key: Option<String>,
        oauth: Option<Arc<OAuthRuntime>>,
        bind_port: u16,
        configured_public_url: String,
        workspace_id: String,
        write: bool,
    ) -> Self {
        Self {
            auth_type,
            workspace_id,
            write,
            api_key,
            oauth,
            bind_port,
            configured_public_url,
        }
    }

    pub fn no_auth(&self) -> bool {
        self.auth_type == "none"
    }

    pub fn api_key_enabled(&self) -> bool {
        self.auth_type == "api_key"
    }

    pub fn oauth_enabled(&self) -> bool {
        self.auth_type == "oauth"
    }
}

/// Actions identities are confined to this protocol and their configured workspace.
pub const ACTIONS_SCOPE: &str = "actions";

pub async fn require_actions_auth(
    Extension(auth): Extension<Arc<AuthConfig>>,
    mut request: Request,
    next: Next,
) -> Response {
    if auth.no_auth() {
        let context = match actions_context(&auth, ActionsSubject::Anonymous) {
            Ok(context) => context,
            Err(response) => return *response,
        };
        request.extensions_mut().insert(context);
        return next.run(request).await;
    }

    if auth.api_key_enabled() {
        let Some(expected) = auth.api_key.as_ref() else {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Actions API key is not configured",
            )
                .into_response();
        };

        let Some(header_value) = request.headers().get(AUTHORIZATION) else {
            return (StatusCode::UNAUTHORIZED, "Missing Authorization header").into_response();
        };

        let Ok(header_str) = header_value.to_str() else {
            return (StatusCode::UNAUTHORIZED, "Invalid Authorization header").into_response();
        };

        let Some(token) = header_str.strip_prefix("Bearer ").map(str::trim) else {
            return (StatusCode::UNAUTHORIZED, "Invalid API key").into_response();
        };

        if !constant_time_eq(token.as_bytes(), expected.as_bytes()) {
            return (StatusCode::UNAUTHORIZED, "Invalid API key").into_response();
        }

        let context = match actions_context(&auth, ActionsSubject::ApiKey) {
            Ok(context) => context,
            Err(response) => return *response,
        };
        request.extensions_mut().insert(context);
        return next.run(request).await;
    }

    if auth.oauth_enabled() {
        let Some(oauth) = auth.oauth.as_ref() else {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Actions OAuth is not configured",
            )
                .into_response();
        };
        let identity = match verify_oauth_bearer_header(
            request.headers(),
            oauth,
            &external_base_url(
                request.headers(),
                auth.bind_port,
                &auth.configured_public_url,
            ),
        ) {
            Ok(identity) => identity,
            Err(response) => return *response,
        };
        let context = match actions_context(
            &auth,
            ActionsSubject::OAuth {
                client_id: identity.client_id,
            },
        ) {
            Ok(context) => context,
            Err(response) => return *response,
        };
        request.extensions_mut().insert(context);
        return next.run(request).await;
    }

    (
        StatusCode::INTERNAL_SERVER_ERROR,
        format!("Unsupported Actions auth type: {}", auth.auth_type),
    )
        .into_response()
}

fn actions_context(
    auth: &AuthConfig,
    subject: ActionsSubject,
) -> Result<AuthContext, Box<Response>> {
    AuthContext::actions(ACTIONS_SCOPE, &auth.workspace_id, subject, auth.write).map_err(|_| {
        Box::new(
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Invalid Actions authority configuration",
            )
                .into_response(),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actions_authority_is_workspace_bound_and_explicit() {
        let auth = AuthConfig::new(
            "api_key".into(),
            Some("fixture".into()),
            None,
            0,
            String::new(),
            "project".into(),
            false,
        );
        let context = actions_context(&auth, ActionsSubject::ApiKey).unwrap();
        assert_eq!(context.actions_workspace_id(), Some("project"));
        assert!(context.allows_project("project"));
        assert!(!context.allows_project("different"));
        assert!(context.read_only());
        assert!(context.grant().is_none());
        assert!(!context.is_local_operator());
        assert!(!context.may_read_user_prompts());
        assert!(!context.may_use_machine_mcp());
        assert!(!context.may_manage_workspaces());
    }

    #[test]
    fn anonymous_actions_are_explicit_and_missing_configuration_is_rejected() {
        let mut auth = AuthConfig::new(
            "none".into(),
            None,
            None,
            0,
            String::new(),
            "project".into(),
            true,
        );
        let context = actions_context(&auth, ActionsSubject::Anonymous).unwrap();
        assert!(!context.is_authenticated());
        assert!(!context.is_local_operator());
        assert!(!context.read_only());
        auth.workspace_id.clear();
        assert!(actions_context(&auth, ActionsSubject::Anonymous).is_err());
    }
}
