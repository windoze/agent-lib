use super::{Case, assert_rejected_case};
use crate::conversation::validation::tests::fixtures::{
    conversation, draft, message, message_id, pairing, single_tool_draft, text, text_draft,
    tool_call_id, tool_result, tool_use, turn_id,
};
use crate::{
    conversation::{CommitError, Conversation, ConversationError},
    model::message::Role,
};

fn text_history() -> Conversation {
    let mut history = conversation();
    history
        .commit_draft(text_draft(10, None, 100))
        .expect("seed text history");
    history
}

#[test]
fn duplicate_identity_and_parent_errors_are_classified_atomically() {
    let mut tool_history = conversation();
    tool_history
        .commit_draft(single_tool_draft(10, None, 100, 500, "old-call"))
        .expect("seed tool history");

    let duplicate_message_in_turn = draft(
        10,
        None,
        vec![
            message(100, Role::User, vec![text("question")]),
            message(100, Role::Assistant, vec![text("answer")]),
        ],
        Vec::new(),
    );
    let duplicate_call_in_turn = draft(
        10,
        None,
        vec![
            message(100, Role::User, vec![text("question")]),
            message(
                101,
                Role::Assistant,
                vec![tool_use("parallel-a"), tool_use("parallel-b")],
            ),
            message(
                102,
                Role::Tool,
                vec![tool_result("parallel-a"), tool_result("parallel-b")],
            ),
            message(103, Role::Assistant, vec![text("answer")]),
        ],
        vec![
            pairing(500, "parallel-a", 101, 102),
            pairing(500, "parallel-b", 101, 102),
        ],
    );

    let cases = vec![
        Case {
            name: "duplicate turn id in history",
            conversation: text_history(),
            data: text_draft(10, Some(turn_id(10)), 200),
            expected: ConversationError::Commit(CommitError::DuplicateTurnId {
                turn_id: turn_id(10),
            }),
        },
        Case {
            name: "duplicate message id in candidate",
            conversation: conversation(),
            data: duplicate_message_in_turn,
            expected: ConversationError::Commit(CommitError::DuplicateMessageId {
                message_id: message_id(100),
            }),
        },
        Case {
            name: "duplicate message id in history",
            conversation: text_history(),
            data: draft(
                11,
                Some(turn_id(10)),
                vec![
                    message(100, Role::User, vec![text("reused id")]),
                    message(201, Role::Assistant, vec![text("answer")]),
                ],
                Vec::new(),
            ),
            expected: ConversationError::Commit(CommitError::DuplicateMessageId {
                message_id: message_id(100),
            }),
        },
        Case {
            name: "duplicate tool-call id in candidate",
            conversation: conversation(),
            data: duplicate_call_in_turn,
            expected: ConversationError::Commit(CommitError::DuplicateToolCallId {
                call_id: tool_call_id(500),
            }),
        },
        Case {
            name: "duplicate tool-call id in history",
            conversation: tool_history,
            data: single_tool_draft(11, Some(turn_id(10)), 200, 500, "new-provider-call"),
            expected: ConversationError::Commit(CommitError::DuplicateToolCallId {
                call_id: tool_call_id(500),
            }),
        },
        Case {
            name: "wrong parent",
            conversation: text_history(),
            data: text_draft(11, None, 200),
            expected: ConversationError::Commit(CommitError::ParentMismatch {
                expected: Some(turn_id(10)),
                actual: None,
            }),
        },
    ];

    for case in cases {
        assert_rejected_case(case);
    }
}
