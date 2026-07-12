//! Classified validator failures and transaction atomicity.

mod content;
mod identity;
mod pairing;
mod serde;
mod state;

use crate::conversation::{Conversation, ConversationError, turn::TurnData};

/// One rejected transaction and its exact classified error.
struct Case {
    name: &'static str,
    conversation: Conversation,
    data: TurnData,
    expected: ConversationError,
}

/// Compares a rejected case without consuming the state needed for atomicity.
fn assert_rejected_case(mut case: Case) {
    let before = case.conversation.clone();
    let actual = case
        .conversation
        .commit_draft(case.data)
        .expect_err(case.name);
    assert_eq!(actual, case.expected, "case `{}` error", case.name);
    assert_eq!(
        case.conversation, before,
        "case `{}` changed conversation state",
        case.name
    );
}
