//! Verified authority for one call. Network callers cannot acquire authority by omitting a grant.

use alloc::string::String;

use serde::{Deserialize, Serialize};

use crate::{
    ActionsSubject, CallerId, CallerIdentity, Grant, GrantStatus, McpSubject, PolicyError,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialKind {
    #[serde(rename = "oauth")]
    OAuth,
    Token,
    Url,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Authority {
    Operator,
    Mcp {
        grant: Grant,
        credential_kind: CredentialKind,
    },
    Actions {
        workspace_id: String,
        write: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthContext {
    caller_id: CallerId,
    authority: Authority,
}

impl AuthContext {
    /// Only trusted local application/IPC entrypoints may construct this authority.
    pub fn local_operator() -> Self {
        Self {
            caller_id: CallerId::local_operator(),
            authority: Authority::Operator,
        }
    }

    /// Call after verifying the credential, its lifetime and current grant/revision.
    pub fn verified_mcp(
        entry: impl Into<String>,
        grant: Grant,
        credential_kind: CredentialKind,
        subject: McpSubject,
    ) -> Result<Self, PolicyError> {
        grant.validate()?;
        if grant.status != GrantStatus::Active {
            return Err(PolicyError("revoked grant cannot authorize a call"));
        }
        let kind_matches = matches!(
            (&subject, credential_kind),
            (McpSubject::OAuth { .. }, CredentialKind::OAuth)
                | (
                    McpSubject::Token | McpSubject::Tunnel { .. },
                    CredentialKind::Token
                )
                | (McpSubject::Url, CredentialKind::Url)
        );
        if !kind_matches {
            return Err(PolicyError(
                "MCP subject does not match the verified credential kind",
            ));
        }
        if credential_kind == CredentialKind::Url && !grant.permissions.service.is_empty() {
            return Err(PolicyError(
                "URL credentials cannot carry service capabilities",
            ));
        }
        let caller_id = CallerId::new(
            entry.into(),
            CallerIdentity::McpClient {
                grant_id: grant.id.clone(),
                subject,
            },
        )?;
        Ok(Self {
            caller_id,
            authority: Authority::Mcp {
                grant,
                credential_kind,
            },
        })
    }

    /// Actions authorization is explicit and confined to its configured workspace.
    pub fn actions(
        entry: impl Into<String>,
        workspace_id: impl Into<String>,
        subject: ActionsSubject,
        write: bool,
    ) -> Result<Self, PolicyError> {
        let workspace_id = workspace_id.into();
        if workspace_id.trim().is_empty() {
            return Err(PolicyError("Actions authority requires a workspace ID"));
        }
        let caller_id = CallerId::new(
            entry.into(),
            CallerIdentity::ActionsClient {
                workspace_id: workspace_id.clone(),
                subject,
            },
        )?;
        Ok(Self {
            caller_id,
            authority: Authority::Actions {
                workspace_id,
                write,
            },
        })
    }

    pub fn id(&self) -> &CallerId {
        &self.caller_id
    }

    pub fn identity(&self) -> &CallerIdentity {
        self.caller_id.identity()
    }

    pub fn grant(&self) -> Option<&Grant> {
        match &self.authority {
            Authority::Mcp { grant, .. } => Some(grant),
            Authority::Operator | Authority::Actions { .. } => None,
        }
    }

    pub fn credential_kind(&self) -> Option<CredentialKind> {
        match &self.authority {
            Authority::Mcp {
                credential_kind, ..
            } => Some(*credential_kind),
            Authority::Operator | Authority::Actions { .. } => None,
        }
    }

    pub fn actions_workspace_id(&self) -> Option<&str> {
        match &self.authority {
            Authority::Actions { workspace_id, .. } => Some(workspace_id),
            Authority::Operator | Authority::Mcp { .. } => None,
        }
    }

    pub fn scope(&self) -> &str {
        self.caller_id.scope()
    }

    pub fn tag(&self) -> String {
        self.caller_id.key().into()
    }

    pub fn is_authenticated(&self) -> bool {
        !matches!(
            self.identity(),
            CallerIdentity::ActionsClient {
                subject: ActionsSubject::Anonymous,
                ..
            }
        )
    }

    pub fn is_local_operator(&self) -> bool {
        matches!(self.authority, Authority::Operator)
    }

    pub fn read_only(&self) -> bool {
        !self.may_write_project()
    }

    pub fn may_write_project(&self) -> bool {
        match &self.authority {
            Authority::Operator => true,
            Authority::Mcp { grant, .. } => grant.permissions.write,
            Authority::Actions { write, .. } => *write,
        }
    }

    /// Membership in the Hub and project/node policy must still be checked by the caller.
    pub fn allows_project(&self, project: &str) -> bool {
        match &self.authority {
            Authority::Operator => !project.trim().is_empty(),
            Authority::Mcp { grant, .. } => grant.allows(project),
            Authority::Actions { workspace_id, .. } => project == workspace_id,
        }
    }

    pub fn may_read_user_prompts(&self) -> bool {
        match &self.authority {
            Authority::Operator => true,
            Authority::Mcp { grant, .. } => grant.permissions.service.user_prompts,
            Authority::Actions { .. } => false,
        }
    }

    pub fn may_use_machine_mcp(&self) -> bool {
        match &self.authority {
            Authority::Operator => true,
            Authority::Mcp { grant, .. } => grant.permissions.service.machine_mcp,
            Authority::Actions { .. } => false,
        }
    }

    pub fn may_call_machine_mcp(&self) -> bool {
        self.may_use_machine_mcp() && self.may_write_project()
    }

    pub fn may_manage_workspaces(&self) -> bool {
        self.may_write_project()
            && match &self.authority {
                Authority::Operator => true,
                Authority::Mcp { grant, .. } => grant.permissions.service.managed_workspaces,
                Authority::Actions { .. } => false,
            }
    }

    pub fn may_delegate(&self) -> bool {
        match &self.authority {
            Authority::Operator => true,
            Authority::Mcp {
                grant,
                credential_kind,
            } => grant.permissions.write && *credential_kind != CredentialKind::Url,
            Authority::Actions { .. } => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{GrantPermissions, ProjectScope, ServiceCapabilities};
    use alloc::vec;

    fn grant(write: bool) -> Grant {
        Grant {
            id: "grant-id".into(),
            name: "name".into(),
            scope: ProjectScope::AllProjects,
            permissions: GrantPermissions {
                write,
                service: ServiceCapabilities::default(),
            },
            status: GrantStatus::Active,
        }
    }

    #[test]
    fn default_all_projects_grants_do_not_inherit_service_or_write_authority() {
        let auth = AuthContext::verified_mcp(
            "hub",
            grant(false),
            CredentialKind::Token,
            McpSubject::Token,
        )
        .unwrap();
        assert!(auth.allows_project("new-project"));
        assert!(auth.is_authenticated());
        assert!(auth.read_only());
        assert!(!auth.may_read_user_prompts());
        assert!(!auth.may_use_machine_mcp());
        assert!(!auth.may_manage_workspaces());
        assert!(!auth.may_delegate());
        assert!(!auth.is_local_operator());
    }

    #[test]
    fn service_capabilities_intersect_write_and_credential_limits() {
        let mut grant = grant(false);
        grant.permissions.service = ServiceCapabilities {
            user_prompts: true,
            machine_mcp: true,
            managed_workspaces: true,
        };
        let read = AuthContext::verified_mcp(
            "hub",
            grant.clone(),
            CredentialKind::Token,
            McpSubject::Token,
        )
        .unwrap();
        assert!(read.may_read_user_prompts());
        assert!(read.may_use_machine_mcp());
        assert!(!read.may_call_machine_mcp());
        assert!(!read.may_manage_workspaces());
        grant.permissions.write = true;
        let write = AuthContext::verified_mcp(
            "hub",
            grant.clone(),
            CredentialKind::Token,
            McpSubject::Token,
        )
        .unwrap();
        assert!(write.may_call_machine_mcp());
        assert!(write.may_manage_workspaces());
        assert!(write.may_delegate());
        assert_eq!(read.id(), write.id());
        assert!(
            AuthContext::verified_mcp("hub", grant, CredentialKind::Url, McpSubject::Url).is_err()
        );
        let url = AuthContext::verified_mcp(
            "hub",
            crate::context::tests::grant(true),
            CredentialKind::Url,
            McpSubject::Url,
        )
        .unwrap();
        assert!(!url.may_delegate());
    }

    #[test]
    fn identity_does_not_change_with_name_or_scope_and_permissions() {
        let mut grant = grant(false);
        let first = AuthContext::verified_mcp(
            "hub",
            grant.clone(),
            CredentialKind::OAuth,
            McpSubject::OAuth {
                client_id: "registered".into(),
            },
        )
        .unwrap();
        grant.name = "renamed".into();
        grant.scope = ProjectScope::Selected(vec!["project".into()]);
        grant.permissions.write = true;
        let second = AuthContext::verified_mcp(
            "hub",
            grant,
            CredentialKind::OAuth,
            McpSubject::OAuth {
                client_id: "registered".into(),
            },
        )
        .unwrap();
        assert_eq!(first.id(), second.id());
        assert_ne!(first, second);
    }

    #[test]
    fn revoked_mismatched_and_empty_identity_cannot_authorize() {
        let mut revoked = grant(false);
        revoked.status = GrantStatus::Revoked;
        assert!(AuthContext::verified_mcp(
            "hub",
            revoked,
            CredentialKind::Token,
            McpSubject::Token
        )
        .is_err());
        assert!(AuthContext::verified_mcp(
            "hub",
            grant(false),
            CredentialKind::Url,
            McpSubject::Token
        )
        .is_err());
        assert!(AuthContext::verified_mcp(
            "hub",
            grant(false),
            CredentialKind::OAuth,
            McpSubject::OAuth {
                client_id: "".into()
            }
        )
        .is_err());
    }

    #[test]
    fn actions_identity_is_workspace_bound_and_permission_independent() {
        for subject in [
            ActionsSubject::Anonymous,
            ActionsSubject::ApiKey,
            ActionsSubject::OAuth {
                client_id: "same-client".into(),
            },
        ] {
            let read =
                AuthContext::actions("actions", "project-a", subject.clone(), false).unwrap();
            let write =
                AuthContext::actions("actions", "project-a", subject.clone(), true).unwrap();
            let other = AuthContext::actions("actions", "project-b", subject, true).unwrap();
            assert_eq!(read.id(), write.id());
            assert_ne!(write.id(), other.id());
            assert_ne!(write.id().key(), other.id().key());
            assert_eq!(write.scope(), other.scope());
            assert!(!other.allows_project("project-a"));
        }
    }

    #[test]
    fn actions_never_get_operator_or_service_authority_from_missing_grant() {
        let auth =
            AuthContext::actions("actions", "project", ActionsSubject::ApiKey, true).unwrap();
        assert!(auth.grant().is_none());
        assert!(auth.is_authenticated());
        assert!(auth.allows_project("project"));
        assert!(!auth.allows_project("other"));
        assert!(!auth.read_only());
        assert!(!auth.is_local_operator());
        assert!(!auth.may_read_user_prompts());
        assert!(!auth.may_use_machine_mcp());
        assert!(!auth.may_manage_workspaces());
        assert!(!auth.may_delegate());
        let anonymous =
            AuthContext::actions("actions", "project", ActionsSubject::Anonymous, false).unwrap();
        assert!(!anonymous.is_authenticated());
        assert_ne!(auth.id(), anonymous.id());
        assert_ne!(auth.id(), AuthContext::local_operator().id());
        assert!(AuthContext::actions("actions", "", ActionsSubject::ApiKey, true).is_err());
    }
}
