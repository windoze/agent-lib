//! Tool approval runtime boundaries.
//!
//! Approval state is split deliberately: requests and responses are data-only
//! values, while the policy and pending responder are live runtime handles owned
//! by an [`crate::agent::AgentLoop`] implementation.

use crate::{agent::StepId, conversation::ToolCallId, model::tool::ToolCall};
use serde::{Deserialize, Serialize};
use std::fmt;
use thiserror::Error;

/// Runtime policy result for one tool call.
///
/// Derives `serde` (pure, non-behavioral) so it can ride inside a persistable
/// [`InteractionKind::Approval`](crate::agent::InteractionKind::Approval)
/// request without redefining the type.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalRequirement {
    /// Execute the tool without pausing for external approval.
    AutoApprove,
    /// Pause the feed stream and emit an approval request before execution.
    RequireApproval {
        /// Stable reason shown to the external approver.
        reason: Option<String>,
    },
}

impl ApprovalRequirement {
    /// Creates an approval requirement with optional non-empty reason text.
    #[must_use]
    pub fn required(reason: Option<String>) -> Self {
        Self::RequireApproval {
            reason: reason.and_then(non_empty),
        }
    }

    /// Returns the approval reason, if one was supplied.
    #[must_use]
    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::AutoApprove => None,
            Self::RequireApproval { reason } => reason.as_deref(),
        }
    }
}

/// Live runtime policy that decides whether a tool call needs approval.
pub trait ToolApprovalPolicy: Send + Sync + fmt::Debug {
    /// Returns the approval requirement for one provider-neutral tool call.
    fn approval_requirement(&self, call_id: ToolCallId, call: &ToolCall) -> ApprovalRequirement;
}

/// Approval policy that never pauses tool execution.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoApprovalPolicy;

impl ToolApprovalPolicy for NoApprovalPolicy {
    fn approval_requirement(&self, _call_id: ToolCallId, _call: &ToolCall) -> ApprovalRequirement {
        ApprovalRequirement::AutoApprove
    }
}

/// External decision supplied for a pending approval request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    /// The tool may execute normally.
    Approve,
    /// The tool is denied by policy or human decision.
    Deny,
    /// The approval window expired before an allow decision arrived.
    Timeout,
    /// The pending tool execution should be cancelled.
    Cancel,
}

/// Data-only response to a tool approval request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalResponse {
    step_id: StepId,
    call_id: ToolCallId,
    decision: ApprovalDecision,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    message: Option<String>,
}

impl ApprovalResponse {
    /// Creates an approval response with optional stable message text.
    #[must_use]
    pub fn new(
        step_id: StepId,
        call_id: ToolCallId,
        decision: ApprovalDecision,
        message: Option<String>,
    ) -> Self {
        Self {
            step_id,
            call_id,
            decision,
            message: message.and_then(non_empty),
        }
    }

    /// Creates an allow response.
    #[must_use]
    pub fn approve(step_id: StepId, call_id: ToolCallId) -> Self {
        Self::new(step_id, call_id, ApprovalDecision::Approve, None)
    }

    /// Creates a deny response.
    #[must_use]
    pub fn deny(step_id: StepId, call_id: ToolCallId, message: Option<String>) -> Self {
        Self::new(step_id, call_id, ApprovalDecision::Deny, message)
    }

    /// Creates a timeout response.
    #[must_use]
    pub fn timeout(step_id: StepId, call_id: ToolCallId, message: Option<String>) -> Self {
        Self::new(step_id, call_id, ApprovalDecision::Timeout, message)
    }

    /// Creates a cancel response.
    #[must_use]
    pub fn cancel(step_id: StepId, call_id: ToolCallId, message: Option<String>) -> Self {
        Self::new(step_id, call_id, ApprovalDecision::Cancel, message)
    }

    /// Returns the step awaiting approval.
    #[must_use]
    pub const fn step_id(&self) -> StepId {
        self.step_id
    }

    /// Returns the tool call awaiting approval.
    #[must_use]
    pub const fn call_id(&self) -> ToolCallId {
        self.call_id
    }

    /// Returns the external decision.
    #[must_use]
    pub const fn decision(&self) -> ApprovalDecision {
        self.decision
    }

    /// Returns stable external message text, if supplied.
    #[must_use]
    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }
}

/// Classified approval runtime error.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum ApprovalError {
    /// The loop has no pending approval matching the submitted response.
    #[error("no pending approval for step {step_id} and tool call {call_id}")]
    NoPending {
        /// Step identity supplied by the caller.
        step_id: StepId,
        /// Tool call identity supplied by the caller.
        call_id: ToolCallId,
    },
    /// A second approval waiter was registered for the same tool call.
    #[error("approval already pending for tool call {call_id}")]
    DuplicatePending {
        /// Tool call identity already waiting for approval.
        call_id: ToolCallId,
    },
    /// The live responder was dropped before a response could be delivered.
    #[error("approval responder for tool call {call_id} is no longer active")]
    ResponderClosed {
        /// Tool call identity whose responder is no longer active.
        call_id: ToolCallId,
    },
}

fn non_empty(value: String) -> Option<String> {
    if value.is_empty() { None } else { Some(value) }
}

#[cfg(test)]
mod tests {
    use super::{
        ApprovalDecision, ApprovalError, ApprovalRequirement, ApprovalResponse, NoApprovalPolicy,
        ToolApprovalPolicy,
    };
    use crate::{conversation::ToolCallId, model::tool::ToolCall};
    use serde_json::json;

    fn step_id() -> crate::agent::StepId {
        "018f0d9c-7b6a-7c12-8f31-123456789008"
            .parse()
            .expect("step id")
    }

    fn call_id() -> ToolCallId {
        "018f0d9c-7b6a-7c12-8f31-123456789009"
            .parse()
            .expect("tool call id")
    }

    fn call() -> ToolCall {
        ToolCall {
            id: "call-weather".to_owned(),
            name: "get_weather".to_owned(),
            input: json!({ "city": "Shanghai" }),
        }
    }

    #[test]
    fn approval_response_round_trips_with_each_decision() {
        for decision in [
            ApprovalDecision::Approve,
            ApprovalDecision::Deny,
            ApprovalDecision::Timeout,
            ApprovalDecision::Cancel,
        ] {
            let response = ApprovalResponse::new(
                step_id(),
                call_id(),
                decision,
                Some("external decision".to_owned()),
            );
            let encoded = serde_json::to_value(&response).expect("serialize response");
            let decoded: ApprovalResponse =
                serde_json::from_value(encoded).expect("deserialize response");

            assert_eq!(decoded, response);
        }
    }

    #[test]
    fn empty_approval_reason_and_message_are_omitted() {
        let requirement = ApprovalRequirement::required(Some(String::new()));
        assert_eq!(requirement.reason(), None);

        let response = ApprovalResponse::deny(step_id(), call_id(), Some(String::new()));
        assert_eq!(response.message(), None);
        let encoded = serde_json::to_value(&response).expect("serialize response");
        assert!(encoded.get("message").is_none());
    }

    #[test]
    fn no_approval_policy_auto_approves_calls() {
        let policy = NoApprovalPolicy;
        assert_eq!(
            policy.approval_requirement(call_id(), &call()),
            ApprovalRequirement::AutoApprove
        );
    }

    #[test]
    fn approval_error_is_stable_data_for_missing_responder() {
        let error = ApprovalError::NoPending {
            step_id: step_id(),
            call_id: call_id(),
        };

        assert!(error.to_string().contains("no pending approval"));
    }
}
