use super::{Case, assert_rejected_case};
use crate::conversation::validation::tests::fixtures::{
    conversation, draft, message, message_id, text, text_draft, tool_result, tool_use,
};
use crate::{
    conversation::{CommitError, ContentBlockKind, ConversationError, turn::TurnCompletion},
    model::{content::ContentBlock, message::Role, tool::ToolStatus},
};
use serde_json::{Map, json};

#[test]
fn duplicate_orphan_dangling_and_incomplete_content_are_rejected_atomically() {
    let mut pending = text_draft(10, None, 100);
    pending.completion = TurnCompletion::PendingContent;

    let unnamed_tool_use = ContentBlock::ToolUse {
        id: "unnamed".to_owned(),
        name: String::new(),
        input: json!({}),
        extra: Map::new(),
    };
    let empty_result_id = ContentBlock::ToolResult {
        tool_use_id: String::new(),
        content: vec![text("result")],
        status: ToolStatus::Ok,
        extra: Map::new(),
    };
    let nested_thinking = ContentBlock::ToolResult {
        tool_use_id: "nested".to_owned(),
        content: vec![ContentBlock::Thinking {
            text: "not tool output".to_owned(),
            signature: None,
            extra: Map::new(),
        }],
        status: ToolStatus::Ok,
        extra: Map::new(),
    };

    let cases = vec![
        Case {
            name: "pending completion marker",
            conversation: conversation(),
            data: pending,
            expected: ConversationError::Commit(CommitError::IncompleteContent {
                message_id: None,
                detail: "a pending message or content block has not reached its terminal boundary",
            }),
        },
        Case {
            name: "empty tool-use provider id",
            conversation: conversation(),
            data: draft(
                10,
                None,
                vec![
                    message(100, Role::User, vec![text("question")]),
                    message(101, Role::Assistant, vec![tool_use("")]),
                ],
                Vec::new(),
            ),
            expected: ConversationError::Commit(CommitError::IncompleteContent {
                message_id: Some(message_id(101)),
                detail: "a tool-use block has no provider call id",
            }),
        },
        Case {
            name: "empty tool name",
            conversation: conversation(),
            data: draft(
                10,
                None,
                vec![
                    message(100, Role::User, vec![text("question")]),
                    message(101, Role::Assistant, vec![unnamed_tool_use]),
                ],
                Vec::new(),
            ),
            expected: ConversationError::Commit(CommitError::IncompleteContent {
                message_id: Some(message_id(101)),
                detail: "a tool-use block has no tool name",
            }),
        },
        Case {
            name: "empty tool-result provider id",
            conversation: conversation(),
            data: draft(
                10,
                None,
                vec![
                    message(100, Role::User, vec![text("question")]),
                    message(101, Role::Assistant, vec![tool_use("call")]),
                    message(102, Role::Tool, vec![empty_result_id]),
                    message(103, Role::Assistant, vec![text("answer")]),
                ],
                Vec::new(),
            ),
            expected: ConversationError::Commit(CommitError::IncompleteContent {
                message_id: Some(message_id(102)),
                detail: "a tool-result block has no provider call id",
            }),
        },
        Case {
            name: "nested thinking in tool output",
            conversation: conversation(),
            data: draft(
                10,
                None,
                vec![
                    message(100, Role::User, vec![text("question")]),
                    message(101, Role::Assistant, vec![tool_use("nested")]),
                    message(102, Role::Tool, vec![nested_thinking]),
                    message(103, Role::Assistant, vec![text("answer")]),
                ],
                Vec::new(),
            ),
            expected: ConversationError::Commit(CommitError::InvalidToolResultContent {
                message_id: message_id(102),
                provider_call_id: "nested".to_owned(),
                block: ContentBlockKind::Thinking,
            }),
        },
        Case {
            name: "duplicate provider tool use",
            conversation: conversation(),
            data: draft(
                10,
                None,
                vec![
                    message(100, Role::User, vec![text("question")]),
                    message(
                        101,
                        Role::Assistant,
                        vec![tool_use("duplicate"), tool_use("duplicate")],
                    ),
                ],
                Vec::new(),
            ),
            expected: ConversationError::Commit(CommitError::DuplicateProviderCallId {
                provider_call_id: "duplicate".to_owned(),
            }),
        },
        Case {
            name: "duplicate tool result consumption",
            conversation: conversation(),
            data: draft(
                10,
                None,
                vec![
                    message(100, Role::User, vec![text("question")]),
                    message(101, Role::Assistant, vec![tool_use("duplicate")]),
                    message(
                        102,
                        Role::Tool,
                        vec![tool_result("duplicate"), tool_result("duplicate")],
                    ),
                    message(103, Role::Assistant, vec![text("answer")]),
                ],
                Vec::new(),
            ),
            expected: ConversationError::Commit(CommitError::DuplicateToolResult {
                provider_call_id: "duplicate".to_owned(),
                first_result_msg: message_id(102),
                duplicate_result_msg: message_id(102),
            }),
        },
        Case {
            name: "duplicate tool result across messages",
            conversation: conversation(),
            data: draft(
                10,
                None,
                vec![
                    message(100, Role::User, vec![text("question")]),
                    message(101, Role::Assistant, vec![tool_use("duplicate")]),
                    message(102, Role::Tool, vec![tool_result("duplicate")]),
                    message(103, Role::Tool, vec![tool_result("duplicate")]),
                    message(104, Role::Assistant, vec![text("answer")]),
                ],
                Vec::new(),
            ),
            expected: ConversationError::Commit(CommitError::DuplicateToolResult {
                provider_call_id: "duplicate".to_owned(),
                first_result_msg: message_id(102),
                duplicate_result_msg: message_id(103),
            }),
        },
        Case {
            name: "orphan provider result",
            conversation: conversation(),
            data: draft(
                10,
                None,
                vec![
                    message(100, Role::User, vec![text("question")]),
                    message(101, Role::Assistant, vec![tool_use("expected")]),
                    message(102, Role::Tool, vec![tool_result("orphan")]),
                    message(103, Role::Assistant, vec![text("answer")]),
                ],
                Vec::new(),
            ),
            expected: ConversationError::Commit(CommitError::OrphanToolResult {
                provider_call_id: "orphan".to_owned(),
                result_msg: message_id(102),
            }),
        },
        Case {
            name: "orphan provider result before any call",
            conversation: conversation(),
            data: draft(
                10,
                None,
                vec![
                    message(100, Role::User, vec![text("question")]),
                    message(101, Role::Tool, vec![tool_result("orphan")]),
                    message(102, Role::Assistant, vec![text("answer")]),
                ],
                Vec::new(),
            ),
            expected: ConversationError::Commit(CommitError::OrphanToolResult {
                provider_call_id: "orphan".to_owned(),
                result_msg: message_id(101),
            }),
        },
        Case {
            name: "unfinished parallel provider call",
            conversation: conversation(),
            data: draft(
                10,
                None,
                vec![
                    message(100, Role::User, vec![text("question")]),
                    message(
                        101,
                        Role::Assistant,
                        vec![tool_use("closed"), tool_use("dangling")],
                    ),
                    message(102, Role::Tool, vec![tool_result("closed")]),
                ],
                Vec::new(),
            ),
            expected: ConversationError::Commit(CommitError::DanglingProviderCall {
                provider_call_id: "dangling".to_owned(),
                call_msg: message_id(101),
            }),
        },
    ];

    for case in cases {
        assert_rejected_case(case);
    }
}
