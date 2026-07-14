//! Read-only assertion helpers for the complex agent-effect mock tests
//! (milestone 1, M1-3).
//!
//! `docs/complex-tests.md` §6 requires that a failing complex scenario surface
//! enough context to localize the mismatch: the committed role sequence, the tool
//! result status, the mock store operation log, and the handler call counts. A
//! bare `assert_eq!` over a deep observation buries all of that, so this module
//! provides small, focused helpers whose panics embed exactly the observation
//! that failed.
//!
//! Every helper is strictly read-only — it inspects a [`MockPlanBlackboardStore`],
//! a committed [`Conversation`], a [`ComplexToolHandler`] log, or an interaction
//! [`InteractionCallLog`], and never mutates the machine, store, or context. The
//! conversation helpers deliberately build on
//! [`agent_testkit::assert_conversation`] rather than re-implementing its
//! coordinate-level checks; they add only the role-sequence and pivot-position
//! queries the complex flows need on top.

use agent_lib::conversation::Conversation;
use agent_lib::model::content::ContentBlock;
use agent_lib::model::message::{Message, Role};

use agent_testkit::handlers::InteractionCallLog;

use super::plan_blackboard::{MockPlanBlackboardStore, TaskState, TaskStatus};
use super::tools::ComplexToolHandler;

// ----- plan / blackboard store assertions ----------------------------------

/// Looks up `id` in the store's plan snapshot, panicking with the store op log
/// when the task is absent.
fn task_state(store: &MockPlanBlackboardStore, id: &str) -> TaskState {
    store
        .plan_snapshot()
        .tasks
        .get(id)
        .cloned()
        .unwrap_or_else(|| {
            panic!(
                "expected plan task {id:?} to exist, but it was not found\nstore operations:\n{}",
                store.ops_summary()
            )
        })
}

/// Asserts that plan task `id` currently has status `expected`.
///
/// On mismatch the panic prints the store operation log so the failing complex
/// test shows the exact sequence that produced the wrong status.
pub fn assert_task_status(store: &MockPlanBlackboardStore, id: &str, expected: TaskStatus) {
    let actual = task_state(store, id).status;
    assert!(
        actual == expected,
        "expected task {id:?} status {expected:?}, found {actual:?}\nstore operations:\n{}",
        store.ops_summary()
    );
}

/// Asserts that plan task `id` is currently owned by `owner`.
pub fn assert_task_owner(store: &MockPlanBlackboardStore, id: &str, owner: &str) {
    let actual = task_state(store, id).owner;
    assert!(
        actual.as_deref() == Some(owner),
        "expected task {id:?} to be owned by {owner:?}, found {actual:?}\nstore operations:\n{}",
        store.ops_summary()
    );
}

/// Asserts that plan task `id` has no owner (it is unclaimed).
pub fn assert_no_task_owner(store: &MockPlanBlackboardStore, id: &str) {
    let actual = task_state(store, id).owner;
    assert!(
        actual.is_none(),
        "expected task {id:?} to have no owner, found {actual:?}\nstore operations:\n{}",
        store.ops_summary()
    );
}

/// Asserts that plan task `id` declares exactly `expected` dependency ids, in
/// order.
pub fn assert_task_depends_on(store: &MockPlanBlackboardStore, id: &str, expected: &[&str]) {
    let actual = task_state(store, id).depends_on;
    let matches =
        actual.len() == expected.len() && actual.iter().zip(expected).all(|(a, e)| a == e);
    assert!(
        matches,
        "expected task {id:?} to depend on {expected:?}, found {actual:?}\nstore operations:\n{}",
        store.ops_summary()
    );
}

/// Asserts that the blackboard holds exactly one message per entry in
/// `expected_substrings_in_order`, each message's text containing the
/// corresponding substring, in the same order.
///
/// The exact length match doubles as a no-duplicate-side-effect guard: an extra
/// (duplicated) post makes the counts disagree and fails the assertion.
pub fn assert_board_messages(
    store: &MockPlanBlackboardStore,
    expected_substrings_in_order: &[&str],
) {
    let board = store.board_snapshot();
    let ok = board.len() == expected_substrings_in_order.len()
        && board
            .iter()
            .zip(expected_substrings_in_order)
            .all(|(message, needle)| message.text.contains(*needle));
    assert!(
        ok,
        "expected {} blackboard message(s) containing {:?} in order, found {:?}\nstore operations:\n{}",
        expected_substrings_in_order.len(),
        expected_substrings_in_order,
        board
            .iter()
            .map(|message| format!("{}:{}", message.sender, message.text))
            .collect::<Vec<_>>(),
        store.ops_summary()
    );
}

// ----- conversation assertions ---------------------------------------------

/// Returns the ordered message roles of committed `turn_index`.
///
/// Panics (with a compact conversation summary) when `turn_index` is out of
/// range, so a scenario that asserts on a turn that never committed fails with a
/// localizable diagnostic rather than a silent empty vector.
#[must_use]
pub fn role_sequence(conversation: &Conversation, turn_index: usize) -> Vec<Role> {
    let turns = conversation.turns();
    let turn = turns.get(turn_index).unwrap_or_else(|| {
        panic!(
            "turn index {turn_index} is out of range: only {} committed turn(s)\n{}",
            turns.len(),
            conversation_summary(conversation)
        )
    });
    turn.messages()
        .iter()
        .map(|message| message.payload().role)
        .collect()
}

/// Asserts that a `Role::User` pivot message whose text contains `pivot_text`
/// appears somewhere after a tool-result message in conversation order.
///
/// This is the position check `docs/complex-tests.md` §6 calls for: a mid-turn
/// pivot lands as an extra `Role::User` message after the tool-result batch that
/// preceded it. The scan walks every committed message (then any pending ones) in
/// order, and on failure prints the full role sequence so the mismatch is easy to
/// place.
pub fn assert_pivot_after_tool_result(conversation: &Conversation, pivot_text: &str) {
    let mut seen_tool_result = false;
    for message in messages_in_order(conversation) {
        if message.role == Role::Tool || has_tool_result(message) {
            seen_tool_result = true;
        } else if seen_tool_result
            && message.role == Role::User
            && message_text(message).contains(pivot_text)
        {
            return;
        }
    }
    panic!(
        "expected a Role::User pivot containing {pivot_text:?} after a tool result, but none was found\n{}",
        conversation_summary(conversation)
    );
}

// ----- handler / interaction log assertions --------------------------------

/// Asserts that tool `tool_name` executed exactly `expected` time(s) against
/// `handler`.
///
/// A recorded invocation means the tool actually ran at the effect boundary, so
/// this doubles as the "dangerous tool never executed" check when `expected` is
/// `0`. On mismatch the panic prints the handler's per-tool call log.
pub fn assert_tool_executions(handler: &ComplexToolHandler, tool_name: &str, expected: usize) {
    let actual = handler.execution_count(tool_name);
    assert!(
        actual == expected,
        "expected tool {tool_name:?} to execute {expected} time(s), found {actual}\ntool calls: {:?}",
        handler
            .calls()
            .iter()
            .map(|call| format!("{}({:?})", call.name, call.outcome))
            .collect::<Vec<_>>()
    );
}

/// Asserts that exactly `expected` interaction decisions were rendered against
/// `log` (each completed interaction is one decision).
pub fn assert_interaction_decisions(log: &InteractionCallLog, expected: usize) {
    let actual = log.completed_len();
    assert!(
        actual == expected,
        "expected {expected} interaction decision(s), found {actual} (begun={}, completed={})",
        log.len(),
        log.completed_len()
    );
}

// ----- shared helpers ------------------------------------------------------

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

/// Returns whether `message` carries any tool-result content block.
fn has_tool_result(message: &Message) -> bool {
    message
        .content
        .iter()
        .any(|block| matches!(block, ContentBlock::ToolResult { .. }))
}

/// Iterates every committed message, then any pending message, in conversation
/// order.
fn messages_in_order(conversation: &Conversation) -> impl Iterator<Item = &Message> {
    let committed = conversation
        .turns()
        .iter()
        .flat_map(|turn| turn.messages().iter());
    let pending = conversation
        .pending()
        .into_iter()
        .flat_map(|pending| pending.messages().iter());
    committed.chain(pending).map(|message| message.payload())
}

/// Renders a compact, one-line-per-message role/summary of the conversation for
/// diagnostics.
fn conversation_summary(conversation: &Conversation) -> String {
    let mut out = String::from("conversation:");
    for (turn_index, turn) in conversation.turns().iter().enumerate() {
        out.push_str(&format!("\n  turn {turn_index}:"));
        for (message_index, message) in turn.messages().iter().enumerate() {
            out.push_str(&format!(
                "\n    [msg {message_index}] {:?}: {}",
                message.payload().role,
                describe_content(message.payload())
            ));
        }
    }
    match conversation.pending() {
        Some(pending) => out.push_str(&format!(
            "\n  pending: {} frozen message(s)",
            pending.messages().len()
        )),
        None => out.push_str("\n  pending: none"),
    }
    out
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
