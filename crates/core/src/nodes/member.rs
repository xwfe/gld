use serde_json::{json, Value};

#[derive(Clone)]
pub struct NativeMember {
    pub id: String,
    pub name: String,
    pub machine_id: String,
    pub remote_workspace_id: String,
    pub online: bool,
    pub revoked: bool,
    pub tool_profile: String,
}

impl NativeMember {
    pub fn read_only(&self) -> bool {
        !matches!(self.tool_profile.as_str(), "file-write" | "commands")
    }

    pub fn listed(&self) -> Value {
        json!({
            "id": self.id, "name": self.name, "kind": "node",
            "machine_id": self.machine_id, "online": self.online, "revoked": self.revoked,
            "tool_profile": self.tool_profile,
            "read_only": self.read_only(),
            "commands": self.tool_profile == "commands",
        })
    }
}
