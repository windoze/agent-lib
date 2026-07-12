//! Message and role types shared by provider adapters.

use serde::{Deserialize, Serialize};

/// Provider-neutral chat participant roles.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// A message authored by the end user.
    User,
    /// A message authored by the assistant model.
    Assistant,
    /// System-level instructions for the model.
    System,
    /// Tool output or tool-related content.
    Tool,
}

#[cfg(test)]
mod tests {
    use super::Role;

    #[test]
    fn role_round_trips_through_lowercase_wire_name() {
        let json = serde_json::to_string(&Role::Assistant).expect("serialize role");
        assert_eq!(json, "\"assistant\"");

        let decoded: Role = serde_json::from_str(&json).expect("deserialize role");
        assert_eq!(decoded, Role::Assistant);
    }
}
