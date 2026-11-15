//! OAuth handlers and HTTP credential validation. Persisted credentials stay in core.

mod bearer;
mod oauth;
mod oauth_flow;
#[cfg(test)]
pub(crate) mod test_home;

pub use bearer::verify_bearer_header;
pub use gld_core::auth::{ActionsSubject, AuthContext, ClientRegistry};
pub use oauth::{
    actions_audience, authorization_server_metadata, external_base_url, hub_audience,
    protected_resource_metadata, protected_resource_metadata_url, workspace_audience,
};
pub use oauth_flow::{
    authorize_get, authorize_post, register_client, token_exchange, verify_oauth_bearer_header,
    AccessIdentity, AuthorizeForm, AuthorizeParams, ClientRegistrationRequest, OAuthRuntime,
    TokenForm,
};
