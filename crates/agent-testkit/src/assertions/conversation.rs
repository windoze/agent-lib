//! Read-only assertions over a committed [`Conversation`].
//!
//! [`assert_conversation`] is the entry point; it returns a [`Copy`]
//! [`ConversationAssertions`] builder whose checks chain and whose failures name
//! the turn/message coordinates and a compact conversation summary.

use agent_lib::conversation::{Conversation, ConversationMessage};
use agent_lib::model::content::ContentBlock;
use agent_lib::model::message::{Message, Role};
use agent_lib::model::tool::ToolStatus;

/// Starts a fluent, read-only assertion over `conversation`.
#[must_use]
pub fn assert_conversation(conversation: &Conversation) -> ConversationAssertions<'_> {
    ConversationAssertions { conversation }
}

/// A fluent, read-only assertion builder over a [`Conversation`].
///
/// Construct one with [`assert_conversation`]. Every method returns `Self` so
/// checks chain; a failed check panics with a diagnostic that includes the
/// offending coordinates and a compact summary of the committed turns.
#[derive(Clone, Copy)]
pub struct ConversationAssertions<'a> {
    conversation: &'a Conversation,
}

impl<'a> ConversationAssertions<'a> {
    /// Returns the underlying conversation for an escape-hatch inspection.
    pub const fn conversation(self) -> &'a Conversation {
        self.conversation
    }

    /// Asserts the number of committed (closed) turns on the current lineage.
    pub fn committed_turns(self, expected: usize) -> Self {
        let actual = self.conversation.turns().len();
        assert!(
            actual == expected,
            "expected {expected} committed turn(s), found {actual}\n{}",
            self.summary()
        );
        self
    }

    /// Asserts that an uncommitted pending turn is present.
    pub fn pending_present(self) -> Self {
        assert!(
            self.conversation.pending().is_some(),
            "expected a pending turn, but the conversation had none\n{}",
            self.summary()
        );
        self
    }

    /// Asserts that no uncommitted pending turn remains (the turn committed).
    pub fn pending_none(self) -> Self {
        if let Some(pending) = self.conversation.pending() {
            panic!(
                "expected no pending turn, but one was open with {} frozen message(s) and {} open call(s)\n{}",
                pending.messages().len(),
                pending.open_calls().count(),
                self.summary()
            );
        }
        self
    }

    /// Asserts the number of still-open (unpaired) tool calls in the pending
    /// turn. A conversation with no pending turn has zero open calls.
    pub fn open_call_count(self, expected: usize) -> Self {
        let actual = self
            .conversation
            .pending()
            .map_or(0, |pending| pending.open_calls().count());
        assert!(
            actual == expected,
            "expected {expected} open tool call(s), found {actual}\n{}",
            self.summary()
        );
        self
    }

    /// Asserts the role of the message at `message` within committed `turn`.
    pub fn message_role(self, turn: usize, message: usize, expected: Role) -> Self {
        let actual = self.message_at(turn, message).payload().role;
        assert!(
            actual == expected,
            "expected message [turn {turn}][msg {message}] to be {expected:?}, found {actual:?}\n{}",
            self.summary()
        );
        self
    }

    /// Asserts that the concatenated text of message `message` in committed
    /// `turn` exactly equals `expected`.
    pub fn message_text(self, turn: usize, message: usize, expected: &str) -> Self {
        let actual = message_text(self.message_at(turn, message).payload());
        assert!(
            actual == expected,
            "expected message [turn {turn}][msg {message}] text {expected:?}, found {actual:?}\n{}",
            self.summary()
        );
        self
    }

    /// Asserts that the text of message `message` in committed `turn` contains
    /// `needle`.
    pub fn message_text_contains(self, turn: usize, message: usize, needle: &str) -> Self {
        let actual = message_text(self.message_at(turn, message).payload());
        assert!(
            actual.contains(needle),
            "expected message [turn {turn}][msg {message}] text to contain {needle:?}, found {actual:?}\n{}",
            self.summary()
        );
        self
    }

    /// Asserts that the last assistant message's text exactly equals `expected`.
    pub fn last_assistant_text(self, expected: &str) -> Self {
        let actual = self.last_assistant_text_value();
        assert!(
            actual == expected,
            "expected last assistant text {expected:?}, found {actual:?}\n{}",
            self.summary()
        );
        self
    }

    /// Asserts that the last assistant message's text contains `needle`.
    pub fn last_assistant_text_contains(self, needle: &str) -> Self {
        let actual = self.last_assistant_text_value();
        assert!(
            actual.contains(needle),
            "expected last assistant text to contain {needle:?}, found {actual:?}\n{}",
            self.summary()
        );
        self
    }

    /// Asserts the [`ToolStatus`] of the tool result answering the tool call
    /// whose provider id is `provider_call_id`.
    pub fn tool_result_status(self, provider_call_id: &str, expected: ToolStatus) -> Self {
        let actual = self.find_tool_result_status(provider_call_id);
        match actual {
            Some(status) if status == expected => self,
            Some(status) => panic!(
                "expected tool result for {provider_call_id:?} to be {expected:?}, found {status:?}\n{}",
                self.summary()
            ),
            None => panic!(
                "expected a tool result for {provider_call_id:?}, but none was found\n{}",
                self.summary()
            ),
        }
    }

    /// Asserts the number of completed tool-call pairings in committed `turn`.
    pub fn pairing_count(self, turn: usize, expected: usize) -> Self {
        let actual = self.turn_at(turn).pairings().len();
        assert!(
            actual == expected,
            "expected {expected} pairing(s) in turn {turn}, found {actual}\n{}",
            self.summary()
        );
        self
    }

    fn turn_at(self, turn: usize) -> &'a agent_lib::conversation::Turn {
        let turns = self.conversation.turns();
        turns.get(turn).unwrap_or_else(|| {
            panic!(
                "turn index {turn} is out of range: only {} committed turn(s)\n{}",
                turns.len(),
                self.summary()
            )
        })
    }

    fn message_at(self, turn: usize, message: usize) -> &'a ConversationMessage {
        let messages = self.turn_at(turn).messages();
        messages.get(message).unwrap_or_else(|| {
            panic!(
                "message index {message} is out of range in turn {turn}: only {} message(s)\n{}",
                messages.len(),
                self.summary()
            )
        })
    }

    fn last_assistant_text_value(self) -> String {
        for message in self.messages_in_order().rev() {
            if message.payload().role == Role::Assistant {
                return message_text(message.payload());
            }
        }
        panic!(
            "expected at least one assistant message, but the conversation had none\n{}",
            self.summary()
        )
    }

    fn find_tool_result_status(self, provider_call_id: &str) -> Option<ToolStatus> {
        for message in self.messages_in_order() {
            for block in &message.payload().content {
                if let ContentBlock::ToolResult {
                    tool_use_id,
                    status,
                    ..
                } = block
                    && tool_use_id == provider_call_id
                {
                    return Some(*status);
                }
            }
        }
        None
    }

    /// Iterates every message on the committed lineage, then any pending
    /// messages, in conversation order.
    fn messages_in_order(self) -> impl DoubleEndedIterator<Item = &'a ConversationMessage> {
        let committed = self
            .conversation
            .turns()
            .iter()
            .flat_map(|turn| turn.messages().iter());
        let pending = self
            .conversation
            .pending()
            .into_iter()
            .flat_map(|pending| pending.messages().iter());
        committed.chain(pending)
    }

    fn summary(self) -> String {
        let mut out = String::from("conversation:");
        for (turn_index, turn) in self.conversation.turns().iter().enumerate() {
            out.push_str(&format!("\n  turn {turn_index}:"));
            for (message_index, message) in turn.messages().iter().enumerate() {
                out.push_str(&format!(
                    "\n    [msg {message_index}] {:?}: {}",
                    message.payload().role,
                    describe_content(message.payload())
                ));
            }
        }
        match self.conversation.pending() {
            Some(pending) => out.push_str(&format!(
                "\n  pending: {} frozen message(s), {} open call(s)",
                pending.messages().len(),
                pending.open_calls().count()
            )),
            None => out.push_str("\n  pending: none"),
        }
        out
    }
}

/// Concatenates every [`ContentBlock::Text`] payload of `message`.
fn message_text(message: &Message) -> String {
    let mut text = String::new();
    for block in &message.content {
        if let ContentBlock::Text { text: value, .. } = block {
            text.push_str(value);
        }
    }
    text
}

/// Renders a one-line summary of a message's content blocks for diagnostics.
fn describe_content(message: &Message) -> String {
    if message.content.is_empty() {
        return "<empty>".to_owned();
    }
    message
        .content
        .iter()
        .map(|block| match block {
            ContentBlock::Text { text, .. } => format!("text{text:?}"),
            ContentBlock::ToolUse { name, id, .. } => format!("tool_use({name}, id={id})"),
            ContentBlock::ToolResult {
                tool_use_id,
                status,
                ..
            } => format!("tool_result(id={tool_use_id}, {status:?})"),
            ContentBlock::Thinking { .. } => "thinking".to_owned(),
            ContentBlock::Image { .. } => "image".to_owned(),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::assert_conversation;
    use crate::fixtures::{
        agent_spec_with_tools, agent_state, default_machine, root_context, tool_call, weather_tool,
    };
    use crate::handlers::{ScriptedLlmHandler, ScriptedToolHandler};
    use crate::scope::TestScope;
    use crate::script::{LlmStep, ToolStep};
    use crate::{harness::DrainHarness, ids::SeqIds};
    use agent_lib::model::message::Role;
    use agent_lib::model::tool::ToolStatus;
    use serde_json::json;
    use std::sync::Arc;

    /// Drives a full user -> tool_use -> tool result -> final-text turn and
    /// returns the machine so its committed conversation can be asserted on.
    async fn weather_turn() -> agent_lib::agent::DefaultAgentMachine {
        let ids = SeqIds::new();
        let ctx = root_context(&ids);
        let spec = agent_spec_with_tools(&ids, vec![weather_tool()]);
        let machine = default_machine(&ids, agent_state(&ids, spec));

        let llm = ScriptedLlmHandler::from_steps([
            LlmStep::tool_use(vec![tool_call(
                "call-weather",
                "get_weather",
                json!({ "city": "Shanghai" }),
            )]),
            LlmStep::text("It is sunny in Shanghai."),
        ]);
        let tool = ScriptedToolHandler::from_steps([ToolStep::ok("call-weather", "Sunny, 20C")]);
        let scope = TestScope::builder()
            .llm(Arc::new(llm))
            .tool(Arc::new(tool))
            .build();

        let mut harness = DrainHarness::with_ids(machine, &scope, None, &ctx, ids);
        harness.run_user("weather?").await.expect("turn drains");
        harness.into_machine()
    }

    #[tokio::test]
    async fn happy_path_covers_every_conversation_check() {
        let machine = weather_turn().await;
        assert_conversation(machine.state().conversation())
            .committed_turns(1)
            .pending_none()
            .open_call_count(0)
            .pairing_count(0, 1)
            .message_role(0, 0, Role::User)
            .message_text(0, 0, "weather?")
            .message_role(0, 1, Role::Assistant)
            .message_role(0, 2, Role::Tool)
            .message_role(0, 3, Role::Assistant)
            .message_text_contains(0, 3, "sunny")
            .tool_result_status("call-weather", ToolStatus::Ok)
            .last_assistant_text("It is sunny in Shanghai.");
    }

    #[tokio::test]
    async fn last_assistant_text_failure_message_carries_context() {
        let machine = weather_turn().await;
        let conversation = machine.state().conversation();
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            assert_conversation(conversation).last_assistant_text("rainy");
        }))
        .expect_err("mismatched assistant text must panic");
        let message = panic
            .downcast_ref::<String>()
            .expect("panic payload is a String");
        assert!(
            message.contains("expected last assistant text \"rainy\""),
            "message names the expectation: {message}"
        );
        assert!(
            message.contains("It is sunny in Shanghai."),
            "message shows the actual conversation: {message}"
        );
        assert!(
            message.contains("turn 0"),
            "message includes a conversation summary: {message}"
        );
    }

    #[tokio::test]
    async fn committed_turns_failure_message_reports_actual_count() {
        let machine = weather_turn().await;
        let conversation = machine.state().conversation();
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            assert_conversation(conversation).committed_turns(3);
        }))
        .expect_err("wrong turn count must panic");
        let message = panic
            .downcast_ref::<String>()
            .expect("panic payload is a String");
        assert!(
            message.contains("expected 3 committed turn(s), found 1"),
            "message names expected and actual: {message}"
        );
    }
}
