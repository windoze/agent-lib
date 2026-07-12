//! Classified validator failures and transaction atomicity.

mod content;
mod identity;
mod pairing;
mod serde;
mod state;

use super::fixtures::committed_state;
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
    let before = committed_state(&case.conversation);
    let actual = case
        .conversation
        .commit_draft(case.data)
        .expect_err(case.name);
    assert_eq!(actual, case.expected, "case `{}` error", case.name);
    assert_eq!(
        committed_state(&case.conversation),
        before,
        "case `{}` changed committed conversation state",
        case.name
    );
    assert!(case.conversation.pending().is_none());
}
