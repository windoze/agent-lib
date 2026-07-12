use super::{Case, assert_rejected_case};
use crate::conversation::validation::tests::fixtures::{
    conversation, draft, image, message, message_id, text, tool_use,
};
use crate::{
    conversation::{CommitError, ContentBlockKind, ConversationError},
    model::message::Role,
};

#[test]
fn invalid_start_end_role_and_block_states_are_classified_atomically() {
    let cases = vec![
        Case {
            name: "empty turn",
            conversation: conversation(),
            data: draft(10, None, Vec::new(), Vec::new()),
            expected: ConversationError::Commit(CommitError::InvalidStartState {
                first_role: None,
            }),
        },
        Case {
            name: "assistant starts turn",
            conversation: conversation(),
            data: draft(
                10,
                None,
                vec![message(100, Role::Assistant, vec![text("answer")])],
                Vec::new(),
            ),
            expected: ConversationError::Commit(CommitError::InvalidStartState {
                first_role: Some(Role::Assistant),
            }),
        },
        Case {
            name: "user without final assistant",
            conversation: conversation(),
            data: draft(
                10,
                None,
                vec![message(100, Role::User, vec![text("question")])],
                Vec::new(),
            ),
            expected: ConversationError::Commit(CommitError::InvalidEndState {
                last_role: Some(Role::User),
                has_open_calls: false,
            }),
        },
        Case {
            name: "final assistant still has tool use",
            conversation: conversation(),
            data: draft(
                10,
                None,
                vec![
                    message(100, Role::User, vec![text("question")]),
                    message(101, Role::Assistant, vec![tool_use("open-call")]),
                ],
                Vec::new(),
            ),
            expected: ConversationError::Commit(CommitError::InvalidEndState {
                last_role: Some(Role::Assistant),
                has_open_calls: true,
            }),
        },
        Case {
            name: "system role in history",
            conversation: conversation(),
            data: draft(
                10,
                None,
                vec![
                    message(100, Role::User, vec![text("question")]),
                    message(101, Role::System, vec![text("forbidden")]),
                    message(102, Role::Assistant, vec![text("answer")]),
                ],
                Vec::new(),
            ),
            expected: ConversationError::Commit(CommitError::SystemRole {
                message_id: message_id(101),
            }),
        },
        Case {
            name: "user carries tool use",
            conversation: conversation(),
            data: draft(
                10,
                None,
                vec![
                    message(100, Role::User, vec![tool_use("wrong-role")]),
                    message(101, Role::Assistant, vec![text("answer")]),
                ],
                Vec::new(),
            ),
            expected: ConversationError::Commit(CommitError::InvalidRoleBlock {
                message_id: message_id(100),
                role: Role::User,
                block: ContentBlockKind::ToolUse,
            }),
        },
        Case {
            name: "assistant carries image outside shared adapter subset",
            conversation: conversation(),
            data: draft(
                10,
                None,
                vec![
                    message(100, Role::User, vec![text("question")]),
                    message(101, Role::Assistant, vec![image()]),
                ],
                Vec::new(),
            ),
            expected: ConversationError::Commit(CommitError::InvalidRoleBlock {
                message_id: message_id(101),
                role: Role::Assistant,
                block: ContentBlockKind::Image,
            }),
        },
        Case {
            name: "tool carries unlinked text",
            conversation: conversation(),
            data: draft(
                10,
                None,
                vec![
                    message(100, Role::User, vec![text("question")]),
                    message(101, Role::Assistant, vec![tool_use("call")]),
                    message(102, Role::Tool, vec![text("not wrapped")]),
                    message(103, Role::Assistant, vec![text("answer")]),
                ],
                Vec::new(),
            ),
            expected: ConversationError::Commit(CommitError::InvalidRoleBlock {
                message_id: message_id(102),
                role: Role::Tool,
                block: ContentBlockKind::Text,
            }),
        },
        Case {
            name: "empty tool message",
            conversation: conversation(),
            data: draft(
                10,
                None,
                vec![
                    message(100, Role::User, vec![text("question")]),
                    message(101, Role::Assistant, vec![tool_use("call")]),
                    message(102, Role::Tool, Vec::new()),
                    message(103, Role::Assistant, vec![text("answer")]),
                ],
                Vec::new(),
            ),
            expected: ConversationError::Commit(CommitError::EmptyToolMessage {
                message_id: message_id(102),
            }),
        },
        Case {
            name: "assistant arrives before tools close",
            conversation: conversation(),
            data: draft(
                10,
                None,
                vec![
                    message(100, Role::User, vec![text("question")]),
                    message(101, Role::Assistant, vec![tool_use("call")]),
                    message(102, Role::Assistant, vec![text("too early")]),
                ],
                Vec::new(),
            ),
            expected: ConversationError::Commit(CommitError::UnexpectedRole {
                message_id: message_id(102),
                actual: Role::Assistant,
                expected: "one or more tool messages until every parallel call is answered",
            }),
        },
    ];

    for case in cases {
        assert_rejected_case(case);
    }
}
