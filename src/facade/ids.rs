//! Built-in monotonic identity source shared by the Chat and Agent facades.
//!
//! The library core never mints ids: [`crate::agent::RequirementIds`] and
//! [`crate::agent::ToolExecutionIds`] are caller-supplied hooks. The facade is
//! an assembly layer, so it provides one small default implementation,
//! [`FacadeIds`], modeled on the `DemoIds` helper from `examples/agent_chat.rs`.
//!
//! Every id is drawn from a single monotonic counter that starts at `1`
//! (so no id is ever the nil UUID) and is widened into a
//! [`uuid::Uuid`] via [`uuid::Uuid::from_u128`]. A single [`FacadeIds`] is
//! cheaply clonable (it shares one atomic counter), so the same source can be
//! handed to a machine, a handler, and the facade driver simultaneously while
//! still producing globally unique ids.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::agent::{
    AgentId, RequirementError, RequirementId, RequirementIds, RequirementKindTag, RunId, StepId,
    ToolExecutionIds, ToolRuntimeError, ToolSetId, TraceNodeId,
};
use crate::conversation::{Conversation, ConversationId, MessageId, ToolCallId, TurnId};
use crate::model::tool::ToolCall;

/// A cloneable, monotonic identity source for the facade layer.
///
/// Clones share the same underlying counter, so ids stay globally unique across
/// every clone. The counter starts at `1`, guaranteeing no minted id is the nil
/// UUID.
#[derive(Clone, Debug)]
pub struct FacadeIds {
    counter: Arc<AtomicU64>,
}

impl FacadeIds {
    /// Creates a fresh identity source whose counter starts at `1`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            counter: Arc::new(AtomicU64::new(1)),
        }
    }

    /// Creates an identity source whose counter starts at `start` (clamped to at
    /// least `1`, so no minted id is the nil UUID).
    ///
    /// This is mainly used to continue a previously advanced counter, for example
    /// after restoring a [`Conversation`] from a snapshot. Prefer
    /// [`continuing_after`](FacadeIds::continuing_after) when the exact start is
    /// derived from restored history.
    #[must_use]
    pub fn seeded(start: u64) -> Self {
        Self {
            counter: Arc::new(AtomicU64::new(start.max(1))),
        }
    }

    /// Creates an identity source seeded to continue *after* every id already
    /// present in `conversation`, so newly minted ids cannot collide with
    /// restored history.
    ///
    /// A [`ConversationSnapshot`](crate::conversation::ConversationSnapshot) is
    /// data-only and does not carry a runtime counter, so a naive fresh source
    /// (starting at `1`) would re-mint ids that already exist in the restored
    /// history and be rejected as duplicates. This constructor scans the restored
    /// ids and continues past the largest one.
    ///
    /// Only ids whose UUID fits the built-in monotonic counter space (the low
    /// 64 bits) are considered; externally supplied random or UUIDv7 ids are
    /// ignored, because the small counter values this source mints can never
    /// collide with them.
    #[must_use]
    pub fn continuing_after(conversation: &Conversation) -> Self {
        let mut max_seen: u64 = 0;
        let mut consider = |uuid: uuid::Uuid| {
            if let Ok(narrow) = u64::try_from(uuid.as_u128()) {
                max_seen = max_seen.max(narrow);
            }
        };

        consider(conversation.id().into_uuid());
        for turn in conversation.turns() {
            consider(turn.id().into_uuid());
            for message in turn.messages() {
                consider(message.id().into_uuid());
            }
            for pairing in turn.pairings() {
                consider(pairing.call_id().into_uuid());
                consider(pairing.call_msg().into_uuid());
                consider(pairing.result_msg().into_uuid());
            }
        }

        Self::seeded(max_seen.saturating_add(1))
    }

    /// Returns the next raw UUID, advancing the shared counter.
    fn next_uuid(&self) -> uuid::Uuid {
        uuid::Uuid::from_u128(u128::from(self.counter.fetch_add(1, Ordering::SeqCst)))
    }

    /// Mints the next Agent identity.
    #[must_use]
    pub fn agent_id(&self) -> AgentId {
        AgentId::new(self.next_uuid())
    }

    /// Mints the next run identity.
    #[must_use]
    pub fn run_id(&self) -> RunId {
        RunId::new(self.next_uuid())
    }

    /// Mints the next tool-set identity.
    #[must_use]
    pub fn tool_set_id(&self) -> ToolSetId {
        ToolSetId::new(self.next_uuid())
    }

    /// Mints the next Conversation identity.
    #[must_use]
    pub fn conversation_id(&self) -> ConversationId {
        ConversationId::new(self.next_uuid())
    }

    /// Mints the next turn identity.
    #[must_use]
    pub fn turn_id(&self) -> TurnId {
        TurnId::new(self.next_uuid())
    }

    /// Mints the next message identity.
    #[must_use]
    pub fn message_id(&self) -> MessageId {
        MessageId::new(self.next_uuid())
    }

    /// Mints the next framework tool-call identity.
    ///
    /// Used by the facade to key a rules-routed delegation into its shared
    /// delegation recorder when no model-issued tool call supplies one
    /// (`docs/facade-api.md` §13.2). Named to avoid clashing with the
    /// `ToolExecutionIds::tool_call_id` trait method, which derives a call id
    /// from an existing model-issued [`ToolCall`].
    #[must_use]
    pub fn fresh_tool_call_id(&self) -> ToolCallId {
        ToolCallId::new(self.next_uuid())
    }

    /// Mints the next Agent step identity.
    #[must_use]
    pub fn step_id(&self) -> StepId {
        StepId::new(self.next_uuid())
    }

    /// Builds a stable trace-root node id from a caller-provided label.
    #[must_use]
    pub fn trace_root(&self, label: impl Into<String>) -> TraceNodeId {
        TraceNodeId::new(label.into())
    }
}

impl Default for FacadeIds {
    fn default() -> Self {
        Self::new()
    }
}

impl RequirementIds for FacadeIds {
    fn next_requirement_id(
        &self,
        _kind_tag: RequirementKindTag,
    ) -> Result<RequirementId, RequirementError> {
        Ok(RequirementId::new(self.next_uuid()))
    }
}

impl ToolExecutionIds for FacadeIds {
    fn tool_call_id(&self, _call: &ToolCall) -> Result<ToolCallId, ToolRuntimeError> {
        Ok(ToolCallId::new(self.next_uuid()))
    }

    fn tool_result_message_id(
        &self,
        _call_id: ToolCallId,
        _call: &ToolCall,
    ) -> Result<MessageId, ToolRuntimeError> {
        Ok(MessageId::new(self.next_uuid()))
    }

    fn next_assistant_message_id(&self) -> Result<MessageId, ToolRuntimeError> {
        Ok(MessageId::new(self.next_uuid()))
    }

    fn next_step_id(&self) -> Result<StepId, ToolRuntimeError> {
        Ok(StepId::new(self.next_uuid()))
    }
}

#[cfg(test)]
mod tests {
    use super::FacadeIds;
    use crate::agent::{RequirementIds, RequirementKindTag, ToolExecutionIds};
    use crate::model::tool::ToolCall;
    use serde_json::{Map, Value};
    use std::collections::HashSet;

    fn sample_call() -> ToolCall {
        ToolCall {
            id: "provider-call-1".to_owned(),
            name: "noop".to_owned(),
            input: Value::Object(Map::new()),
        }
    }

    #[test]
    fn ids_are_unique_and_non_nil_across_families() {
        let ids = FacadeIds::new();
        let mut seen: HashSet<uuid::Uuid> = HashSet::new();

        // Collect a mix of ids from every family and assert none collide or are nil.
        let raw = [
            ids.agent_id().into_uuid(),
            ids.run_id().into_uuid(),
            ids.tool_set_id().into_uuid(),
            ids.conversation_id().into_uuid(),
            ids.turn_id().into_uuid(),
            ids.message_id().into_uuid(),
            ToolExecutionIds::tool_call_id(&ids, &sample_call())
                .expect("tool call id")
                .into_uuid(),
            ids.step_id().into_uuid(),
        ];
        for uuid in raw {
            assert_ne!(uuid, uuid::Uuid::nil());
            assert!(seen.insert(uuid), "duplicate id minted: {uuid}");
        }
    }

    #[test]
    fn clones_share_the_same_counter() {
        let ids = FacadeIds::new();
        let clone = ids.clone();

        let first = ids.turn_id().into_uuid();
        let second = clone.turn_id().into_uuid();

        assert_ne!(first, second);
    }

    #[test]
    fn trait_impls_hand_out_fresh_ids() {
        let ids = FacadeIds::new();
        let call = sample_call();

        let req = ids
            .next_requirement_id(RequirementKindTag::Llm)
            .expect("requirement id");
        let tool_call = ids.tool_call_id(&call).expect("tool call id");
        let result_msg = ids
            .tool_result_message_id(tool_call, &call)
            .expect("tool result message id");
        let assistant_msg = ids
            .next_assistant_message_id()
            .expect("assistant message id");
        let step = ids.next_step_id().expect("step id");

        assert_ne!(req.into_uuid(), uuid::Uuid::nil());
        assert_ne!(tool_call.into_uuid(), result_msg.into_uuid());
        assert_ne!(result_msg.into_uuid(), assistant_msg.into_uuid());
        assert_ne!(assistant_msg.into_uuid(), step.into_uuid());
    }

    #[test]
    fn seeded_starts_from_the_supplied_value_and_clamps_zero() {
        let ids = FacadeIds::seeded(10);
        assert_eq!(ids.turn_id().into_uuid(), uuid::Uuid::from_u128(10));
        assert_eq!(ids.turn_id().into_uuid(), uuid::Uuid::from_u128(11));

        // A zero seed is clamped to 1 so no minted id is the nil UUID.
        let clamped = FacadeIds::seeded(0);
        assert_eq!(clamped.turn_id().into_uuid(), uuid::Uuid::from_u128(1));
    }
}
