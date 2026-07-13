//! Agent loop input, event, outcome, and error contracts.
//!
//! The types in this module are data boundaries for a future runtime loop.
//! They carry provider-neutral Client stream events, Conversation boundaries,
//! caller-supplied identities, and stable outcome classifications without
//! storing live streams, responders, clients, or tool registries.

use crate::{
    agent::{
        AgentStateError, ApprovalError, BudgetError, RunContextError, StepId, TraceNodeId,
        state::QueuedPivot, tool::ToolRuntimeError,
    },
    client::ClientError,
    conversation::{Boundary, ConversationError, MessageId, ToolCallId, TurnId},
    model::{
        message::{Message, Role},
        tool::{ToolCall, ToolResponse},
    },
    stream::StreamEvent,
};
use serde::{Deserialize, Deserializer, Serialize, de};
use serde_json::{Map, Value};
use thiserror::Error;

/// User-role pivot queued for a future step boundary.
///
/// This is the same checked data shape as [`QueuedPivot`]: a pivot is a
/// `Role::User` message plus source metadata, not a system prompt or reconfig
/// request.
pub type PivotMessage = QueuedPivot;

/// Input accepted by one Agent `feed` segment.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum AgentInput {
    /// Start a new user-authored turn.
    UserMessage(AgentUserInput),
    /// Start the next turn from the oldest queued pivot message.
    QueuedPivotTurn(QueuedPivotTurnInput),
    /// Resume from a data-only loop cursor after external recovery.
    Resume(ResumeInput),
}

impl AgentInput {
    /// Creates input for starting a user-authored turn.
    ///
    /// # Errors
    ///
    /// Returns [`AgentError::InvalidInputRole`] when `message` is not a
    /// `Role::User` payload.
    pub fn user_message(
        turn_id: TurnId,
        message_id: MessageId,
        message: Message,
        assistant_message_id: MessageId,
        step_id: StepId,
    ) -> Result<Self, AgentError> {
        Ok(Self::UserMessage(AgentUserInput::new(
            turn_id,
            message_id,
            message,
            assistant_message_id,
            step_id,
        )?))
    }

    /// Creates input for starting a turn from the oldest queued pivot.
    #[must_use]
    pub const fn queued_pivot_turn(
        turn_id: TurnId,
        assistant_message_id: MessageId,
        step_id: StepId,
    ) -> Self {
        Self::QueuedPivotTurn(QueuedPivotTurnInput::new(
            turn_id,
            assistant_message_id,
            step_id,
        ))
    }

    /// Creates input for resuming a feed segment after external recovery.
    #[must_use]
    pub const fn resume(step_id: StepId) -> Self {
        Self::Resume(ResumeInput::new(step_id))
    }
}

/// Complete user message and identities needed to begin a Conversation turn.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentUserInput {
    turn_id: TurnId,
    message_id: MessageId,
    message: Message,
    assistant_message_id: MessageId,
    step_id: StepId,
}

impl AgentUserInput {
    /// Creates checked user-turn input from caller-supplied identities.
    ///
    /// # Errors
    ///
    /// Returns [`AgentError::InvalidInputRole`] when `message.role` is not
    /// [`Role::User`].
    pub fn new(
        turn_id: TurnId,
        message_id: MessageId,
        message: Message,
        assistant_message_id: MessageId,
        step_id: StepId,
    ) -> Result<Self, AgentError> {
        if message.role != Role::User {
            return Err(AgentError::InvalidInputRole(message.role));
        }

        Ok(Self {
            turn_id,
            message_id,
            message,
            assistant_message_id,
            step_id,
        })
    }

    /// Returns the caller-supplied turn identity.
    #[must_use]
    pub const fn turn_id(&self) -> TurnId {
        self.turn_id
    }

    /// Returns the caller-supplied user message identity.
    #[must_use]
    pub const fn message_id(&self) -> MessageId {
        self.message_id
    }

    /// Returns the caller-supplied assistant message identity for this step.
    #[must_use]
    pub const fn assistant_message_id(&self) -> MessageId {
        self.assistant_message_id
    }

    /// Returns the complete user message payload.
    #[must_use]
    pub const fn message(&self) -> &Message {
        &self.message
    }

    /// Returns the first Agent step identity for this feed segment.
    #[must_use]
    pub const fn step_id(&self) -> StepId {
        self.step_id
    }
}

impl<'de> Deserialize<'de> for AgentUserInput {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Record {
            turn_id: TurnId,
            message_id: MessageId,
            message: Message,
            assistant_message_id: MessageId,
            step_id: StepId,
        }

        let record = Record::deserialize(deserializer)?;
        Self::new(
            record.turn_id,
            record.message_id,
            record.message,
            record.assistant_message_id,
            record.step_id,
        )
        .map_err(de::Error::custom)
    }
}

/// Identities needed to turn the oldest queued pivot into a new turn.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueuedPivotTurnInput {
    turn_id: TurnId,
    assistant_message_id: MessageId,
    step_id: StepId,
}

impl QueuedPivotTurnInput {
    /// Creates queued-pivot turn input from caller-supplied identities.
    ///
    /// The pivot already carries the user message id and payload. The caller
    /// still supplies the turn, assistant response, and step identities because
    /// Agent code must not synthesize ids.
    #[must_use]
    pub const fn new(turn_id: TurnId, assistant_message_id: MessageId, step_id: StepId) -> Self {
        Self {
            turn_id,
            assistant_message_id,
            step_id,
        }
    }

    /// Returns the caller-supplied turn identity.
    #[must_use]
    pub const fn turn_id(&self) -> TurnId {
        self.turn_id
    }

    /// Returns the caller-supplied assistant message identity for this step.
    #[must_use]
    pub const fn assistant_message_id(&self) -> MessageId {
        self.assistant_message_id
    }

    /// Returns the Agent step identity for this queued-pivot turn.
    #[must_use]
    pub const fn step_id(&self) -> StepId {
        self.step_id
    }
}

/// Input for resuming after a pause, approval, tool wait, or recovery point.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResumeInput {
    step_id: StepId,
}

impl ResumeInput {
    /// Creates resume input with the next caller-supplied step identity.
    #[must_use]
    pub const fn new(step_id: StepId) -> Self {
        Self { step_id }
    }

    /// Returns the next Agent step identity.
    #[must_use]
    pub const fn step_id(&self) -> StepId {
        self.step_id
    }
}

/// Event emitted by an Agent loop while one feed segment is active.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum AgentEvent {
    /// Provider-neutral LLM stream event, carried without Agent-side rewriting.
    Llm(StreamEvent),
    /// Agent step boundary where cross-cutting policies can be evaluated.
    StepBoundary(StepBoundary),
    /// Tool execution has started for a mapped tool call.
    ToolCallStarted(ToolCallStarted),
    /// Tool execution has finished and produced a complete response.
    ToolCallFinished(ToolCallFinished),
    /// The loop is waiting for external approval before executing a tool.
    AwaitingApproval(ApprovalRequest),
    /// The feed segment has ended with a classified outcome.
    Done(AgentOutcome),
}

/// Payload emitted at an Agent step boundary.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StepBoundary {
    step_id: StepId,
    boundary: Boundary,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    trace_node_id: Option<TraceNodeId>,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    metadata: Map<String, Value>,
}

impl StepBoundary {
    /// Creates a step-boundary payload without extra metadata.
    #[must_use]
    pub fn new(step_id: StepId, boundary: Boundary, trace_node_id: Option<TraceNodeId>) -> Self {
        Self::with_metadata(step_id, boundary, trace_node_id, Map::new())
    }

    /// Creates a step-boundary payload with caller-supplied metadata.
    #[must_use]
    pub const fn with_metadata(
        step_id: StepId,
        boundary: Boundary,
        trace_node_id: Option<TraceNodeId>,
        metadata: Map<String, Value>,
    ) -> Self {
        Self {
            step_id,
            boundary,
            trace_node_id,
            metadata,
        }
    }

    /// Returns the Agent step identity for this boundary.
    #[must_use]
    pub const fn step_id(&self) -> StepId {
        self.step_id
    }

    /// Returns the Conversation-issued boundary token.
    #[must_use]
    pub const fn boundary(&self) -> Boundary {
        self.boundary
    }

    /// Returns the trace node associated with this step, if one was recorded.
    #[must_use]
    pub const fn trace_node_id(&self) -> Option<&TraceNodeId> {
        self.trace_node_id.as_ref()
    }

    /// Returns step-boundary metadata supplied by the loop.
    #[must_use]
    pub const fn metadata(&self) -> &Map<String, Value> {
        &self.metadata
    }
}

/// Tool-start payload emitted by an Agent loop.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolCallStarted {
    step_id: StepId,
    call_id: ToolCallId,
    call: ToolCall,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    trace_node_id: Option<TraceNodeId>,
}

impl ToolCallStarted {
    /// Creates a tool-start event payload.
    #[must_use]
    pub fn new(
        step_id: StepId,
        call_id: ToolCallId,
        call: ToolCall,
        trace_node_id: Option<TraceNodeId>,
    ) -> Self {
        Self {
            step_id,
            call_id,
            call,
            trace_node_id,
        }
    }

    /// Returns the step that opened this tool call.
    #[must_use]
    pub const fn step_id(&self) -> StepId {
        self.step_id
    }

    /// Returns the framework-level tool-call identity.
    #[must_use]
    pub const fn call_id(&self) -> ToolCallId {
        self.call_id
    }

    /// Returns the provider-neutral complete tool call.
    #[must_use]
    pub const fn call(&self) -> &ToolCall {
        &self.call
    }

    /// Returns the trace node associated with this tool call, if any.
    #[must_use]
    pub const fn trace_node_id(&self) -> Option<&TraceNodeId> {
        self.trace_node_id.as_ref()
    }
}

/// Tool-finish payload emitted by an Agent loop.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolCallFinished {
    step_id: StepId,
    call_id: ToolCallId,
    response: ToolResponse,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    trace_node_id: Option<TraceNodeId>,
}

impl ToolCallFinished {
    /// Creates a tool-finish event payload.
    #[must_use]
    pub fn new(
        step_id: StepId,
        call_id: ToolCallId,
        response: ToolResponse,
        trace_node_id: Option<TraceNodeId>,
    ) -> Self {
        Self {
            step_id,
            call_id,
            response,
            trace_node_id,
        }
    }

    /// Returns the step that completed this tool call.
    #[must_use]
    pub const fn step_id(&self) -> StepId {
        self.step_id
    }

    /// Returns the framework-level tool-call identity.
    #[must_use]
    pub const fn call_id(&self) -> ToolCallId {
        self.call_id
    }

    /// Returns the complete tool response.
    #[must_use]
    pub const fn response(&self) -> &ToolResponse {
        &self.response
    }

    /// Returns the trace node associated with this tool call, if any.
    #[must_use]
    pub const fn trace_node_id(&self) -> Option<&TraceNodeId> {
        self.trace_node_id.as_ref()
    }
}

/// Data emitted when a tool call is waiting for external approval.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalRequest {
    step_id: StepId,
    call_id: ToolCallId,
    call: ToolCall,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    trace_node_id: Option<TraceNodeId>,
}

impl ApprovalRequest {
    /// Creates an approval-wait payload.
    #[must_use]
    pub fn new(
        step_id: StepId,
        call_id: ToolCallId,
        call: ToolCall,
        trace_node_id: Option<TraceNodeId>,
    ) -> Self {
        Self::with_reason(step_id, call_id, call, None, trace_node_id)
    }

    /// Creates an approval-wait payload with stable reason text.
    #[must_use]
    pub fn with_reason(
        step_id: StepId,
        call_id: ToolCallId,
        call: ToolCall,
        reason: Option<String>,
        trace_node_id: Option<TraceNodeId>,
    ) -> Self {
        Self {
            step_id,
            call_id,
            call,
            reason: reason.and_then(non_empty),
            trace_node_id,
        }
    }

    /// Returns the step that requested approval.
    #[must_use]
    pub const fn step_id(&self) -> StepId {
        self.step_id
    }

    /// Returns the framework-level tool-call identity awaiting approval.
    #[must_use]
    pub const fn call_id(&self) -> ToolCallId {
        self.call_id
    }

    /// Returns the provider-neutral complete tool call awaiting approval.
    #[must_use]
    pub const fn call(&self) -> &ToolCall {
        &self.call
    }

    /// Returns stable approval reason text, if one was supplied.
    #[must_use]
    pub fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }

    /// Returns the trace node associated with this approval wait, if any.
    #[must_use]
    pub const fn trace_node_id(&self) -> Option<&TraceNodeId> {
        self.trace_node_id.as_ref()
    }
}

/// Coarse terminal outcome category for a feed segment.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentOutcomeKind {
    /// The segment reached a normal final assistant response.
    Completed,
    /// The segment stopped because budget was exhausted.
    BudgetExhausted,
    /// The segment stopped because cancellation was observed and closed.
    Cancelled,
    /// The segment stopped with a classified runtime error.
    Error,
    /// The segment yielded until an external actor resumes it.
    WaitingForExternalRecovery,
}

/// Classified terminal outcome for one feed segment.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", content = "data", rename_all = "snake_case")]
pub enum AgentOutcome {
    /// The segment reached a normal final assistant response.
    Completed,
    /// The segment stopped because a budget limit was exceeded.
    BudgetExhausted(BudgetExhaustedOutcome),
    /// The segment stopped because cancellation was observed and closed.
    Cancelled,
    /// The segment stopped with a classified runtime error.
    Error(AgentFailure),
    /// The segment yielded until a host, approver, or tool executor resumes it.
    WaitingForExternalRecovery(ExternalRecoveryOutcome),
}

impl AgentOutcome {
    /// Creates a budget-exhausted outcome from a classified budget error.
    #[must_use]
    pub const fn budget_exhausted(error: BudgetError) -> Self {
        Self::BudgetExhausted(BudgetExhaustedOutcome::new(error))
    }

    /// Creates an error outcome from a classified Agent error.
    #[must_use]
    pub fn error(error: &AgentError) -> Self {
        Self::Error(AgentFailure::from(error))
    }

    /// Creates an external-recovery outcome.
    #[must_use]
    pub fn waiting_for_external_recovery(
        kind: ExternalRecoveryKind,
        message: Option<String>,
    ) -> Self {
        Self::WaitingForExternalRecovery(ExternalRecoveryOutcome::new(kind, message))
    }

    /// Returns the coarse terminal outcome category.
    #[must_use]
    pub const fn kind(&self) -> AgentOutcomeKind {
        match self {
            Self::Completed => AgentOutcomeKind::Completed,
            Self::BudgetExhausted(_) => AgentOutcomeKind::BudgetExhausted,
            Self::Cancelled => AgentOutcomeKind::Cancelled,
            Self::Error(_) => AgentOutcomeKind::Error,
            Self::WaitingForExternalRecovery(_) => AgentOutcomeKind::WaitingForExternalRecovery,
        }
    }
}

/// Budget-exhausted outcome payload.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BudgetExhaustedOutcome {
    error: BudgetError,
}

impl BudgetExhaustedOutcome {
    /// Creates a budget-exhausted payload.
    #[must_use]
    pub const fn new(error: BudgetError) -> Self {
        Self { error }
    }

    /// Returns the budget error that ended the segment.
    #[must_use]
    pub const fn error(&self) -> &BudgetError {
        &self.error
    }
}

/// External recovery category for a yielded feed segment.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalRecoveryKind {
    /// A tool call is waiting for approval.
    AwaitingApproval,
    /// One or more tool calls are waiting for host-side execution results.
    AwaitingToolResults,
    /// The host intentionally paused the loop at a recovery point.
    Paused,
    /// Cancellation recovery requires a later resume operation.
    CancelRecovery,
}

/// External-recovery outcome payload.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalRecoveryOutcome {
    kind: ExternalRecoveryKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    message: Option<String>,
}

impl ExternalRecoveryOutcome {
    /// Creates an external-recovery payload.
    #[must_use]
    pub fn new(kind: ExternalRecoveryKind, message: Option<String>) -> Self {
        Self {
            kind,
            message: message.and_then(non_empty),
        }
    }

    /// Returns the external-recovery category.
    #[must_use]
    pub const fn kind(&self) -> ExternalRecoveryKind {
        self.kind
    }

    /// Returns stable diagnostic text, if one was supplied.
    #[must_use]
    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }
}

/// Stable error category usable in data-only outcomes and diagnostics.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentErrorKind {
    /// A feed call was attempted while a prior feed stream was still active.
    FeedInProgress,
    /// Input failed Agent-level validation before reaching Conversation.
    InvalidInput,
    /// A Client operation failed.
    Client,
    /// A Conversation operation failed.
    Conversation,
    /// A budget limit was exceeded.
    Budget,
    /// Cancellation was observed.
    Cancelled,
    /// Trace recording failed.
    Trace,
    /// Agent state or cursor validation failed.
    AgentState,
    /// Tool registry, tool execution, or tool identity injection failed.
    Tool,
    /// Tool approval policy or responder handling failed.
    Approval,
    /// The failure did not fit a more specific category.
    Other,
}

/// Data-only description of an Agent failure.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentFailure {
    kind: AgentErrorKind,
    message: String,
}

impl AgentFailure {
    /// Creates failure data from a stable category and diagnostic message.
    #[must_use]
    pub fn new(kind: AgentErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    /// Returns the stable error category.
    #[must_use]
    pub const fn kind(&self) -> AgentErrorKind {
        self.kind
    }

    /// Returns the diagnostic message.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl From<&AgentError> for AgentFailure {
    fn from(error: &AgentError) -> Self {
        Self::new(error.kind(), error.to_string())
    }
}

/// Classified Agent loop failure.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum AgentError {
    /// A feed stream is already active for this Agent.
    #[error("another Agent feed stream is still active")]
    FeedInProgress,
    /// User-turn input carried a role other than `Role::User`.
    #[error("agent user input must use Role::User, found {0:?}")]
    InvalidInputRole(Role),
    /// A queued pivot must be consumed before accepting another user turn.
    #[error("queued pivot messages must be consumed before a new user turn")]
    QueuedPivotPending,
    /// Queued-pivot turn input was supplied while the pivot queue was empty.
    #[error("queued-pivot turn input requires at least one queued pivot")]
    NoQueuedPivot,
    /// The underlying LLM client failed.
    #[error("client operation failed: {0}")]
    Client(#[from] ClientError),
    /// A Conversation transition failed.
    #[error("conversation operation failed: {0}")]
    Conversation(#[from] ConversationError),
    /// Run context cancellation, budget, or trace handling failed.
    #[error("run context operation failed: {0}")]
    RunContext(#[from] RunContextError),
    /// Agent state validation failed.
    #[error("agent state operation failed: {0}")]
    State(#[from] AgentStateError),
    /// Tool runtime operation failed.
    #[error("tool runtime operation failed: {0}")]
    Tool(#[from] ToolRuntimeError),
    /// Approval runtime operation failed.
    #[error("approval operation failed: {0}")]
    Approval(#[from] ApprovalError),
    /// A loop implementation returned an uncategorized failure.
    #[error("agent runtime error: {0}")]
    Other(String),
}

impl AgentError {
    /// Returns the stable category for this error.
    #[must_use]
    pub const fn kind(&self) -> AgentErrorKind {
        match self {
            Self::FeedInProgress => AgentErrorKind::FeedInProgress,
            Self::InvalidInputRole(_) | Self::QueuedPivotPending | Self::NoQueuedPivot => {
                AgentErrorKind::InvalidInput
            }
            Self::Client(_) => AgentErrorKind::Client,
            Self::Conversation(_) => AgentErrorKind::Conversation,
            Self::RunContext(RunContextError::Cancelled) => AgentErrorKind::Cancelled,
            Self::RunContext(RunContextError::Budget(_)) => AgentErrorKind::Budget,
            Self::RunContext(RunContextError::Trace(_)) => AgentErrorKind::Trace,
            Self::State(_) => AgentErrorKind::AgentState,
            Self::Tool(_) => AgentErrorKind::Tool,
            Self::Approval(_) => AgentErrorKind::Approval,
            Self::Other(_) => AgentErrorKind::Other,
        }
    }
}

fn non_empty(value: String) -> Option<String> {
    if value.is_empty() { None } else { Some(value) }
}

#[cfg(test)]
mod tests {
    use super::{
        AgentError, AgentErrorKind, AgentEvent, AgentInput, AgentOutcome, AgentOutcomeKind,
        ApprovalRequest, ExternalRecoveryKind, QueuedPivotTurnInput, StepBoundary,
        ToolCallFinished, ToolCallStarted,
    };
    use crate::{
        agent::{BudgetDimension, BudgetError, StepId, TraceNodeId},
        conversation::{
            Conversation, ConversationConfig, ConversationId, MessageId, ToolCallId, TurnId,
        },
        model::{
            content::ContentBlock,
            message::{Message, Role},
            normalized::StopReason,
            tool::{ToolCall, ToolResponse, ToolStatus},
        },
        stream::{BlockId, Delta, StreamEvent},
    };
    use serde::{Serialize, de::DeserializeOwned};
    use serde_json::{Map, Value, json};
    use std::fmt::Debug;

    fn assert_json_round_trip<T>(value: T)
    where
        T: Debug + PartialEq + Serialize + DeserializeOwned,
    {
        let encoded = serde_json::to_value(&value).expect("serialize value");
        let decoded: T = serde_json::from_value(encoded).expect("deserialize value");

        assert_eq!(decoded, value);
    }

    fn conversation_id() -> ConversationId {
        "018f0d9c-7b6a-7c12-8f31-1234567890f1"
            .parse()
            .expect("conversation id")
    }

    fn turn_id() -> TurnId {
        "018f0d9c-7b6a-7c12-8f31-1234567890f2"
            .parse()
            .expect("turn id")
    }

    fn message_id() -> MessageId {
        "018f0d9c-7b6a-7c12-8f31-1234567890f3"
            .parse()
            .expect("message id")
    }

    fn assistant_message_id() -> MessageId {
        "018f0d9c-7b6a-7c12-8f31-1234567890f6"
            .parse()
            .expect("assistant message id")
    }

    fn step_id() -> StepId {
        "018f0d9c-7b6a-7c12-8f31-1234567890f4"
            .parse()
            .expect("step id")
    }

    fn tool_call_id() -> ToolCallId {
        "018f0d9c-7b6a-7c12-8f31-1234567890f5"
            .parse()
            .expect("tool call id")
    }

    fn user_message(text: &str) -> Message {
        Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: text.to_owned(),
                extra: Map::new(),
            }],
        }
    }

    fn tool_call() -> ToolCall {
        ToolCall {
            id: "provider-call-1".to_owned(),
            name: "get_weather".to_owned(),
            input: json!({ "city": "Shanghai" }),
        }
    }

    fn tool_response() -> ToolResponse {
        ToolResponse {
            tool_call_id: "provider-call-1".to_owned(),
            content: vec![ContentBlock::Text {
                text: "Sunny".to_owned(),
                extra: Map::new(),
            }],
            status: ToolStatus::Ok,
            extra: Map::new(),
        }
    }

    fn zero_boundary() -> crate::conversation::Boundary {
        Conversation::new(conversation_id(), ConversationConfig::default()).head()
    }

    #[test]
    fn agent_input_rejects_non_user_turn_payloads() {
        let error = AgentInput::user_message(
            turn_id(),
            message_id(),
            Message {
                role: Role::Assistant,
                content: Vec::new(),
            },
            assistant_message_id(),
            step_id(),
        )
        .expect_err("assistant input must not start a user turn");

        assert_eq!(error, AgentError::InvalidInputRole(Role::Assistant));

        let encoded = json!({
            "type": "user_message",
            "data": {
                "turn_id": turn_id(),
                "message_id": message_id(),
                "message": { "role": "system", "content": [] },
                "assistant_message_id": assistant_message_id(),
                "step_id": step_id()
            }
        });
        let serde_error = serde_json::from_value::<AgentInput>(encoded)
            .expect_err("serde must revalidate the user role");
        assert!(serde_error.to_string().contains("Role::User"));

        assert_json_round_trip(AgentInput::QueuedPivotTurn(QueuedPivotTurnInput::new(
            turn_id(),
            assistant_message_id(),
            step_id(),
        )));
    }

    #[test]
    fn agent_events_round_trip_as_data_shapes() {
        let mut metadata = Map::new();
        metadata.insert("budget_checked".to_owned(), Value::Bool(true));
        let boundary = StepBoundary::with_metadata(
            step_id(),
            zero_boundary(),
            Some(TraceNodeId::new("step-trace")),
            metadata,
        );
        let call = tool_call();

        let events = [
            AgentEvent::StepBoundary(boundary),
            AgentEvent::ToolCallStarted(ToolCallStarted::new(
                step_id(),
                tool_call_id(),
                call.clone(),
                Some(TraceNodeId::new("tool-trace")),
            )),
            AgentEvent::AwaitingApproval(ApprovalRequest::new(
                step_id(),
                tool_call_id(),
                call,
                None,
            )),
            AgentEvent::ToolCallFinished(ToolCallFinished::new(
                step_id(),
                tool_call_id(),
                tool_response(),
                None,
            )),
            AgentEvent::Done(AgentOutcome::Completed),
            AgentEvent::Done(AgentOutcome::waiting_for_external_recovery(
                ExternalRecoveryKind::AwaitingApproval,
                Some("waiting for human approval".to_owned()),
            )),
        ];

        for event in events {
            assert_json_round_trip(event);
        }
    }

    #[test]
    fn llm_event_payload_is_transparent_stream_event_data() {
        let llm = StreamEvent::BlockDelta {
            id: BlockId::new("text-1"),
            delta: Delta::Text("hello".to_owned()),
        };
        let event = AgentEvent::Llm(llm.clone());

        let encoded = serde_json::to_value(&event).expect("serialize agent event");
        assert_eq!(encoded["type"], json!("llm"));
        assert_eq!(
            encoded["data"],
            serde_json::to_value(&llm).expect("serialize stream event")
        );

        let decoded: AgentEvent = serde_json::from_value(encoded).expect("decode agent event");
        assert_eq!(decoded, event);
    }

    #[test]
    fn done_outcomes_keep_distinct_terminal_classifications() {
        let budget = BudgetError::Exceeded {
            dimension: BudgetDimension::Steps,
            limit: 2,
            attempted: 3,
            remaining: 0,
        };
        let outcomes = [
            (AgentOutcome::Completed, AgentOutcomeKind::Completed),
            (
                AgentOutcome::budget_exhausted(budget),
                AgentOutcomeKind::BudgetExhausted,
            ),
            (AgentOutcome::Cancelled, AgentOutcomeKind::Cancelled),
            (
                AgentOutcome::error(&AgentError::Client(crate::client::ClientError::Timeout)),
                AgentOutcomeKind::Error,
            ),
            (
                AgentOutcome::waiting_for_external_recovery(
                    ExternalRecoveryKind::AwaitingToolResults,
                    None,
                ),
                AgentOutcomeKind::WaitingForExternalRecovery,
            ),
        ];

        for (outcome, expected_kind) in outcomes {
            assert_eq!(outcome.kind(), expected_kind);
            assert_json_round_trip(AgentEvent::Done(outcome));
        }
    }

    #[test]
    fn agent_error_kind_preserves_budget_cancel_and_trace_categories() {
        assert_eq!(
            AgentError::InvalidInputRole(Role::Tool).kind(),
            AgentErrorKind::InvalidInput
        );
        assert_eq!(
            AgentError::RunContext(crate::agent::RunContextError::Cancelled).kind(),
            AgentErrorKind::Cancelled
        );
        assert_eq!(
            AgentError::RunContext(crate::agent::RunContextError::Budget(
                BudgetError::Exceeded {
                    dimension: BudgetDimension::Tokens,
                    limit: 5,
                    attempted: 6,
                    remaining: 0,
                }
            ))
            .kind(),
            AgentErrorKind::Budget
        );
    }

    #[test]
    fn agent_input_round_trips_for_checked_user_and_resume_shapes() {
        assert_json_round_trip(
            AgentInput::user_message(
                turn_id(),
                message_id(),
                user_message("hello"),
                assistant_message_id(),
                step_id(),
            )
            .expect("valid user input"),
        );
        assert_json_round_trip(AgentInput::resume(step_id()));
    }

    #[test]
    fn llm_done_event_accepts_stream_stop_reason() {
        let event = AgentEvent::Llm(StreamEvent::MessageStop {
            stop_reason: StopReason::normalize("end_turn"),
        });

        assert_json_round_trip(event);
    }
}
