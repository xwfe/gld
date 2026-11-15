//! OAuth handlers and HTTP credential validation. Persisted credentials stay in core.

mod bearer;
mod oauth;
mod oauth_flow;
#[cfg(test)]
pub(crate) mod test_home;

pub(crate) use bearer::header_secret_matches;
pub use bearer::verify_bearer_header;
pub(crate) use gld_core::auth::bearer_token;
pub use gld_core::auth::{
    AuthContext, ClientRegistry, GrantBook, NoGrants, Principal, StoredGrants,
};
pub use oauth::{
    actions_audience, authorization_server_metadata, external_base_url, hub_audience,
    protected_resource_metadata, protected_resource_metadata_url, workspace_audience,
};
pub use oauth_flow::{
    authorize_get, authorize_post, register_client, token_exchange, verify_oauth_bearer_header,
    AccessIdentity, AuthorizeForm, AuthorizeParams, ClientRegistrationRequest, OAuthRuntime,
    TokenForm,
};
