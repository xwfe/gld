//! Stable identity for session and record ownership. Authorization is deliberately absent.

use alloc::string::{String, ToString};
use core::fmt::Write;

use serde::{Deserialize, Serialize};

use crate::PolicyError;

pub const LOCAL_SCOPE: &str = "local";

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum McpSubject {
    #[serde(rename = "oauth")]
    OAuth {
        client_id: String,
    },
    Token,
    Url,
    Tunnel {
        binding_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ActionsSubject {
    Anonymous,
    ApiKey,
    #[serde(rename = "oauth")]
    OAuth {
        client_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CallerIdentity {
    LocalOperator,
    McpClient {
        grant_id: String,
        subject: McpSubject,
    },
    ActionsClient {
        workspace_id: String,
        subject: ActionsSubject,
    },
}

/// Only stable identity participates in Eq/Hash; permission and credential changes do not.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CallerId {
    key: String,
    entry: String,
    identity: CallerIdentity,
}

impl CallerId {
    pub fn local_operator() -> Self {
        Self::new(LOCAL_SCOPE.to_string(), CallerIdentity::LocalOperator)
            .expect("the local operator identity is valid")
    }

    pub(crate) fn new(entry: String, identity: CallerIdentity) -> Result<Self, PolicyError> {
        if entry.trim().is_empty() {
            return Err(PolicyError("caller entry must not be empty"));
        }
        let mut key = String::from("caller-v1:");
        append_part(&mut key, &entry);
        match &identity {
            CallerIdentity::LocalOperator => append_part(&mut key, "local_operator"),
            CallerIdentity::McpClient { grant_id, subject } => {
                if grant_id.trim().is_empty() {
                    return Err(PolicyError("MCP caller requires a grant ID"));
                }
                append_part(&mut key, "mcp");
                append_part(&mut key, grant_id);
                match subject {
                    McpSubject::OAuth { client_id } => {
                        validate_subject_id(client_id)?;
                        append_part(&mut key, "oauth");
                        append_part(&mut key, client_id);
                    }
                    McpSubject::Token => append_part(&mut key, "token"),
                    McpSubject::Url => append_part(&mut key, "url"),
                    McpSubject::Tunnel { binding_id } => {
                        validate_subject_id(binding_id)?;
                        append_part(&mut key, "tunnel");
                        append_part(&mut key, binding_id);
                    }
                }
            }
            CallerIdentity::ActionsClient {
                workspace_id,
                subject,
            } => {
                if workspace_id.trim().is_empty() {
                    return Err(PolicyError("Actions caller requires a workspace ID"));
                }
                append_part(&mut key, "actions");
                append_part(&mut key, workspace_id);
                match subject {
                    ActionsSubject::Anonymous => append_part(&mut key, "anonymous"),
                    ActionsSubject::ApiKey => append_part(&mut key, "api_key"),
                    ActionsSubject::OAuth { client_id } => {
                        validate_subject_id(client_id)?;
                        append_part(&mut key, "oauth");
                        append_part(&mut key, client_id);
                    }
                }
            }
        }
        Ok(Self {
            key,
            entry,
            identity,
        })
    }

    pub fn key(&self) -> &str {
        &self.key
    }

    pub fn scope(&self) -> &str {
        &self.entry
    }

    pub fn identity(&self) -> &CallerIdentity {
        &self.identity
    }

    pub fn grant_id(&self) -> Option<&str> {
        match &self.identity {
            CallerIdentity::McpClient { grant_id, .. } => Some(grant_id),
            CallerIdentity::LocalOperator | CallerIdentity::ActionsClient { .. } => None,
        }
    }
}

fn validate_subject_id(id: &str) -> Result<(), PolicyError> {
    if id.trim().is_empty() {
        Err(PolicyError("client or binding ID must not be empty"))
    } else {
        Ok(())
    }
}

fn append_part(key: &mut String, part: &str) {
    // Byte lengths make delimiters and arbitrary Unicode inside IDs unambiguous.
    write!(key, "{}:", part.len()).expect("formatting a String cannot fail");
    key.push_str(part);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mcp(entry: &str, grant: &str, subject: McpSubject) -> CallerId {
        CallerId::new(
            entry.into(),
            CallerIdentity::McpClient {
                grant_id: grant.into(),
                subject,
            },
        )
        .unwrap()
    }

    #[test]
    fn structured_identity_cannot_collide_at_delimiters() {
        assert_ne!(
            mcp("hub:a", "b", McpSubject::Token),
            mcp("hub", "a:b", McpSubject::Token)
        );
        assert_ne!(
            mcp(
                "hub",
                "grant",
                McpSubject::OAuth {
                    client_id: "x:1:y".into()
                }
            ),
            mcp(
                "hub",
                "grant:x",
                McpSubject::OAuth {
                    client_id: "1:y".into()
                }
            )
        );
        assert_ne!(
            mcp("hub", "授权", McpSubject::Token).key(),
            mcp("hub", "grant", McpSubject::Token).key()
        );
    }

    #[test]
    fn channels_clients_grants_and_entries_are_distinct() {
        let token = mcp("hub", "grant", McpSubject::Token);
        for other in [
            mcp("project", "grant", McpSubject::Token),
            mcp("hub", "rebuilt-grant", McpSubject::Token),
            mcp("hub", "grant", McpSubject::Url),
            mcp(
                "hub",
                "grant",
                McpSubject::Tunnel {
                    binding_id: "binding".into(),
                },
            ),
            mcp(
                "hub",
                "grant",
                McpSubject::OAuth {
                    client_id: "client".into(),
                },
            ),
            CallerId::local_operator(),
            CallerId::new(
                "hub".into(),
                CallerIdentity::ActionsClient {
                    workspace_id: "project".into(),
                    subject: ActionsSubject::ApiKey,
                },
            )
            .unwrap(),
        ] {
            assert_ne!(token.key(), other.key());
        }
        assert_eq!(token, mcp("hub", "grant", McpSubject::Token));
        assert_eq!(token.grant_id(), Some("grant"));
        assert_eq!(token.scope(), "hub");
    }

    #[test]
    fn incomplete_identity_is_refused() {
        assert!(CallerId::new("".into(), CallerIdentity::LocalOperator).is_err());
        assert!(CallerId::new(
            "hub".into(),
            CallerIdentity::McpClient {
                grant_id: "".into(),
                subject: McpSubject::Token
            }
        )
        .is_err());
        assert!(CallerId::new(
            "actions".into(),
            CallerIdentity::ActionsClient {
                workspace_id: "project".into(),
                subject: ActionsSubject::OAuth {
                    client_id: "".into()
                }
            }
        )
        .is_err());
        assert!(CallerId::new(
            "actions".into(),
            CallerIdentity::ActionsClient {
                workspace_id: "".into(),
                subject: ActionsSubject::ApiKey,
            }
        )
        .is_err());
        assert!(serde_json::from_str::<CallerIdentity>(
            r#"{"kind":"actions_client","subject":{"kind":"api_key"}}"#
        )
        .is_err());
    }
}
