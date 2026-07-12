use super::{Case, assert_rejected_case};
use crate::conversation::validation::tests::fixtures::{conversation, single_tool_draft};
use crate::{
    conversation::{
        CommitError, ConversationError, ConversationMessage,
        turn::{TurnCompletion, TurnData},
    },
    model::content::ContentBlock,
};
use serde_json::json;

#[test]
fn serde_pending_marker_prevents_partial_json_null_from_becoming_closed_content() {
    let mut data = single_tool_draft(10, None, 100, 500, "partial-call");
    let (message_id, mut payload) = data.messages[1].clone().into_parts();
    let ContentBlock::ToolUse { input, .. } = &mut payload.content[1] else {
        unreachable!("fixture contains a tool use")
    };
    *input = serde_json::Value::Null;
    data.messages[1] = ConversationMessage::new(message_id, payload);
    data.completion = TurnCompletion::PendingContent;

    let encoded = serde_json::to_value(&data).expect("serialize pending data");
    assert_eq!(encoded["completion"], json!("pending_content"));
    assert_eq!(
        encoded["messages"][1]["payload"]["content"][1]["input"],
        serde_json::Value::Null
    );
    let decoded = serde_json::from_value::<TurnData>(encoded).expect("deserialize pending data");

    assert_rejected_case(Case {
        name: "serde pending content",
        conversation: conversation(),
        data: decoded,
        expected: ConversationError::Commit(CommitError::IncompleteContent {
            message_id: None,
            detail: "a pending message or content block has not reached its terminal boundary",
        }),
    });
}
