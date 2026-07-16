//! Generalized permission requests for external and host-mediated actions.
//!
//! Where [`ApprovalRequirement`](crate::agent::ApprovalRequirement) is bound to
//! a provider-neutral [`ToolCall`](crate::model::tool::ToolCall) and answers the
//! framework's own tool approval, a [`PermissionRequest`] describes an arbitrary
//! privileged action an agent wants to take: running a shell command, reading or
//! writing a file, opening a network connection, spawning a sub-agent, or
//! invoking an MCP capability. It is provider-neutral and *not* tied to a
//! [`ToolCallId`](crate::conversation::ToolCallId), so it can carry the
//! permission asks an external coding-agent runtime surfaces.
//!
//! A permission request rides inside an
//! [`InteractionKind::Permission`](crate::agent::InteractionKind::Permission) so
//! it flows through the same [`Interaction`](crate::agent::Interaction)
//! machinery as questions, choices, and legacy approvals.

use crate::agent::AgentId;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;

/// The class of privileged action a [`PermissionRequest`] asks to perform.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionCategory {
    /// Executing a shell command.
    Shell,
    /// Reading from the filesystem.
    FileRead,
    /// Writing to the filesystem.
    FileWrite,
    /// Opening an outbound network connection.
    Network,
    /// Spawning a child agent.
    SpawnAgent,
    /// Invoking an MCP server capability.
    Mcp,
    /// Any other host-mediated action not covered above.
    Other,
}

impl fmt::Display for PermissionCategory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::Shell => "shell",
            Self::FileRead => "file_read",
            Self::FileWrite => "file_write",
            Self::Network => "network",
            Self::SpawnAgent => "spawn_agent",
            Self::Mcp => "mcp",
            Self::Other => "other",
        };
        formatter.write_str(text)
    }
}

/// Estimated blast radius of granting a [`PermissionRequest`].
///
/// Ordered least-to-most severe so a policy can compare risk levels (for
/// example, deny anything at or above [`High`](PermissionRisk::High)).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionRisk {
    /// Read-only or otherwise easily reversible action.
    Low,
    /// Local mutation with contained impact.
    Medium,
    /// Broad or hard-to-reverse mutation.
    High,
    /// Destructive or externally observable action.
    Critical,
}

impl fmt::Display for PermissionRisk {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Critical => "critical",
        };
        formatter.write_str(text)
    }
}

/// A privileged action an agent asks the "user" (or a headless policy) to allow.
///
/// This is a persistable request *description*: its resolution arrives
/// separately as a permission response through the
/// [`Interaction`](crate::agent::Interaction) machinery. The
/// [`action_id`](Self::action_id) is a stable, request-supplied identity used to
/// correlate the eventual decision back to this request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionRequest {
    /// Stable identity used to match this request to its decision.
    pub action_id: String,
    /// Agent that requested the privileged action.
    pub actor: AgentId,
    /// Class of privileged action being requested.
    pub category: PermissionCategory,
    /// Human-readable summary shown to the approver.
    pub summary: String,
    /// Structured, provider-neutral description of the action's subject.
    pub subject: Value,
    /// Estimated blast radius of granting the action.
    pub risk: PermissionRisk,
    /// Optional rationale supplied by the requesting agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl PermissionRequest {
    /// Creates a permission request; `reason` is normalized so an empty string
    /// is dropped.
    #[must_use]
    pub fn new(
        action_id: String,
        actor: AgentId,
        category: PermissionCategory,
        summary: String,
        subject: Value,
        risk: PermissionRisk,
        reason: Option<String>,
    ) -> Self {
        Self {
            action_id,
            actor,
            category,
            summary,
            subject,
            risk,
            reason: reason.filter(|text| !text.is_empty()),
        }
    }

    /// Returns the stable identity used to correlate the decision.
    #[must_use]
    pub fn action_id(&self) -> &str {
        &self.action_id
    }

    /// Returns the agent that requested the action.
    #[must_use]
    pub const fn actor(&self) -> AgentId {
        self.actor
    }

    /// Returns the class of privileged action.
    #[must_use]
    pub const fn category(&self) -> PermissionCategory {
        self.category
    }

    /// Returns the estimated risk of granting the action.
    #[must_use]
    pub const fn risk(&self) -> PermissionRisk {
        self.risk
    }

    /// Returns the optional rationale, if one was supplied.
    #[must_use]
    pub fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }
}

/// External decision supplied for a pending [`PermissionRequest`].
///
/// Mirrors [`ApprovalDecision`](crate::agent::ApprovalDecision) in intent, but
/// is not bound to a [`ToolCallId`](crate::conversation::ToolCallId): it answers
/// a provider-neutral permission ask instead of the framework's own tool
/// approval. A denial carries an optional rationale inline; a timeout is
/// expressed by the backend as a [`Deny`](Self::Deny) or [`Cancel`](Self::Cancel)
/// rather than a distinct variant.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionDecision {
    /// The privileged action may proceed.
    Approve,
    /// The privileged action is refused, with an optional rationale.
    Deny {
        /// Stable reason shown to the requesting agent, if supplied.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// The pending privileged action should be cancelled.
    Cancel,
}

/// Data-only response to a [`PermissionRequest`].
///
/// The [`action_id`](Self::action_id) correlates the decision back to the
/// originating request; validation rejects a response whose `action_id` does not
/// match the pending [`Interaction`](crate::agent::Interaction).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PermissionResponse {
    action_id: String,
    decision: PermissionDecision,
}

impl PermissionResponse {
    /// Creates a permission response correlated to `action_id`.
    #[must_use]
    pub fn new(action_id: String, decision: PermissionDecision) -> Self {
        Self {
            action_id,
            decision,
        }
    }

    /// Creates an approve response.
    #[must_use]
    pub fn approve(action_id: String) -> Self {
        Self::new(action_id, PermissionDecision::Approve)
    }

    /// Creates a deny response; an empty `reason` is normalized to `None`.
    #[must_use]
    pub fn deny(action_id: String, reason: Option<String>) -> Self {
        Self::new(
            action_id,
            PermissionDecision::Deny {
                reason: reason.filter(|text| !text.is_empty()),
            },
        )
    }

    /// Creates a cancel response.
    #[must_use]
    pub fn cancel(action_id: String) -> Self {
        Self::new(action_id, PermissionDecision::Cancel)
    }

    /// Returns the identity correlating this response to its request.
    #[must_use]
    pub fn action_id(&self) -> &str {
        &self.action_id
    }

    /// Returns the external decision.
    #[must_use]
    pub const fn decision(&self) -> &PermissionDecision {
        &self.decision
    }
}

#[cfg(test)]
mod tests {
    use super::{
        PermissionCategory, PermissionDecision, PermissionRequest, PermissionResponse,
        PermissionRisk,
    };
    use crate::agent::AgentId;
    use serde::{Serialize, de::DeserializeOwned};
    use serde_json::json;
    use std::fmt::Debug;

    fn actor() -> AgentId {
        "018f0d9c-7b6a-7c12-8f31-1234567890c1"
            .parse()
            .expect("agent id")
    }

    fn assert_json_round_trip<T>(value: &T)
    where
        T: Debug + PartialEq + Serialize + DeserializeOwned,
    {
        let encoded = serde_json::to_value(value).expect("serialize");
        let decoded: T = serde_json::from_value(encoded).expect("deserialize");
        assert_eq!(&decoded, value);
    }

    #[test]
    fn permission_request_round_trips() {
        let request = PermissionRequest::new(
            "act-1".to_owned(),
            actor(),
            PermissionCategory::Shell,
            "run tests".to_owned(),
            json!({ "command": "cargo test" }),
            PermissionRisk::Medium,
            Some("verify the change".to_owned()),
        );
        assert_json_round_trip(&request);
    }

    #[test]
    fn permission_category_and_risk_round_trip() {
        for category in [
            PermissionCategory::Shell,
            PermissionCategory::FileRead,
            PermissionCategory::FileWrite,
            PermissionCategory::Network,
            PermissionCategory::SpawnAgent,
            PermissionCategory::Mcp,
            PermissionCategory::Other,
        ] {
            assert_json_round_trip(&category);
        }
        for risk in [
            PermissionRisk::Low,
            PermissionRisk::Medium,
            PermissionRisk::High,
            PermissionRisk::Critical,
        ] {
            assert_json_round_trip(&risk);
        }
    }

    #[test]
    fn permission_risk_orders_least_to_most_severe() {
        assert!(PermissionRisk::Low < PermissionRisk::Medium);
        assert!(PermissionRisk::Medium < PermissionRisk::High);
        assert!(PermissionRisk::High < PermissionRisk::Critical);
    }

    #[test]
    fn permission_request_normalizes_empty_reason() {
        let request = PermissionRequest::new(
            "act-2".to_owned(),
            actor(),
            PermissionCategory::Network,
            "fetch".to_owned(),
            json!({ "url": "https://example.com" }),
            PermissionRisk::Low,
            Some(String::new()),
        );
        assert_eq!(request.reason(), None);
    }

    #[test]
    fn permission_category_and_risk_render_snake_case() {
        assert_eq!(PermissionCategory::FileWrite.to_string(), "file_write");
        assert_eq!(PermissionCategory::SpawnAgent.to_string(), "spawn_agent");
        assert_eq!(PermissionRisk::Critical.to_string(), "critical");
    }

    #[test]
    fn permission_response_round_trips_with_each_decision() {
        for response in [
            PermissionResponse::approve("act-1".to_owned()),
            PermissionResponse::deny("act-1".to_owned(), Some("policy denied".to_owned())),
            PermissionResponse::deny("act-1".to_owned(), None),
            PermissionResponse::cancel("act-1".to_owned()),
        ] {
            assert_json_round_trip(&response);
        }
    }

    #[test]
    fn permission_deny_normalizes_empty_reason() {
        let response = PermissionResponse::deny("act-1".to_owned(), Some(String::new()));
        assert_eq!(
            response.decision(),
            &PermissionDecision::Deny { reason: None }
        );
        let encoded = serde_json::to_value(&response).expect("serialize");
        assert!(encoded["decision"]["deny"].get("reason").is_none());
    }

    #[test]
    fn permission_response_exposes_action_id_and_decision() {
        let response = PermissionResponse::approve("act-42".to_owned());
        assert_eq!(response.action_id(), "act-42");
        assert_eq!(response.decision(), &PermissionDecision::Approve);
    }
}
