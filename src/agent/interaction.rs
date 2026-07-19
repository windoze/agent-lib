//! Generalized user interaction requests, evolving tool approval.
//!
//! The effect model treats a yes/no tool approval as one *sub-type* of a broader
//! interaction: the machine reifies a [`Requirement::NeedInteraction`] whenever
//! it must ask the "user" something. Beyond degenerate approval, an interaction
//! can carry an open [`Question`](InteractionKind::Question) or a
//! [`Choice`](InteractionKind::Choice) between fixed options.
//!
//! # Relationship to the old approval types
//!
//! The stage-0 migration keeps every legacy approval type
//! ([`ApprovalRequirement`], [`ApprovalResponse`],
//! [`ApprovalDecision`](crate::agent::ApprovalDecision),
//! [`ToolApprovalPolicy`](crate::agent::ToolApprovalPolicy), ...) intact and
//! re-exported. This module only *wraps* them:
//!
//! - [`InteractionKind::Approval`] embeds an [`ApprovalRequirement`] verbatim.
//! - [`InteractionResponse::Approval`] embeds an [`ApprovalResponse`] verbatim,
//!   and converts losslessly to/from it (see [`From`]/[`TryFrom`]).
//!
//! [`ToolApprovalPolicy`](crate::agent::ToolApprovalPolicy) is now one *backend*
//! of an interaction handler rather than a policy a loop calls directly, and
//! approvals are answered through the generic
//! [`RequirementResult::Interaction`] return path.
//!
//! [`Requirement::NeedInteraction`]: crate::agent::RequirementKind::NeedInteraction
//! [`RequirementResult::Interaction`]: crate::agent::RequirementResult::Interaction

use crate::{
    agent::{
        StepId,
        approval::{ApprovalRequirement, ApprovalResponse},
        permission::{PermissionRequest, PermissionResponse},
    },
    conversation::ToolCallId,
};
use serde::{Deserialize, Serialize};
use std::fmt;
use thiserror::Error;

/// An interaction the machine needs the "user" to resolve.
///
/// This is a persistable request *description*: its resolution arrives
/// separately as an [`InteractionResponse`]. The [`step_id`](Self::step_id)
/// addresses the step awaiting the answer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Interaction {
    /// Step awaiting the interaction result.
    pub step_id: StepId,
    /// What is being asked of the "user".
    pub kind: InteractionKind,
    /// Optional rendering attribution for delegated interactions.
    ///
    /// Root-agent interactions leave this empty. Delegated interactions can set
    /// it to show which delegate asked and at what delegation depth. This is
    /// only display attribution; privileged-action authority remains on
    /// [`PermissionRequest::actor`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<Box<InteractionOrigin>>,
}

/// Display attribution for an [`Interaction`] raised by a delegated agent.
///
/// `delegate` is the child/delegate name that should be shown to the user, and
/// `depth` is the delegation depth from [`RunContext`](crate::agent::RunContext).
/// This attribution answers "who asked through the delegation chain" for
/// rendering only; it is intentionally separate from
/// [`PermissionRequest::actor`], which remains the permission subject.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InteractionOrigin {
    /// Delegate name to display as the interaction origin.
    pub delegate: String,
    /// Delegation depth associated with the delegate.
    pub depth: u32,
}

impl InteractionOrigin {
    /// Creates delegated rendering attribution.
    #[must_use]
    pub fn new(delegate: impl Into<String>, depth: u32) -> Self {
        Self {
            delegate: delegate.into(),
            depth,
        }
    }
}

impl Interaction {
    /// Creates an interaction addressed to `step_id`.
    #[must_use]
    pub const fn new(step_id: StepId, kind: InteractionKind) -> Self {
        Self {
            step_id,
            kind,
            origin: None,
        }
    }

    /// Annotates this interaction with delegated rendering attribution.
    ///
    /// Use this in routing layers when an interaction raised by a child agent is
    /// forwarded to a parent handler. Root-agent interactions should keep
    /// [`origin`](Self::origin) empty.
    #[must_use]
    pub fn with_origin(mut self, origin: InteractionOrigin) -> Self {
        self.origin = Some(Box::new(origin));
        self
    }

    /// Creates a degenerate yes/no approval interaction for one tool call.
    #[must_use]
    pub fn approval(
        step_id: StepId,
        call_id: ToolCallId,
        requirement: ApprovalRequirement,
    ) -> Self {
        Self::new(
            step_id,
            InteractionKind::Approval {
                call_id,
                requirement,
            },
        )
    }

    /// Creates an open-question interaction.
    #[must_use]
    pub fn question(step_id: StepId, prompt: String) -> Self {
        Self::new(step_id, InteractionKind::Question { prompt })
    }

    /// Creates a fixed-option choice interaction.
    #[must_use]
    pub fn choice(step_id: StepId, prompt: String, options: Vec<String>) -> Self {
        Self::new(step_id, InteractionKind::Choice { prompt, options })
    }

    /// Creates a permission interaction for a privileged agent action.
    #[must_use]
    pub fn permission(step_id: StepId, request: PermissionRequest) -> Self {
        Self::new(step_id, InteractionKind::Permission { request })
    }

    /// Returns the step awaiting this interaction.
    #[must_use]
    pub const fn step_id(&self) -> StepId {
        self.step_id
    }

    /// Returns what is being asked.
    #[must_use]
    pub const fn kind(&self) -> &InteractionKind {
        &self.kind
    }

    /// Returns delegated rendering attribution, when this interaction was
    /// raised by a child agent.
    #[must_use]
    pub fn origin(&self) -> Option<&InteractionOrigin> {
        self.origin.as_deref()
    }

    /// Checks that `response` is a valid answer to this interaction.
    ///
    /// The response family must match the request family, and family-specific
    /// invariants must hold: a `Choice` index must fall within the options
    /// range, an `Approval` response must address this interaction's `step_id`
    /// and tool `call_id`, and a `Permission` response must carry the same
    /// `action_id` as the pending request.
    ///
    /// # Errors
    ///
    /// Returns a classified [`InteractionError`]:
    /// [`ResponseKindMismatch`](InteractionError::ResponseKindMismatch) when the
    /// response family does not match, [`ChoiceOutOfRange`](InteractionError::ChoiceOutOfRange)
    /// for an index past the options,
    /// [`StepMismatch`](InteractionError::StepMismatch) /
    /// [`CallMismatch`](InteractionError::CallMismatch) when an approval
    /// response addresses a different step or tool call, or
    /// [`ActionMismatch`](InteractionError::ActionMismatch) when a permission
    /// response addresses a different action.
    pub fn accepts_response(&self, response: &InteractionResponse) -> Result<(), InteractionError> {
        match (&self.kind, response) {
            (
                InteractionKind::Approval { call_id, .. },
                InteractionResponse::Approval(approval),
            ) => {
                if approval.step_id() != self.step_id {
                    return Err(InteractionError::StepMismatch {
                        expected: self.step_id,
                        actual: approval.step_id(),
                    });
                }
                if approval.call_id() != *call_id {
                    return Err(InteractionError::CallMismatch {
                        expected: *call_id,
                        actual: approval.call_id(),
                    });
                }
                Ok(())
            }
            (InteractionKind::Question { .. }, InteractionResponse::Answer(_)) => Ok(()),
            (InteractionKind::Choice { options, .. }, InteractionResponse::Choice(index)) => {
                if *index < options.len() {
                    Ok(())
                } else {
                    Err(InteractionError::ChoiceOutOfRange {
                        index: *index,
                        options: options.len(),
                    })
                }
            }
            (
                InteractionKind::Permission { request },
                InteractionResponse::Permission(response),
            ) => {
                if response.action_id() == request.action_id() {
                    Ok(())
                } else {
                    Err(InteractionError::ActionMismatch {
                        expected: request.action_id().to_owned(),
                        actual: response.action_id().to_owned(),
                    })
                }
            }
            (kind, response) => Err(InteractionError::ResponseKindMismatch {
                expected: kind.tag(),
                actual: response.tag(),
            }),
        }
    }
}

/// What an [`Interaction`] asks of the "user".
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InteractionKind {
    /// Degenerate yes/no approval for a specific tool call (carries legacy approval).
    Approval {
        /// Framework tool-call identity awaiting approval.
        call_id: ToolCallId,
        /// The reused approval requirement (auto-approve vs require).
        requirement: ApprovalRequirement,
    },
    /// An open-ended question or clarification.
    Question {
        /// Prompt shown to the "user".
        prompt: String,
    },
    /// A choice between fixed options.
    Choice {
        /// Prompt shown to the "user".
        prompt: String,
        /// Ordered options the "user" selects from by index.
        options: Vec<String>,
    },
    /// A request to allow a privileged agent action (shell, edit, network,
    /// sub-agent spawn, MCP, ...).
    ///
    /// Unlike [`Approval`](Self::Approval), a permission is not bound to a
    /// framework tool call; it carries a provider-neutral [`PermissionRequest`].
    Permission {
        /// The privileged action awaiting a decision.
        request: PermissionRequest,
    },
}

impl InteractionKind {
    /// Returns the family this interaction belongs to.
    #[must_use]
    pub const fn tag(&self) -> InteractionKindTag {
        match self {
            Self::Approval { .. } => InteractionKindTag::Approval,
            Self::Question { .. } => InteractionKindTag::Question,
            Self::Choice { .. } => InteractionKindTag::Choice,
            Self::Permission { .. } => InteractionKindTag::Permission,
        }
    }
}

/// A resolution to an [`Interaction`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InteractionResponse {
    /// Answer to an [`InteractionKind::Approval`]; reuses [`ApprovalResponse`].
    Approval(ApprovalResponse),
    /// Free-form answer to an [`InteractionKind::Question`].
    Answer(String),
    /// Zero-based selected index for an [`InteractionKind::Choice`].
    Choice(usize),
    /// Decision on an [`InteractionKind::Permission`]; carries a
    /// [`PermissionResponse`] correlated by `action_id`.
    Permission(PermissionResponse),
}

impl InteractionResponse {
    /// Creates a free-form answer response.
    #[must_use]
    pub fn answer(text: String) -> Self {
        Self::Answer(text)
    }

    /// Creates a choice response validated against `interaction`.
    ///
    /// # Errors
    ///
    /// Returns [`InteractionError::ResponseKindMismatch`] when `interaction` is
    /// not a [`Choice`](InteractionKind::Choice), or
    /// [`InteractionError::ChoiceOutOfRange`] when `index` is past the options.
    pub fn choice_for(interaction: &Interaction, index: usize) -> Result<Self, InteractionError> {
        let response = Self::Choice(index);
        interaction.accepts_response(&response)?;
        Ok(response)
    }

    /// Wraps an [`ApprovalResponse`], validated against `interaction`.
    ///
    /// # Errors
    ///
    /// Returns [`InteractionError::ResponseKindMismatch`] when `interaction` is
    /// not an [`Approval`](InteractionKind::Approval), or
    /// [`InteractionError::StepMismatch`] / [`InteractionError::CallMismatch`]
    /// when `response` addresses a different step or tool call.
    pub fn approval_for(
        interaction: &Interaction,
        response: ApprovalResponse,
    ) -> Result<Self, InteractionError> {
        let wrapped = Self::Approval(response);
        interaction.accepts_response(&wrapped)?;
        Ok(wrapped)
    }

    /// Wraps a [`PermissionResponse`], validated against `interaction`.
    ///
    /// # Errors
    ///
    /// Returns [`InteractionError::ResponseKindMismatch`] when `interaction` is
    /// not a [`Permission`](InteractionKind::Permission), or
    /// [`InteractionError::ActionMismatch`] when `response` addresses a
    /// different action than the pending request.
    pub fn permission_for(
        interaction: &Interaction,
        response: PermissionResponse,
    ) -> Result<Self, InteractionError> {
        let wrapped = Self::Permission(response);
        interaction.accepts_response(&wrapped)?;
        Ok(wrapped)
    }

    /// Returns the family this response satisfies.
    #[must_use]
    pub const fn tag(&self) -> InteractionKindTag {
        match self {
            Self::Approval(_) => InteractionKindTag::Approval,
            Self::Answer(_) => InteractionKindTag::Question,
            Self::Choice(_) => InteractionKindTag::Choice,
            Self::Permission(_) => InteractionKindTag::Permission,
        }
    }
}

impl From<ApprovalResponse> for InteractionResponse {
    fn from(response: ApprovalResponse) -> Self {
        Self::Approval(response)
    }
}

impl TryFrom<InteractionResponse> for ApprovalResponse {
    type Error = InteractionError;

    fn try_from(response: InteractionResponse) -> Result<Self, Self::Error> {
        match response {
            InteractionResponse::Approval(approval) => Ok(approval),
            other => Err(InteractionError::ResponseKindMismatch {
                expected: InteractionKindTag::Approval,
                actual: other.tag(),
            }),
        }
    }
}

/// Discriminant identifying the family of an [`InteractionKind`] or
/// [`InteractionResponse`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InteractionKindTag {
    /// Degenerate yes/no approval.
    Approval,
    /// Open-ended question.
    Question,
    /// Fixed-option choice.
    Choice,
    /// Privileged-action permission request.
    Permission,
}

impl fmt::Display for InteractionKindTag {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::Approval => "approval",
            Self::Question => "question",
            Self::Choice => "choice",
            Self::Permission => "permission",
        };
        formatter.write_str(text)
    }
}

/// Classified error from validating an [`InteractionResponse`] against its
/// [`Interaction`].
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum InteractionError {
    /// The response family does not match the interaction family.
    #[error("interaction `{expected}` cannot accept a `{actual}` response")]
    ResponseKindMismatch {
        /// Family expected by the interaction.
        expected: InteractionKindTag,
        /// Family actually carried by the response.
        actual: InteractionKindTag,
    },
    /// A choice index fell outside the options range.
    #[error("choice index {index} is out of range for {options} option(s)")]
    ChoiceOutOfRange {
        /// Selected index supplied by the response.
        index: usize,
        /// Number of options offered by the interaction.
        options: usize,
    },
    /// An approval response addressed a different step than the interaction.
    #[error("approval response step {actual} does not match interaction step {expected}")]
    StepMismatch {
        /// Step the interaction awaits.
        expected: StepId,
        /// Step carried by the approval response.
        actual: StepId,
    },
    /// An approval response addressed a different tool call than the interaction.
    #[error("approval response tool call {actual} does not match interaction tool call {expected}")]
    CallMismatch {
        /// Tool call the interaction awaits.
        expected: ToolCallId,
        /// Tool call carried by the approval response.
        actual: ToolCallId,
    },
    /// A permission response addressed a different action than the interaction.
    #[error("permission response action `{actual}` does not match interaction action `{expected}`")]
    ActionMismatch {
        /// Action identity the interaction awaits.
        expected: String,
        /// Action identity carried by the permission response.
        actual: String,
    },
}

#[cfg(test)]
mod tests {
    use super::{
        Interaction, InteractionError, InteractionKindTag, InteractionOrigin, InteractionResponse,
    };
    use crate::{
        agent::{
            AgentId, StepId,
            approval::{ApprovalRequirement, ApprovalResponse},
            permission::{
                PermissionCategory, PermissionRequest, PermissionResponse, PermissionRisk,
            },
        },
        conversation::ToolCallId,
    };
    use serde::{Serialize, de::DeserializeOwned};
    use serde_json::json;
    use std::fmt::Debug;

    fn actor() -> AgentId {
        "018f0d9c-7b6a-7c12-8f31-1234567890c1"
            .parse()
            .expect("agent id")
    }

    fn permission_request(action_id: &str) -> PermissionRequest {
        PermissionRequest::new(
            action_id.to_owned(),
            actor(),
            PermissionCategory::Shell,
            "run tests".to_owned(),
            serde_json::json!({ "command": "cargo test" }),
            PermissionRisk::Medium,
            None,
        )
    }

    fn step_id() -> StepId {
        "018f0d9c-7b6a-7c12-8f31-1234567890a1"
            .parse()
            .expect("step id")
    }

    fn other_step_id() -> StepId {
        "018f0d9c-7b6a-7c12-8f31-1234567890a2"
            .parse()
            .expect("other step id")
    }

    fn call_id() -> ToolCallId {
        "018f0d9c-7b6a-7c12-8f31-1234567890b1"
            .parse()
            .expect("tool call id")
    }

    fn other_call_id() -> ToolCallId {
        "018f0d9c-7b6a-7c12-8f31-1234567890b2"
            .parse()
            .expect("other tool call id")
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
    fn interaction_without_origin_omits_origin_and_round_trips() {
        let interaction = Interaction::question(step_id(), "how?".to_owned());

        assert_eq!(interaction.origin(), None);
        let encoded = serde_json::to_value(&interaction).expect("serialize");
        assert!(encoded.get("origin").is_none());
        let decoded: Interaction = serde_json::from_value(encoded).expect("deserialize");
        assert_eq!(decoded, interaction);
        assert_eq!(decoded.origin(), None);
    }

    #[test]
    fn interaction_with_origin_round_trips() {
        let origin = InteractionOrigin::new("codex", 1);
        let interaction =
            Interaction::question(step_id(), "approve?".to_owned()).with_origin(origin.clone());

        assert_eq!(interaction.origin(), Some(&origin));
        let encoded = serde_json::to_value(&interaction).expect("serialize");
        assert_eq!(
            encoded.get("origin"),
            Some(&json!({ "delegate": "codex", "depth": 1 }))
        );
        let decoded: Interaction = serde_json::from_value(encoded).expect("deserialize");
        assert_eq!(decoded, interaction);
    }

    #[test]
    fn interaction_deserializes_legacy_json_without_origin() {
        let decoded: Interaction = serde_json::from_value(json!({
            "step_id": step_id().to_string(),
            "kind": { "question": { "prompt": "old" } }
        }))
        .expect("deserialize legacy interaction");

        assert_eq!(decoded.origin(), None);
        assert_eq!(decoded.kind().tag(), InteractionKindTag::Question);
    }

    #[test]
    fn every_interaction_kind_round_trips() {
        let approval = Interaction::approval(
            step_id(),
            call_id(),
            ApprovalRequirement::required(Some("why".to_owned())),
        );
        let question = Interaction::question(step_id(), "how?".to_owned());
        let choice =
            Interaction::choice(step_id(), "pick".to_owned(), vec!["a".into(), "b".into()]);

        for interaction in [&approval, &question, &choice] {
            assert_json_round_trip(interaction);
        }

        assert_eq!(approval.kind().tag(), InteractionKindTag::Approval);
        assert_eq!(question.kind().tag(), InteractionKindTag::Question);
        assert_eq!(choice.kind().tag(), InteractionKindTag::Choice);
    }

    #[test]
    fn every_interaction_response_round_trips() {
        let responses = [
            InteractionResponse::Approval(ApprovalResponse::approve(step_id(), call_id())),
            InteractionResponse::answer("free text".to_owned()),
            InteractionResponse::Choice(1),
            InteractionResponse::Permission(PermissionResponse::approve("act-1".to_owned())),
        ];
        for response in &responses {
            assert_json_round_trip(response);
        }
    }

    #[test]
    fn choice_response_accepts_only_in_range_indices() {
        let interaction =
            Interaction::choice(step_id(), "pick".to_owned(), vec!["a".into(), "b".into()]);

        assert_eq!(
            InteractionResponse::choice_for(&interaction, 1),
            Ok(InteractionResponse::Choice(1))
        );
        assert_eq!(
            InteractionResponse::choice_for(&interaction, 2),
            Err(InteractionError::ChoiceOutOfRange {
                index: 2,
                options: 2,
            })
        );
    }

    #[test]
    fn approval_response_must_match_step_and_call() {
        let interaction =
            Interaction::approval(step_id(), call_id(), ApprovalRequirement::required(None));

        let matching = ApprovalResponse::approve(step_id(), call_id());
        assert_eq!(
            InteractionResponse::approval_for(&interaction, matching.clone()),
            Ok(InteractionResponse::Approval(matching))
        );

        let wrong_step = ApprovalResponse::approve(other_step_id(), call_id());
        assert_eq!(
            InteractionResponse::approval_for(&interaction, wrong_step),
            Err(InteractionError::StepMismatch {
                expected: step_id(),
                actual: other_step_id(),
            })
        );

        let wrong_call = ApprovalResponse::approve(step_id(), other_call_id());
        assert_eq!(
            InteractionResponse::approval_for(&interaction, wrong_call),
            Err(InteractionError::CallMismatch {
                expected: call_id(),
                actual: other_call_id(),
            })
        );
    }

    #[test]
    fn accepts_response_rejects_mismatched_families() {
        let question = Interaction::question(step_id(), "how?".to_owned());
        let response = InteractionResponse::Choice(0);
        assert_eq!(
            question.accepts_response(&response),
            Err(InteractionError::ResponseKindMismatch {
                expected: InteractionKindTag::Question,
                actual: InteractionKindTag::Choice,
            })
        );
    }

    #[test]
    fn permission_interaction_has_permission_tag_and_round_trips() {
        let interaction = Interaction::permission(step_id(), permission_request("act-1"));

        assert_eq!(interaction.kind().tag(), InteractionKindTag::Permission);
        assert_json_round_trip(&interaction);
    }

    #[test]
    fn permission_response_family_matches() {
        let interaction = Interaction::permission(step_id(), permission_request("act-1"));

        let response = PermissionResponse::approve("act-1".to_owned());
        assert_eq!(
            InteractionResponse::permission_for(&interaction, response.clone()),
            Ok(InteractionResponse::Permission(response))
        );

        let wrong_family = InteractionResponse::answer("nope".to_owned());
        assert_eq!(
            interaction.accepts_response(&wrong_family),
            Err(InteractionError::ResponseKindMismatch {
                expected: InteractionKindTag::Permission,
                actual: InteractionKindTag::Question,
            })
        );

        let permission_for_non_permission = Interaction::question(step_id(), "how?".to_owned());
        assert_eq!(
            InteractionResponse::permission_for(
                &permission_for_non_permission,
                PermissionResponse::approve("act-1".to_owned())
            ),
            Err(InteractionError::ResponseKindMismatch {
                expected: InteractionKindTag::Question,
                actual: InteractionKindTag::Permission,
            })
        );
    }

    #[test]
    fn permission_response_action_id_mismatch_rejected() {
        let interaction = Interaction::permission(step_id(), permission_request("act-1"));

        let mismatched = PermissionResponse::deny("act-2".to_owned(), Some("no".to_owned()));
        assert_eq!(
            InteractionResponse::permission_for(&interaction, mismatched),
            Err(InteractionError::ActionMismatch {
                expected: "act-1".to_owned(),
                actual: "act-2".to_owned(),
            })
        );
    }

    #[test]
    fn approval_variant_round_trips_through_legacy_type_losslessly() {
        let approval = ApprovalResponse::deny(step_id(), call_id(), Some("no".to_owned()));

        let wrapped: InteractionResponse = approval.clone().into();
        assert_eq!(wrapped, InteractionResponse::Approval(approval.clone()));

        let unwrapped: ApprovalResponse = wrapped.try_into().expect("approval variant");
        assert_eq!(unwrapped, approval);

        let not_approval = InteractionResponse::answer("text".to_owned());
        assert_eq!(
            ApprovalResponse::try_from(not_approval),
            Err(InteractionError::ResponseKindMismatch {
                expected: InteractionKindTag::Approval,
                actual: InteractionKindTag::Question,
            })
        );
    }
}
