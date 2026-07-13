//! Data-only Agent loop cursor types.

use crate::{agent::StepId, conversation::ToolCallId};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

use super::AgentStateError;

/// Data-only loop recovery cursor.
///
/// The cursor records where a future Agent loop can resume. It does not contain
/// a live stream, task handle, approval responder, or tool executor.
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
    /// The current feed segment reached a normal terminal outcome.
    Done(DoneCursor),
    /// The current feed segment ended with a classified runtime error.
    Error(ErrorCursor),
}

impl LoopCursor {
    /// Creates a streaming-step cursor.
    #[must_use]
    pub const fn streaming_step(step_id: StepId) -> Self {
        Self::StreamingStep(StepCursor::new(step_id))
    }

    /// Creates an awaiting-tool cursor after validating the call set.
    ///
    /// # Errors
    ///
    /// Returns [`AgentStateError::EmptyToolWait`] for an empty call set or
    /// [`AgentStateError::DuplicateToolCall`] for repeated tool-call ids.
    pub fn awaiting_tool(
        step_id: StepId,
        tool_call_ids: Vec<ToolCallId>,
    ) -> Result<Self, AgentStateError> {
        Ok(Self::AwaitingTool(ToolWaitCursor::new(
            step_id,
            tool_call_ids,
        )?))
    }

    /// Creates an awaiting-approval cursor.
    #[must_use]
    pub const fn awaiting_approval(step_id: StepId, tool_call_id: ToolCallId) -> Self {
        Self::AwaitingApproval(ApprovalCursor::new(step_id, tool_call_id))
    }

    /// Creates a cancellation-recovery cursor.
    #[must_use]
    pub const fn cancel_recovery(step_id: Option<StepId>, reason: CancelRecoveryReason) -> Self {
        Self::CancelRecovery(CancelRecoveryCursor::new(step_id, reason))
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

    /// Returns the coarse cursor kind used by transition validation.
    #[must_use]
    pub const fn kind(&self) -> LoopCursorKind {
        match self {
            Self::Idle => LoopCursorKind::Idle,
            Self::StreamingStep(_) => LoopCursorKind::StreamingStep,
            Self::AwaitingTool(_) => LoopCursorKind::AwaitingTool,
            Self::AwaitingApproval(_) => LoopCursorKind::AwaitingApproval,
            Self::CancelRecovery(_) => LoopCursorKind::CancelRecovery,
            Self::Done(_) => LoopCursorKind::Done,
            Self::Error(_) => LoopCursorKind::Error,
        }
    }

    pub(super) fn validate(&self) -> Result<(), AgentStateError> {
        match self {
            Self::Idle | Self::StreamingStep(_) | Self::AwaitingApproval(_) | Self::Done(_) => {
                Ok(())
            }
            Self::AwaitingTool(cursor) => cursor.validate(),
            Self::CancelRecovery(_) => Ok(()),
            Self::Error(cursor) => cursor.validate(),
        }
    }

    pub(super) fn can_transition_to(&self, next: &Self) -> bool {
        use LoopCursorKind::{
            AwaitingApproval, AwaitingTool, CancelRecovery, Done, Error, Idle, StreamingStep,
        };

        matches!(
            (self.kind(), next.kind()),
            (Idle, StreamingStep | Done | Error)
                | (
                    StreamingStep,
                    Idle | AwaitingTool | AwaitingApproval | CancelRecovery | Done | Error
                )
                | (
                    AwaitingTool,
                    StreamingStep | AwaitingApproval | CancelRecovery | Done | Error
                )
                | (
                    AwaitingApproval,
                    AwaitingTool | CancelRecovery | Done | Error
                )
                | (CancelRecovery, Idle | Done | Error)
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
}

impl StepCursor {
    /// Creates a step cursor from a caller-supplied step identity.
    #[must_use]
    pub const fn new(step_id: StepId) -> Self {
        Self { step_id }
    }

    /// Returns the step identity represented by this cursor.
    #[must_use]
    pub const fn step_id(&self) -> StepId {
        self.step_id
    }
}

/// Cursor payload for one or more tool calls in flight.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolWaitCursor {
    step_id: StepId,
    tool_call_ids: Vec<ToolCallId>,
}

impl ToolWaitCursor {
    /// Creates a tool-wait cursor from checked tool-call identities.
    ///
    /// # Errors
    ///
    /// Returns [`AgentStateError::EmptyToolWait`] for an empty list or
    /// [`AgentStateError::DuplicateToolCall`] when a call id repeats.
    pub fn new(step_id: StepId, tool_call_ids: Vec<ToolCallId>) -> Result<Self, AgentStateError> {
        let cursor = Self {
            step_id,
            tool_call_ids,
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

    fn validate(&self) -> Result<(), AgentStateError> {
        if self.tool_call_ids.is_empty() {
            return Err(AgentStateError::EmptyToolWait);
        }
        if let Some(call_id) = first_duplicate(&self.tool_call_ids) {
            return Err(AgentStateError::DuplicateToolCall { call_id });
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
}

impl ApprovalCursor {
    /// Creates an approval cursor for one tool call.
    #[must_use]
    pub const fn new(step_id: StepId, tool_call_id: ToolCallId) -> Self {
        Self {
            step_id,
            tool_call_id,
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
}

impl ErrorCursor {
    /// Creates an error cursor from stable diagnostic text.
    ///
    /// # Errors
    ///
    /// Returns [`AgentStateError::EmptyCursorError`] when `message` is empty.
    pub fn new(message: impl Into<String>) -> Result<Self, AgentStateError> {
        let cursor = Self {
            message: message.into(),
        };
        cursor.validate()?;
        Ok(cursor)
    }

    /// Returns the stable diagnostic text.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    fn validate(&self) -> Result<(), AgentStateError> {
        if self.message.is_empty() {
            Err(AgentStateError::EmptyCursorError)
        } else {
            Ok(())
        }
    }
}

fn first_duplicate<T>(items: &[T]) -> Option<T>
where
    T: Copy + Ord,
{
    let mut seen = BTreeSet::new();
    items.iter().copied().find(|item| !seen.insert(*item))
}
