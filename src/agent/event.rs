//! Agent loop input, event, outcome, and error contracts.
//!
//! The types in this module are data boundaries for the sans-io Agent machine.
//! They carry provider-neutral Client stream events, Conversation boundaries,
//! caller-supplied identities, and stable outcome classifications without
//! storing live streams, responders, clients, or tool registries.
//!
//! [`Notification`] is the Agent-effect-model *notification* subset: the
//! skippable, observe-only facts a machine reports while it advances. The old
//! combined `AgentEvent` push stream (with its `AwaitingApproval` request and
//! `Done` terminal variants) has been removed; requests are now
//! [`Requirement`](crate::agent::Requirement)s resolved on the return path, and
//! turn completion is expressed by a quiescent
//! [`StepOutcome`](crate::agent::StepOutcome) instead of a stream event.

use crate::{
    agent::{
        AgentStateError, ApprovalError, RunContextError, StepId, TraceNodeId,
        requirement::{AgentPath, RequirementKindTag},
        state::QueuedPivot,
        tool::ToolRuntimeError,
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

/// External input accepted by one Agent machine step.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum AgentInput {
    /// Start a new user-authored turn.
    UserMessage(AgentUserInput),
    /// A `Role::User` pivot injected directly between two steps.
    ///
    /// This is the effect-model replacement for the pivot queue: a driver soft-
    /// turns the machine by feeding a pivot at a step boundary instead of
    /// queueing it (see the migration doc §2.2). Queueing policy — when to
    /// inject — moves to the driver / session.
    Pivot(PivotMessage),
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

    /// Creates input for injecting a `Role::User` pivot between two steps.
    #[must_use]
    pub const fn pivot(pivot: PivotMessage) -> Self {
        Self::Pivot(pivot)
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

/// Pure notification emitted by an Agent machine that a `drain` may skip.
///
/// A [`Notification`] is the observe-only subset of the events a machine
/// reports: every variant here carries a fact the machine wants to surface,
/// never a request the machine is blocked on. A consumer that only advances the
/// machine may therefore drop notifications without stalling progress. This is
/// the Agent-effect-model split of the old combined event stream into
/// *notifications* (skippable) and
/// [*requirements*](crate::agent::Requirement) (must be resolved); see the
/// Agent-effect migration doc §3.1.
///
/// The two kinds of events deliberately excluded here are requests or terminal
/// states, not notifications, and map to the new model as follows:
///
/// - Approval waits are requests the machine blocks on; each becomes a
///   `Requirement::NeedInteraction` (generalized approval, §4) resolved through
///   the requirement return path, not observed as a notification.
/// - Turn completion is no longer a stream event; it is expressed by a quiescent
///   step outcome (`StepOutcome.quiescent == true`) with an empty requirement
///   set and the loop cursor reaching `Done`/`Error` (§3.1/§5).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum Notification {
    /// Provider-neutral LLM stream event, carried without Agent-side rewriting.
    Llm(StreamEvent),
    /// Agent step boundary where cross-cutting policies can be evaluated.
    StepBoundary(StepBoundary),
    /// Tool execution has started for a mapped tool call.
    ToolCallStarted(ToolCallStarted),
    /// Tool execution has finished and produced a complete response.
    ToolCallFinished(ToolCallFinished),
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

/// Stable error category usable in data-only outcomes and diagnostics.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentErrorKind {
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
    /// A requirement reached the top scope with no handler to fulfill it.
    UnhandledRequirement,
    /// A subagent orchestration guard failed (for example the maximum
    /// hierarchy depth was exceeded).
    Subagent,
    /// The failure did not fit a more specific category.
    Other,
}

/// Classified Agent loop failure.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum AgentError {
    /// User-turn input carried a role other than `Role::User`.
    #[error("agent user input must use Role::User, found {0:?}")]
    InvalidInputRole(Role),
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
    /// A requirement popped past the top scope with no handler to fulfill it.
    ///
    /// The scope chain must be *total* at its root: a headless driver that omits
    /// (for example) an interaction handler turns any deep interaction request
    /// into this classified error at start-up rather than a silent hang.
    #[error("no handler for `{kind}` requirement at the top scope (origin: {origin:?})")]
    UnhandledRequirement {
        /// Requirement family that reached the top scope unhandled.
        kind: RequirementKindTag,
        /// Path of the machine that emitted the requirement.
        origin: AgentPath,
    },
    /// A machine or driver returned an uncategorized failure.
    #[error("agent runtime error: {0}")]
    Other(String),
    /// A subagent would deepen the scope chain past the configured limit.
    ///
    /// The only scope-deepening handler (`NeedSubagent`) enforces a maximum
    /// hierarchy depth so a coordinator that can spawn subagents cannot recurse
    /// without bound (migration doc §7.2 / `agent-layer.md` §6.3).
    #[error("subagent depth limit {limit} exceeded (context depth {depth})")]
    SubagentDepthExceeded {
        /// Maximum nesting depth the subagent handler allows.
        limit: u32,
        /// Depth of the context that requested a further child.
        depth: u32,
    },
}

impl AgentError {
    /// Returns the stable category for this error.
    #[must_use]
    pub const fn kind(&self) -> AgentErrorKind {
        match self {
            Self::InvalidInputRole(_) => AgentErrorKind::InvalidInput,
            Self::Client(_) => AgentErrorKind::Client,
            Self::Conversation(_) => AgentErrorKind::Conversation,
            Self::RunContext(RunContextError::Cancelled) => AgentErrorKind::Cancelled,
            Self::RunContext(RunContextError::Budget(_)) => AgentErrorKind::Budget,
            Self::RunContext(RunContextError::Trace(_)) => AgentErrorKind::Trace,
            Self::State(_) => AgentErrorKind::AgentState,
            Self::Tool(_) => AgentErrorKind::Tool,
            Self::Approval(_) => AgentErrorKind::Approval,
            Self::UnhandledRequirement { .. } => AgentErrorKind::UnhandledRequirement,
            Self::SubagentDepthExceeded { .. } => AgentErrorKind::Subagent,
            Self::Other(_) => AgentErrorKind::Other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AgentError, AgentErrorKind, AgentInput, Notification, PivotMessage, StepBoundary,
        ToolCallFinished, ToolCallStarted,
    };
    use crate::{
        agent::{BudgetDimension, BudgetError, PivotSource, StepId, TraceNodeId},
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
    }

    #[test]
    fn notification_payloads_round_trip_as_data_shapes() {
        let mut metadata = Map::new();
        metadata.insert("budget_checked".to_owned(), Value::Bool(true));
        let boundary = StepBoundary::with_metadata(
            step_id(),
            zero_boundary(),
            Some(TraceNodeId::new("step-trace")),
            metadata,
        );

        let notifications = [
            Notification::StepBoundary(boundary),
            Notification::ToolCallStarted(ToolCallStarted::new(
                step_id(),
                tool_call_id(),
                tool_call(),
                Some(TraceNodeId::new("tool-trace")),
            )),
            Notification::ToolCallFinished(ToolCallFinished::new(
                step_id(),
                tool_call_id(),
                tool_response(),
                None,
            )),
        ];

        for notification in notifications {
            assert_json_round_trip(notification);
        }
    }

    #[test]
    fn llm_notification_payload_is_transparent_stream_event_data() {
        let llm = StreamEvent::BlockDelta {
            id: BlockId::new("text-1"),
            delta: Delta::Text("hello".to_owned()),
        };
        let notification = Notification::Llm(llm.clone());

        let encoded = serde_json::to_value(&notification).expect("serialize notification");
        assert_eq!(encoded["type"], json!("llm"));
        assert_eq!(
            encoded["data"],
            serde_json::to_value(&llm).expect("serialize stream event")
        );

        let decoded: Notification = serde_json::from_value(encoded).expect("decode notification");
        assert_eq!(decoded, notification);
    }

    #[test]
    fn notifications_round_trip_and_keep_wire_shape() {
        let boundary = StepBoundary::new(
            step_id(),
            zero_boundary(),
            Some(TraceNodeId::new("step-trace")),
        );
        let started = ToolCallStarted::new(step_id(), tool_call_id(), tool_call(), None);
        let finished = ToolCallFinished::new(step_id(), tool_call_id(), tool_response(), None);
        let llm = StreamEvent::BlockDelta {
            id: BlockId::new("text-1"),
            delta: Delta::Text("hello".to_owned()),
        };

        let notifications = [
            Notification::Llm(llm),
            Notification::StepBoundary(boundary),
            Notification::ToolCallStarted(started),
            Notification::ToolCallFinished(finished),
        ];

        for notification in notifications {
            assert_json_round_trip(notification.clone());

            // Every notification serializes as a `{ "type", "data" }` tagged
            // record, the wire shape the driver forwards downstream.
            let encoded = serde_json::to_value(&notification).expect("serialize notification");
            assert!(encoded.get("type").is_some());
        }
    }

    #[test]
    fn notification_rejects_request_and_terminal_variants() {
        // Approval waits are requests (now `Requirement::NeedInteraction`) and
        // turn completion is a quiescent `StepOutcome`, so neither has a
        // `Notification` counterpart. Their old tagged encodings must fail to
        // decode as a notification, which pins the notification variant set
        // structurally.
        let excluded = [
            json!({ "type": "awaiting_approval", "data": {} }),
            json!({ "type": "done", "data": { "status": "completed" } }),
        ];

        for encoded in excluded {
            serde_json::from_value::<Notification>(encoded)
                .expect_err("notification must not carry request/terminal variants");
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
    fn agent_input_round_trips_for_checked_user_and_pivot_shapes() {
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
        let pivot = PivotMessage::new(
            message_id(),
            user_message("change direction"),
            PivotSource::Human,
        )
        .expect("valid pivot payload");
        assert_json_round_trip(AgentInput::pivot(pivot));
    }

    #[test]
    fn llm_notification_accepts_stream_stop_reason() {
        let notification = Notification::Llm(StreamEvent::MessageStop {
            stop_reason: StopReason::normalize("end_turn"),
        });

        assert_json_round_trip(notification);
    }
}
