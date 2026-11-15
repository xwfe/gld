use alloc::{string::String, vec::Vec};
use core::fmt;

use serde::{Deserialize, Deserializer, Serialize};

/// A deterministic policy error. Authentication and persistence stay outside this crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PolicyError(pub(crate) &'static str);

impl fmt::Display for PolicyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0)
    }
}

impl core::error::Error for PolicyError {}

/// All registered projects, or an explicit nonempty set of stable project IDs.
/// The creation default is not a deserialization default.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(tag = "kind", content = "projects", rename_all = "snake_case")]
pub enum ProjectScope {
    AllProjects,
    Selected(Vec<String>),
}

impl ProjectScope {
    pub fn validate(&self) -> Result<(), PolicyError> {
        if let Self::Selected(projects) = self {
            if projects.is_empty() {
                return Err(PolicyError("selected project scope must not be empty"));
            }
            for (index, project) in projects.iter().enumerate() {
                if project.trim().is_empty() {
                    return Err(PolicyError("project ID must not be empty"));
                }
                if projects[..index].contains(project) {
                    return Err(PolicyError("selected project IDs must be unique"));
                }
            }
        }
        Ok(())
    }

    pub fn allows(&self, project: &str) -> bool {
        !project.trim().is_empty()
            && match self {
                Self::AllProjects => true,
                Self::Selected(projects) => projects.iter().any(|id| id == project),
            }
    }
}

impl<'de> Deserialize<'de> for ProjectScope {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(
            tag = "kind",
            content = "projects",
            rename_all = "snake_case",
            deny_unknown_fields
        )]
        enum Repr {
            AllProjects,
            Selected(Vec<String>),
        }
        let scope = match Repr::deserialize(deserializer)? {
            Repr::AllProjects => Self::AllProjects,
            Repr::Selected(projects) => Self::Selected(projects),
        };
        scope.validate().map_err(serde::de::Error::custom)?;
        Ok(scope)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceCapabilities {
    pub user_prompts: bool,
    pub machine_mcp: bool,
    pub managed_workspaces: bool,
}

impl ServiceCapabilities {
    pub fn is_empty(&self) -> bool {
        !self.user_prompts && !self.machine_mcp && !self.managed_workspaces
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrantPermissions {
    pub write: bool,
    pub service: ServiceCapabilities,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrantStatus {
    Active,
    Revoked,
}

/// Authorization only: no secret, credential revision or transport-specific flags.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "GrantData")]
pub struct Grant {
    pub id: String,
    pub name: String,
    pub scope: ProjectScope,
    pub permissions: GrantPermissions,
    pub status: GrantStatus,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GrantData {
    id: String,
    name: String,
    scope: ProjectScope,
    permissions: GrantPermissions,
    status: GrantStatus,
}

impl TryFrom<GrantData> for Grant {
    type Error = PolicyError;

    fn try_from(data: GrantData) -> Result<Self, Self::Error> {
        let grant = Self {
            id: data.id,
            name: data.name,
            scope: data.scope,
            permissions: data.permissions,
            status: data.status,
        };
        grant.validate()?;
        Ok(grant)
    }
}

impl Grant {
    pub fn validate(&self) -> Result<(), PolicyError> {
        if self.id.trim().is_empty() || self.name.trim().is_empty() {
            return Err(PolicyError("grant ID and name must not be empty"));
        }
        self.scope.validate()?;
        if !matches!(self.scope, ProjectScope::AllProjects) && !self.permissions.service.is_empty()
        {
            return Err(PolicyError(
                "service capabilities require all-project scope",
            ));
        }
        Ok(())
    }

    pub fn allows(&self, project: &str) -> bool {
        self.status == GrantStatus::Active && self.scope.allows(project)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use serde_json::json;

    fn grant() -> Grant {
        Grant {
            id: "grant-id".into(),
            name: "client".into(),
            scope: ProjectScope::AllProjects,
            permissions: GrantPermissions::default(),
            status: GrantStatus::Active,
        }
    }

    #[test]
    fn current_shape_round_trips_without_authority_defaults() {
        let original = grant();
        let value = serde_json::to_value(&original).unwrap();
        assert_eq!(
            serde_json::from_value::<Grant>(value.clone()).unwrap(),
            original
        );
        for missing in ["id", "name", "scope", "permissions", "status"] {
            let mut incomplete = value.clone();
            incomplete.as_object_mut().unwrap().remove(missing);
            assert!(
                serde_json::from_value::<Grant>(incomplete).is_err(),
                "{missing}"
            );
        }
        let mut old = value;
        old["workspaces"] = json!(["project"]);
        assert!(serde_json::from_value::<Grant>(old).is_err());
    }

    #[test]
    fn selected_scope_never_interprets_empty_or_invalid_data_as_all() {
        for value in [
            json!({"kind": "selected", "projects": []}),
            json!({"kind": "selected", "projects": [""]}),
            json!({"kind": "selected", "projects": ["same", "same"]}),
            json!({"kind": "all_projects", "workspaces": ["old"]}),
            json!({}),
            json!([]),
        ] {
            assert!(serde_json::from_value::<ProjectScope>(value).is_err());
        }
        let selected = ProjectScope::Selected(vec!["project".into()]);
        assert!(selected.allows("project"));
        assert!(!selected.allows("later"));
        assert!(ProjectScope::AllProjects.allows("later"));
        assert!(!ProjectScope::AllProjects.allows(""));
    }

    #[test]
    fn grant_scope_service_authority_and_revocation_are_explicit() {
        let mut value = grant();
        assert!(value.allows("future-registration"));
        value.permissions.service.user_prompts = true;
        value.scope = ProjectScope::Selected(vec!["project".into()]);
        assert!(value.validate().is_err());
        assert!(serde_json::from_value::<Grant>(serde_json::to_value(&value).unwrap()).is_err());
        value.permissions.service = ServiceCapabilities::default();
        assert!(value.validate().is_ok());
        value.status = GrantStatus::Revoked;
        assert!(!value.allows("project"));
    }

    #[test]
    fn missing_permissions_do_not_default_to_write_or_service_access() {
        for value in [
            json!({}),
            json!({"write": true}),
            json!({"write": false, "service": {}}),
            json!({"write": false, "service": {"user_prompts": false, "machine_mcp": false}}),
        ] {
            assert!(serde_json::from_value::<GrantPermissions>(value).is_err());
        }
    }
}
