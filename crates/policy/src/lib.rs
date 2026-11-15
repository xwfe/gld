//! Deterministic identity, authorization and write policy.
//!
//! This crate performs no I/O. Credential verification, persistence and lifetime checks
//! belong to its callers; deserializing policy values never authenticates a request.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

mod caller;
mod context;
mod grant;
pub mod write_class;

pub use caller::{ActionsSubject, CallerId, CallerIdentity, McpSubject, LOCAL_SCOPE};
pub use context::{AuthContext, CredentialKind};
pub use grant::{
    Grant, GrantPermissions, GrantStatus, PolicyError, ProjectScope, ServiceCapabilities,
};
