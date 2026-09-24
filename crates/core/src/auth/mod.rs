mod bearer;
mod client_registry;
mod context;
mod grant;
mod oauth;
mod oauth_flow;

pub(crate) use bearer::bearer_token;
pub use bearer::verify_bearer_header;
pub use client_registry::{remove_scope as remove_client_registry, ClientRegistry};
pub use context::{AuthContext, Principal};
#[cfg(test)]
pub use grant::FixedGrants;
pub use grant::{Grant, GrantBook, GrantRecord, NoGrants, StoredGrants};
pub use oauth::{
    actions_audience, authorization_server_metadata, external_base_url, hub_audience,
    protected_resource_metadata, protected_resource_metadata_url, workspace_audience,
};
pub use oauth_flow::{
    authorize_get, authorize_post, register_client, token_exchange, verify_oauth_bearer_header,
    AccessIdentity, AuthorizeForm, AuthorizeParams, ClientRegistrationRequest, OAuthRuntime,
    TokenForm,
};
