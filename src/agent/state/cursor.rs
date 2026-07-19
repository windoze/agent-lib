//! Data-only Agent loop cursor types.
//!
//! [`LoopCursor`] is the core of the whole machine's serializable state: it does
//! not merely hint where a future loop can resume, it precisely records *which
//! reified [`Requirement`](crate::agent::Requirement) the machine is stuck on*.
//! The step-, tool-, and approval-wait cursors each carry the addressing of the
//! requirement(s) they await — a [`RequirementId`] plus the emitting
//! [`AgentPath`] origin (always the root path during the stage-0 single-machine
//! migration). A driver can therefore serialize a paused machine, restore it in
//! another process, and rebuild the pending-requirement registry straight from
//! the cursor.
//!
//! The requirement binding is modeled as `Option` so the same cursor shape can
//! serve both requirement-bearing waits — which the sans-io machine always
//! stamps with the outstanding [`RequirementId`] — and any restored cursor whose
//! binding is not yet re-registered. Live handles
//! ([`AgentRuntimeHandles`](super::AgentRuntimeHandles)) stay out of this serde
//! shape entirely.

use crate::{
    agent::{AgentPath, RequirementId, StepId},
    conversation::ToolCallId,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

use super::AgentStateError;

/// Precise address of the single requirement a cursor is stuck on.
///
/// Pairs the host-supplied [`RequirementId`] with the [`AgentPath`] origin of
/// the machine that emitted it. During the stage-0 single-machine migration the
/// origin is always the root path; the field is carried now so signatures do not
/// change when nested machines land in stage 4.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CursorRequirement {
    id: RequirementId,
    #[serde(default, skip_serializing_if = "AgentPath::is_root")]
    origin: AgentPath,
}

impl CursorRequirement {
    /// Creates a requirement address from an id and its emitting origin.
    #[must_use]
    pub const fn new(id: RequirementId, origin: AgentPath) -> Self {
        Self { id, origin }
    }

    /// Creates a requirement address rooted at the top-level machine.
    #[must_use]
    pub fn root(id: RequirementId) -> Self {
        Self {
            id,
            origin: AgentPath::root(),
        }
    }

    /// Returns the addressed requirement identity.
    #[must_use]
    pub const fn id(&self) -> RequirementId {
        self.id
    }

    /// Returns the origin of the machine that emitted the requirement.
    #[must_use]
    pub const fn origin(&self) -> &AgentPath {
        &self.origin
    }
}

/// Address of a batch of tool requirements awaited by one step.
///
/// Every tool call in the batch is emitted by the same machine, so a single
/// [`AgentPath`] origin is shared; each provider-independent [`ToolCallId`] maps
/// to the [`RequirementId`] the driver must fulfill.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolWaitRequirements {
    #[serde(default, skip_serializing_if = "AgentPath::is_root")]
    origin: AgentPath,
    ids: BTreeMap<ToolCallId, RequirementId>,
}

impl ToolWaitRequirements {
    /// Creates a batch requirement address from an origin and its id map.
    #[must_use]
    pub fn new(origin: AgentPath, ids: BTreeMap<ToolCallId, RequirementId>) -> Self {
        Self { origin, ids }
    }

    /// Creates a batch requirement address rooted at the top-level machine.
    #[must_use]
    pub fn root(ids: BTreeMap<ToolCallId, RequirementId>) -> Self {
        Self {
            origin: AgentPath::root(),
            ids,
        }
    }

    /// Returns the shared origin of the tool requirements.
    #[must_use]
    pub const fn origin(&self) -> &AgentPath {
        &self.origin
    }

    /// Returns the map from tool-call identity to requirement identity.
    #[must_use]
    pub const fn ids(&self) -> &BTreeMap<ToolCallId, RequirementId> {
        &self.ids
    }

    /// Returns the requirement bound to `call_id`, if present.
    #[must_use]
    pub fn get(&self, call_id: ToolCallId) -> Option<RequirementId> {
        self.ids.get(&call_id).copied()
    }
}

/// Data-only loop recovery cursor.
///
/// The cursor records the requirement(s) a future Agent loop is stuck on. It
/// does not contain a live stream, task handle, approval responder, or tool
/// executor. The step-, tool-, and approval-wait variants carry the addressing
/// of their outstanding requirement(s) so a driver can rebuild the pending
/// registry after restore (see [`LoopCursor::pending_requirement_ids`]).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", content = "data", rename_all = "snake_case")]
pub enum LoopCursor {
    /// No feed segment is currently active.
    #[default]
    Idle,
    /// An LLM step is currently streaming or awaiting a full response.
    StreamingStep(StepCursor),
    /// The loop is waiting for one or more tool results.
    AwaitingTool(ToolWaitCursor),
    /// The loop is waiting for external approval for one tool call.
    AwaitingApproval(ApprovalCursor),
    /// Cancellation has interrupted the active step and pending state must be closed.
    CancelRecovery(CancelRecoveryCursor),
    /// A queued tool-set reconfiguration is waiting for the driver to resolve a
    /// live registry before the turn boundary can be applied.
    AwaitingReconfig(ReconfigCursor),
    /// The current feed segment reached a normal terminal outcome.
    Done(DoneCursor),
    /// The current feed segment ended with a classified runtime error.
    Error(ErrorCursor),
}

impl LoopCursor {
    /// Creates a streaming-step cursor stuck on an optional `NeedLlm` requirement.
    ///
    /// The sans-io machine supplies the requirement it emitted; the legacy loop,
    /// which awaits the LLM directly, passes `None`.
    #[must_use]
    pub fn streaming_step(step_id: StepId, requirement: Option<CursorRequirement>) -> Self {
        Self::StreamingStep(StepCursor::new(step_id, requirement))
    }

    /// Creates an awaiting-tool cursor after validating the call set.
    ///
    /// When `requirements` is supplied, its keys must exactly match the tool-call
    /// set so every awaited call resolves to a requirement and no stray binding
    /// is recorded.
    ///
    /// # Errors
    ///
    /// Returns [`AgentStateError::EmptyToolWait`] for an empty call set,
    /// [`AgentStateError::DuplicateToolCall`] for repeated tool-call ids, or
    /// [`AgentStateError::ToolRequirementMismatch`] when a supplied binding does
    /// not cover the tool-call set exactly.
    pub fn awaiting_tool(
        step_id: StepId,
        tool_call_ids: Vec<ToolCallId>,
        requirements: Option<ToolWaitRequirements>,
    ) -> Result<Self, AgentStateError> {
        Ok(Self::AwaitingTool(ToolWaitCursor::new(
            step_id,
            tool_call_ids,
            requirements,
        )?))
    }

    /// Creates an awaiting-approval cursor stuck on an optional interaction requirement.
    #[must_use]
    pub fn awaiting_approval(
        step_id: StepId,
        tool_call_id: ToolCallId,
        requirement: Option<CursorRequirement>,
    ) -> Self {
        Self::AwaitingApproval(ApprovalCursor::new(step_id, tool_call_id, requirement))
    }

    /// Creates a cancellation-recovery cursor.
    #[must_use]
    pub const fn cancel_recovery(step_id: Option<StepId>, reason: CancelRecoveryReason) -> Self {
        Self::CancelRecovery(CancelRecoveryCursor::new(step_id, reason))
    }

    /// Creates an awaiting-reconfig cursor stuck on an optional registry requirement.
    ///
    /// `step_id` is the committing assistant step for a during-turn
    /// reconfiguration, or `None` when the reconfiguration is applied at the
    /// start of a fresh user turn.
    #[must_use]
    pub const fn awaiting_reconfig(
        step_id: Option<StepId>,
        requirement: Option<CursorRequirement>,
    ) -> Self {
        Self::AwaitingReconfig(ReconfigCursor::new(step_id, requirement))
    }

    /// Creates a done cursor.
    #[must_use]
    pub const fn done(reason: LoopDoneReason) -> Self {
        Self::Done(DoneCursor::new(reason))
    }

    /// Creates an error cursor with stable diagnostic text.
    ///
    /// # Errors
    ///
    /// Returns [`AgentStateError::EmptyCursorError`] when `message` is empty.
    pub fn error(message: impl Into<String>) -> Result<Self, AgentStateError> {
        Ok(Self::Error(ErrorCursor::new(message)?))
    }

    /// Creates an error cursor with stable diagnostic text and a structured kind.
    ///
    /// The message remains human-readable context; consumers that need stable
    /// classification should inspect [`ErrorCursor::kind`] instead.
    ///
    /// # Errors
    ///
    /// Returns [`AgentStateError::EmptyCursorError`] when `message` is empty.
    pub fn error_with_kind(
        message: impl Into<String>,
        kind: ErrorCursorKind,
    ) -> Result<Self, AgentStateError> {
        Ok(Self::Error(ErrorCursor::with_kind(message, kind)?))
    }

    /// Returns the coarse cursor kind used by transition validation.
    #[must_use]
    pub const fn kind(&self) -> LoopCursorKind {
        match self {
            Self::Idle => LoopCursorKind::Idle,
            Self::StreamingStep(_) => LoopCursorKind::StreamingStep,
            Self::AwaitingTool(_) => LoopCursorKind::AwaitingTool,
            Self::AwaitingApproval(_) => LoopCursorKind::AwaitingApproval,
            Self::CancelRecovery(_) => LoopCursorKind::CancelRecovery,
            Self::AwaitingReconfig(_) => LoopCursorKind::AwaitingReconfig,
            Self::Done(_) => LoopCursorKind::Done,
            Self::Error(_) => LoopCursorKind::Error,
        }
    }

    /// Returns the requirement ids this cursor is currently stuck on.
    ///
    /// A driver uses these to rebuild its pending-requirement registry after a
    /// cross-process restore. Cursors produced by the legacy loop (which never
    /// reifies requirements) and requirement-free cursors return an empty list.
    #[must_use]
    pub fn pending_requirement_ids(&self) -> Vec<RequirementId> {
        match self {
            Self::StreamingStep(cursor) => cursor.requirement_id().into_iter().collect(),
            Self::AwaitingTool(cursor) => cursor
                .requirements()
                .map(|requirements| requirements.ids().values().copied().collect())
                .unwrap_or_default(),
            Self::AwaitingApproval(cursor) => cursor.requirement_id().into_iter().collect(),
            Self::AwaitingReconfig(cursor) => cursor.requirement_id().into_iter().collect(),
            Self::Idle | Self::CancelRecovery(_) | Self::Done(_) | Self::Error(_) => Vec::new(),
        }
    }

    /// Re-stamps the [`AgentPath`] origin of this cursor's requirement
    /// binding(s) to `base`, the absolute path of the machine that owns it.
    ///
    /// A single sans-io machine always stamps its bindings at the root; when
    /// that machine sits inside a nested tree at `base`, the owning node
    /// re-stamps the cursor so the persisted binding records the real path from
    /// the tree root (migration doc §7.1). Requirement-free cursors
    /// ([`Idle`](Self::Idle), [`CancelRecovery`](Self::CancelRecovery),
    /// [`Done`](Self::Done), [`Error`](Self::Error)) are left unchanged.
    pub(crate) fn rebase_origin(&mut self, base: &AgentPath) {
        match self {
            Self::StreamingStep(cursor) => {
                if let Some(requirement) = cursor.requirement.as_mut() {
                    requirement.origin = base.clone();
                }
            }
            Self::AwaitingApproval(cursor) => {
                if let Some(requirement) = cursor.requirement.as_mut() {
                    requirement.origin = base.clone();
                }
            }
            Self::AwaitingReconfig(cursor) => {
                if let Some(requirement) = cursor.requirement.as_mut() {
                    requirement.origin = base.clone();
                }
            }
            Self::AwaitingTool(cursor) => {
                if let Some(requirements) = cursor.requirements.as_mut() {
                    requirements.origin = base.clone();
                }
            }
            Self::Idle | Self::CancelRecovery(_) | Self::Done(_) | Self::Error(_) => {}
        }
    }

    pub(super) fn validate(&self) -> Result<(), AgentStateError> {
        match self {
            Self::Idle | Self::StreamingStep(_) | Self::AwaitingApproval(_) | Self::Done(_) => {
                Ok(())
            }
            Self::AwaitingTool(cursor) => cursor.validate(),
            Self::CancelRecovery(_) | Self::AwaitingReconfig(_) => Ok(()),
            Self::Error(cursor) => cursor.validate(),
        }
    }

    pub(super) fn can_transition_to(&self, next: &Self) -> bool {
        use LoopCursorKind::{
            AwaitingApproval, AwaitingReconfig, AwaitingTool, CancelRecovery, Done, Error, Idle,
            StreamingStep,
        };

        matches!(
            (self.kind(), next.kind()),
            (Idle, StreamingStep | AwaitingReconfig | Done | Error)
                | (
                    StreamingStep,
                    Idle | AwaitingTool
                        | AwaitingApproval
                        | AwaitingReconfig
                        | CancelRecovery
                        | Done
                        | Error
                )
                | (
                    AwaitingTool,
                    StreamingStep | AwaitingApproval | CancelRecovery | Done | Error
                )
                | (
                    AwaitingApproval,
                    AwaitingTool | CancelRecovery | Done | Error
                )
                | (
                    AwaitingReconfig,
                    StreamingStep | CancelRecovery | Done | Error
                )
                | (CancelRecovery, Idle | Done | Error)
                // A completed or errored segment is a terminal rest state. The
                // machine is reused across turns, so a fresh user turn resets it
                // back to the feedable `Idle` before opening the next turn.
                // `Error` is also reachable from here so `fail` can always park
                // a late runtime failure instead of dropping it (M4-4).
                | (Done | Error, Idle | Error)
        )
    }
}

/// Coarse loop cursor kind used in transition errors and diagnostics.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoopCursorKind {
    /// No feed segment is currently active.
    Idle,
    /// An LLM step is currently streaming or awaiting a full response.
    StreamingStep,
    /// The loop is waiting for tool execution to finish.
    AwaitingTool,
    /// The loop is waiting for external approval.
    AwaitingApproval,
    /// Cancellation recovery is closing pending state.
    CancelRecovery,
    /// A queued reconfiguration is waiting for the driver to resolve a registry.
    AwaitingReconfig,
    /// The feed segment completed.
    Done,
    /// The feed segment ended with an error.
    Error,
}

/// Cursor payload for an active LLM step.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StepCursor {
    step_id: StepId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    requirement: Option<CursorRequirement>,
}

impl StepCursor {
    /// Creates a step cursor from a caller-supplied step identity.
    ///
    /// `requirement` addresses the `NeedLlm` requirement the machine is stuck on;
    /// the legacy loop passes `None`.
    #[must_use]
    pub const fn new(step_id: StepId, requirement: Option<CursorRequirement>) -> Self {
        Self {
            step_id,
            requirement,
        }
    }

    /// Returns the step identity represented by this cursor.
    #[must_use]
    pub const fn step_id(&self) -> StepId {
        self.step_id
    }

    /// Returns the addressed `NeedLlm` requirement, if any.
    #[must_use]
    pub const fn requirement(&self) -> Option<&CursorRequirement> {
        self.requirement.as_ref()
    }

    /// Returns the identity of the awaited `NeedLlm` requirement, if any.
    #[must_use]
    pub fn requirement_id(&self) -> Option<RequirementId> {
        self.requirement.as_ref().map(CursorRequirement::id)
    }
}

/// Cursor payload for a queued reconfiguration awaiting a live registry.
///
/// A during-turn reconfiguration parks at the committing assistant `step_id`;
/// a start-of-turn reconfiguration has no step yet and carries `None`. The
/// `requirement` addresses the emitted `NeedReconfigRegistry` requirement the
/// driver must fulfill.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReconfigCursor {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    step_id: Option<StepId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    requirement: Option<CursorRequirement>,
}

impl ReconfigCursor {
    /// Creates a reconfig cursor from an optional committing step and requirement.
    #[must_use]
    pub const fn new(step_id: Option<StepId>, requirement: Option<CursorRequirement>) -> Self {
        Self {
            step_id,
            requirement,
        }
    }

    /// Returns the committing assistant step, if this is a during-turn reconfig.
    #[must_use]
    pub const fn step_id(&self) -> Option<StepId> {
        self.step_id
    }

    /// Returns the addressed `NeedReconfigRegistry` requirement, if any.
    #[must_use]
    pub const fn requirement(&self) -> Option<&CursorRequirement> {
        self.requirement.as_ref()
    }

    /// Returns the identity of the awaited registry requirement, if any.
    #[must_use]
    pub fn requirement_id(&self) -> Option<RequirementId> {
        self.requirement.as_ref().map(CursorRequirement::id)
    }
}

/// Cursor payload for one or more tool calls in flight.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolWaitCursor {
    step_id: StepId,
    tool_call_ids: Vec<ToolCallId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    requirements: Option<ToolWaitRequirements>,
}

impl ToolWaitCursor {
    /// Creates a tool-wait cursor from checked tool-call identities.
    ///
    /// When `requirements` is supplied, its keys must exactly match the tool-call
    /// set.
    ///
    /// # Errors
    ///
    /// Returns [`AgentStateError::EmptyToolWait`] for an empty list,
    /// [`AgentStateError::DuplicateToolCall`] when a call id repeats, or
    /// [`AgentStateError::ToolRequirementMismatch`] when a supplied binding does
    /// not cover the tool-call set exactly.
    pub fn new(
        step_id: StepId,
        tool_call_ids: Vec<ToolCallId>,
        requirements: Option<ToolWaitRequirements>,
    ) -> Result<Self, AgentStateError> {
        let cursor = Self {
            step_id,
            tool_call_ids,
            requirements,
        };
        cursor.validate()?;
        Ok(cursor)
    }

    /// Returns the step identity that opened these tool calls.
    #[must_use]
    pub const fn step_id(&self) -> StepId {
        self.step_id
    }

    /// Returns provider-independent tool-call identities still in flight.
    #[must_use]
    pub fn tool_call_ids(&self) -> &[ToolCallId] {
        &self.tool_call_ids
    }

    /// Returns the requirement addressing for the awaited tool calls, if any.
    #[must_use]
    pub const fn requirements(&self) -> Option<&ToolWaitRequirements> {
        self.requirements.as_ref()
    }

    fn validate(&self) -> Result<(), AgentStateError> {
        if self.tool_call_ids.is_empty() {
            return Err(AgentStateError::EmptyToolWait);
        }
        if let Some(call_id) = first_duplicate(&self.tool_call_ids) {
            return Err(AgentStateError::DuplicateToolCall { call_id });
        }
        if let Some(requirements) = &self.requirements {
            let expected: BTreeSet<ToolCallId> = self.tool_call_ids.iter().copied().collect();
            let bound: BTreeSet<ToolCallId> = requirements.ids().keys().copied().collect();
            if let Some(call_id) = expected.symmetric_difference(&bound).next() {
                return Err(AgentStateError::ToolRequirementMismatch { call_id: *call_id });
            }
        }
        Ok(())
    }
}

/// Cursor payload for a tool call waiting on external approval.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalCursor {
    step_id: StepId,
    tool_call_id: ToolCallId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    requirement: Option<CursorRequirement>,
}

impl ApprovalCursor {
    /// Creates an approval cursor for one tool call.
    ///
    /// `requirement` addresses the `NeedInteraction` requirement the machine is
    /// stuck on; the legacy loop passes `None`.
    #[must_use]
    pub const fn new(
        step_id: StepId,
        tool_call_id: ToolCallId,
        requirement: Option<CursorRequirement>,
    ) -> Self {
        Self {
            step_id,
            tool_call_id,
            requirement,
        }
    }

    /// Returns the step identity that requested approval.
    #[must_use]
    pub const fn step_id(&self) -> StepId {
        self.step_id
    }

    /// Returns the tool call waiting for approval.
    #[must_use]
    pub const fn tool_call_id(&self) -> ToolCallId {
        self.tool_call_id
    }

    /// Returns the addressed `NeedInteraction` requirement, if any.
    #[must_use]
    pub const fn requirement(&self) -> Option<&CursorRequirement> {
        self.requirement.as_ref()
    }

    /// Returns the identity of the awaited `NeedInteraction` requirement, if any.
    #[must_use]
    pub fn requirement_id(&self) -> Option<RequirementId> {
        self.requirement.as_ref().map(CursorRequirement::id)
    }
}

/// Cursor payload for cancellation recovery.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CancelRecoveryCursor {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    step_id: Option<StepId>,
    reason: CancelRecoveryReason,
}

impl CancelRecoveryCursor {
    /// Creates a cancellation-recovery cursor.
    #[must_use]
    pub const fn new(step_id: Option<StepId>, reason: CancelRecoveryReason) -> Self {
        Self { step_id, reason }
    }

    /// Returns the interrupted step identity when recovery is step-scoped.
    #[must_use]
    pub const fn step_id(&self) -> Option<StepId> {
        self.step_id
    }

    /// Returns why cancellation recovery was entered.
    #[must_use]
    pub const fn reason(&self) -> CancelRecoveryReason {
        self.reason
    }
}

/// Stable reason for entering cancellation recovery.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CancelRecoveryReason {
    /// A caller cancelled the run or feed segment.
    Cancelled,
    /// Budget enforcement interrupted the run.
    BudgetExceeded,
    /// Tool execution was interrupted and pending calls must be closed.
    ToolInterrupted,
    /// LLM streaming was interrupted and pending state must be closed.
    LlmInterrupted,
}

/// Cursor payload for a completed feed segment.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DoneCursor {
    reason: LoopDoneReason,
}

impl DoneCursor {
    /// Creates a done cursor with a stable reason.
    #[must_use]
    pub const fn new(reason: LoopDoneReason) -> Self {
        Self { reason }
    }

    /// Returns why the feed segment completed.
    #[must_use]
    pub const fn reason(&self) -> LoopDoneReason {
        self.reason
    }
}

/// Stable terminal reason for a completed feed segment.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoopDoneReason {
    /// The model reached a final assistant response and committed a step.
    Completed,
    /// The segment stopped because it reached a configured step limit.
    StepLimitReached,
    /// The segment stopped because cancellation was observed and closed.
    Cancelled,
    /// The segment stopped because budget was exhausted and recorded.
    BudgetExhausted,
}

/// Cursor payload for an errored feed segment.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ErrorCursor {
    message: String,
    #[serde(default, skip_serializing_if = "ErrorCursorKind::is_other")]
    kind: ErrorCursorKind,
}

impl ErrorCursor {
    /// Creates an error cursor from stable diagnostic text.
    ///
    /// # Errors
    ///
    /// Returns [`AgentStateError::EmptyCursorError`] when `message` is empty.
    pub fn new(message: impl Into<String>) -> Result<Self, AgentStateError> {
        Self::with_kind(message, ErrorCursorKind::Other)
    }

    /// Creates an error cursor from stable diagnostic text and kind.
    ///
    /// The kind is the machine-readable classification channel. The message is
    /// preserved for humans and must not be parsed for semantics.
    ///
    /// # Errors
    ///
    /// Returns [`AgentStateError::EmptyCursorError`] when `message` is empty.
    pub fn with_kind(
        message: impl Into<String>,
        kind: ErrorCursorKind,
    ) -> Result<Self, AgentStateError> {
        let cursor = Self {
            message: message.into(),
            kind,
        };
        cursor.validate()?;
        Ok(cursor)
    }

    /// Returns the stable diagnostic text.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Returns the stable machine-readable error kind.
    #[must_use]
    pub const fn kind(&self) -> ErrorCursorKind {
        self.kind
    }

    fn validate(&self) -> Result<(), AgentStateError> {
        if self.message.is_empty() {
            Err(AgentStateError::EmptyCursorError)
        } else {
            Ok(())
        }
    }
}

/// Stable machine-readable kind carried by an [`ErrorCursor`].
///
/// Human-readable messages can change without changing this value. Consumers
/// such as the facade should classify errors from this enum rather than by
/// matching substrings in [`ErrorCursor::message`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCursorKind {
    /// A general runtime failure with no more specific public classification.
    #[default]
    Other,
    /// The agent loop exhausted its configured per-turn step budget.
    LoopLimitExceeded,
}

impl ErrorCursorKind {
    const fn is_other(kind: &Self) -> bool {
        matches!(kind, Self::Other)
    }
}

fn first_duplicate<T>(items: &[T]) -> Option<T>
where
    T: Copy + Ord,
{
    let mut seen = BTreeSet::new();
    items.iter().copied().find(|item| !seen.insert(*item))
}
