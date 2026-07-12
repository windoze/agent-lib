//! Immutable data types for one complete, closed Conversation turn.
//!
//! A live [`Turn`] deliberately has no public constructor and cannot be
//! deserialized directly. The crate-private `TurnData` shape can represent
//! draft or persisted input, including a temporarily dangling tool call. The
//! Conversation validator is the only path that converts this data into a live
//! closed turn.
//!
//! Callers can inspect messages but cannot replace one through the returned
//! shared slice:
//!
//! ```compile_fail
//! use agent_lib::conversation::{ConversationMessage, Turn};
//!
//! fn replace_message(
//!     turn: &mut Turn,
//!     replacement: ConversationMessage,
//! ) {
//!     turn.messages()[0] = replacement;
//! }
//! ```
//!
//! Direct deserialization is also withheld until the same validation gate can
//! check all closed-turn invariants:
//!
//! ```compile_fail
//! use agent_lib::conversation::Turn;
//!
//! let _unchecked: Turn = serde_json::from_str("{}").unwrap();
//! ```
//!
//! External code cannot assemble a live turn from raw containers either:
//!
//! ```compile_fail
//! use agent_lib::conversation::Turn;
//!
//! let _forged = Turn {
//!     id: todo!(),
//!     messages: todo!(),
//!     pairings: todo!(),
//!     parent: None,
//!     meta: todo!(),
//! };
//! ```

use crate::{
    conversation::{
        ConversationMessage, MessageId, ToolCallId, TurnId, validation::ValidatedTurnData,
    },
    model::{
        normalized::{Normalized, StopReason},
        usage::Usage,
    },
};
use serde::{Deserialize, Serialize, Serializer};
use serde_json::{Map, Value};
use std::sync::Arc;

/// One complete exchange cycle whose messages and tool calls are closed.
///
/// Messages and pairings use shared immutable ownership, so cloning a turn
/// does not clone or re-identify its history. The fields stay private and the
/// type intentionally has no public constructor; the commit validator is the
/// sole creation and deserialization gate for live turns.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Turn {
    id: TurnId,
    messages: Arc<[ConversationMessage]>,
    pairings: Arc<[ToolPairing]>,
    parent: Option<TurnId>,
    meta: TurnMeta,
}

impl Turn {
    /// Materializes a live turn from the validator's unforgeable certificate.
    pub(super) fn from_validated(data: ValidatedTurnData) -> Self {
        let data = data.into_data();
        let pairings = data
            .pairings
            .into_iter()
            .map(|pairing| ToolPairing {
                call_id: pairing.call_id,
                provider_call_id: pairing.provider_call_id,
                call_msg: pairing.call_msg,
                result_msg: pairing
                    .result_msg
                    .expect("validated turn pairings always have a result message"),
            })
            .collect::<Vec<_>>();
        Self {
            id: data.id,
            messages: Arc::from(data.messages),
            pairings: Arc::from(pairings),
            parent: data.parent,
            meta: data.meta,
        }
    }

    /// Returns this turn's externally supplied stable identity.
    #[must_use]
    pub const fn id(&self) -> TurnId {
        self.id
    }

    /// Returns the ordered immutable messages in this closed turn.
    #[must_use]
    pub fn messages(&self) -> &[ConversationMessage] {
        &self.messages
    }

    /// Returns all complete tool-call pairings in this closed turn.
    #[must_use]
    pub fn pairings(&self) -> &[ToolPairing] {
        &self.pairings
    }

    /// Returns the parent turn in the immutable history tree, when present.
    #[must_use]
    pub const fn parent(&self) -> Option<TurnId> {
        self.parent
    }

    /// Returns read-only metadata associated with this complete turn.
    #[must_use]
    pub const fn meta(&self) -> &TurnMeta {
        &self.meta
    }
}

/// Serializes a live turn through the validator-facing data-transfer shape.
///
/// There is intentionally no inverse `Deserialize` implementation on
/// [`Turn`]: persisted input remains in the crate-private data-transfer shape
/// until the same validator used by in-memory commits certifies it.
impl Serialize for Turn {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        TurnData::from(self).serialize(serializer)
    }
}

/// A complete association between one framework tool call and its messages.
///
/// The provider call id remains separate from [`ToolCallId`]. Unlike the
/// crate-private draft representation, `result_msg` is not optional, so a
/// closed pairing cannot express a dangling tool call. A missing provider id
/// is accepted only when the call/result message anchors identify exactly one
/// matching provider id; the closed pairing preserves the original `None`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolPairing {
    call_id: ToolCallId,
    provider_call_id: Option<String>,
    call_msg: MessageId,
    result_msg: MessageId,
}

impl ToolPairing {
    /// Returns the framework-owned identity used for tool bookkeeping.
    #[must_use]
    pub const fn call_id(&self) -> ToolCallId {
        self.call_id
    }

    /// Returns the original provider call id when the provider supplied one.
    #[must_use]
    pub fn provider_call_id(&self) -> Option<&str> {
        self.provider_call_id.as_deref()
    }

    /// Returns the message containing the matching tool-use block.
    #[must_use]
    pub const fn call_msg(&self) -> MessageId {
        self.call_msg
    }

    /// Returns the message containing the matching tool-result block.
    #[must_use]
    pub const fn result_msg(&self) -> MessageId {
        self.result_msg
    }
}

/// Externally supplied metadata for one complete turn.
///
/// The timestamp is kept as a caller-defined stable string (normally an
/// RFC 3339 value); this model never reads a clock. `extra` is a separate
/// object rather than a flattened override channel. Neither metadata nor a
/// future annotation can replace or mutate any historical message payload.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnMeta {
    usage: Usage,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    responses: Vec<TurnResponseMeta>,
    timestamp: Option<String>,
    source: Option<String>,
    extra: Map<String, Value>,
}

impl TurnMeta {
    /// Creates metadata from values supplied by the caller or Client result.
    #[must_use]
    pub fn new(
        usage: Usage,
        timestamp: Option<String>,
        source: Option<String>,
        extra: Map<String, Value>,
    ) -> Self {
        Self {
            usage,
            responses: Vec::new(),
            timestamp,
            source,
            extra,
        }
    }

    /// Returns the provider-neutral token usage aggregated for this turn.
    #[must_use]
    pub const fn usage(&self) -> &Usage {
        &self.usage
    }

    /// Returns metadata retained for each frozen assistant response in order.
    #[must_use]
    pub fn responses(&self) -> &[TurnResponseMeta] {
        &self.responses
    }

    /// Returns the optional caller-supplied timestamp without interpreting it.
    #[must_use]
    pub fn timestamp(&self) -> Option<&str> {
        self.timestamp.as_deref()
    }

    /// Returns the optional caller-supplied source label.
    #[must_use]
    pub fn source(&self) -> Option<&str> {
        self.source.as_deref()
    }

    /// Returns extensible turn data through a shared, non-mutating view.
    #[must_use]
    pub const fn extra(&self) -> &Map<String, Value> {
        &self.extra
    }

    /// Adds response-derived facts while building a pending turn's final DTO.
    pub(crate) fn merge_pending(&mut self, usage: Usage, responses: &[TurnResponseMeta]) {
        self.usage.merge(usage);
        self.responses.extend_from_slice(responses);
    }
}

/// Metadata associated with one frozen assistant response in a turn.
///
/// Token usage is aggregated once at [`TurnMeta::usage`]. This per-response
/// record keeps the response's normalized stop reason and provider evidence
/// without flattening multiple tool round-trips into one lossy map.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnResponseMeta {
    message_id: MessageId,
    stop_reason: Normalized<StopReason>,
    extra: Map<String, Value>,
}

impl TurnResponseMeta {
    /// Creates metadata for one already-frozen assistant message.
    #[must_use]
    pub fn new(
        message_id: MessageId,
        stop_reason: Normalized<StopReason>,
        extra: Map<String, Value>,
    ) -> Self {
        Self {
            message_id,
            stop_reason,
            extra,
        }
    }

    /// Returns the assistant message whose response produced this metadata.
    #[must_use]
    pub const fn message_id(&self) -> MessageId {
        self.message_id
    }

    /// Returns the normalized response stop reason and retained raw spelling.
    #[must_use]
    pub const fn stop_reason(&self) -> &Normalized<StopReason> {
        &self.stop_reason
    }

    /// Returns unmodeled response-level provider metadata without mutation.
    #[must_use]
    pub const fn extra(&self) -> &Map<String, Value> {
        &self.extra
    }
}

/// Crate-private draft and serde DTO for one turn.
///
/// This is data, not a certified closed [`Turn`]. Its tool-pairing DTO permits
/// `result_msg: None` so pending state and untrusted persisted input can be
/// represented without weakening the public closed type. The sole
/// Conversation validator checks this shape before constructing a live turn.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TurnData {
    pub(crate) id: TurnId,
    pub(crate) messages: Vec<ConversationMessage>,
    pub(crate) pairings: Vec<ToolPairingData>,
    pub(crate) parent: Option<TurnId>,
    pub(crate) meta: TurnMeta,
    #[serde(default, skip_serializing_if = "TurnCompletion::is_complete")]
    pub(crate) completion: TurnCompletion,
}

/// Whether a draft has finished freezing all Client content.
///
/// The default and the only state emitted by a live [`Turn`] is `Complete`.
/// In-memory pending code must set `PendingContent` while a block or message is
/// unfinished; validation rejects that marker before inspecting parsed JSON.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TurnCompletion {
    /// Every message contains complete-state Client content.
    #[default]
    Complete,
    /// At least one message or content block is still pending.
    PendingContent,
}

impl TurnCompletion {
    /// Lets serde preserve the existing closed-turn shape for complete data.
    fn is_complete(&self) -> bool {
        *self == Self::Complete
    }
}

/// Crate-private tool-pairing data that may still be pending a result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ToolPairingData {
    pub(crate) call_id: ToolCallId,
    pub(crate) provider_call_id: Option<String>,
    pub(crate) call_msg: MessageId,
    pub(crate) result_msg: Option<MessageId>,
}

/// Copies a live closed turn into its persistence/draft data shape.
impl From<&Turn> for TurnData {
    fn from(turn: &Turn) -> Self {
        Self {
            id: turn.id,
            messages: turn.messages.iter().cloned().collect(),
            pairings: turn.pairings.iter().map(ToolPairingData::from).collect(),
            parent: turn.parent,
            meta: turn.meta.clone(),
            completion: TurnCompletion::Complete,
        }
    }
}

/// Marks every pairing copied from a live turn as complete in the DTO.
impl From<&ToolPairing> for ToolPairingData {
    fn from(pairing: &ToolPairing) -> Self {
        Self {
            call_id: pairing.call_id,
            provider_call_id: pairing.provider_call_id.clone(),
            call_msg: pairing.call_msg,
            result_msg: Some(pairing.result_msg),
        }
    }
}

#[cfg(test)]
mod tests;
