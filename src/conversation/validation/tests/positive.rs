use super::fixtures::{
    assert_closed_invariants, conversation, draft, image, message, pairing, single_tool_draft,
    text, text_draft, tool_result, tool_use, turn_id,
};
use crate::{
    conversation::{ConversationMessage, turn::TurnData},
    model::content::ContentBlock,
    model::message::Role,
};
use serde_json::json;

#[test]
fn pure_text_turn_commits_with_all_invariants() {
    let mut conversation = conversation();
    let expected_id = turn_id(10);

    let committed_id = conversation
        .commit_draft(text_draft(10, None, 100))
        .expect("commit pure text turn");

    assert_eq!(committed_id, expected_id);
    assert_eq!(conversation.version(), 1);
    assert_eq!(conversation.turns().len(), 1);
    assert_eq!(conversation.turns()[0].parent(), None);
    assert_closed_invariants(&conversation.turns()[0]);
}

#[test]
fn assistant_unknown_content_block_commits_as_provider_evidence() {
    let mut conversation = conversation();
    let raw = json!({ "type": "future_block", "payload": { "kept": true } });
    let data = draft(
        10,
        None,
        vec![
            message(100, Role::User, vec![text("question")]),
            message(
                101,
                Role::Assistant,
                vec![ContentBlock::Unknown {
                    type_name: Some("future_block".to_owned()),
                    raw: raw.clone(),
                }],
            ),
        ],
        Vec::new(),
    );

    conversation
        .commit_draft(data)
        .expect("assistant unknown block should be retained");

    assert_eq!(
        conversation.turns()[0].messages()[1].payload().content[0],
        ContentBlock::Unknown {
            type_name: Some("future_block".to_owned()),
            raw,
        }
    );
}

#[test]
fn single_tool_round_trip_commits_with_explicit_pairing() {
    let mut conversation = conversation();

    conversation
        .commit_draft(single_tool_draft(10, None, 100, 500, "call-one"))
        .expect("commit single tool turn");

    let turn = &conversation.turns()[0];
    assert_eq!(turn.messages().len(), 4);
    assert_eq!(turn.pairings().len(), 1);
    assert_eq!(turn.pairings()[0].provider_call_id(), Some("call-one"));
    assert_closed_invariants(turn);
}

#[test]
fn serial_tool_round_trips_stay_inside_one_closed_turn() {
    let mut conversation = conversation();
    let data = draft(
        10,
        None,
        vec![
            message(100, Role::User, vec![text("compare")]),
            message(101, Role::Assistant, vec![tool_use("serial-a")]),
            message(102, Role::Tool, vec![tool_result("serial-a")]),
            message(
                103,
                Role::Assistant,
                vec![text("one more lookup"), tool_use("serial-b")],
            ),
            message(104, Role::Tool, vec![tool_result("serial-b")]),
            message(105, Role::Assistant, vec![text("combined answer")]),
        ],
        vec![
            pairing(500, "serial-a", 101, 102),
            pairing(501, "serial-b", 103, 104),
        ],
    );

    conversation
        .commit_draft(data)
        .expect("commit serial tool rounds");

    let turn = &conversation.turns()[0];
    assert_eq!(turn.messages().len(), 6);
    assert_eq!(turn.pairings().len(), 2);
    assert_closed_invariants(turn);
}

#[test]
fn parallel_calls_may_close_across_multiple_tool_messages() {
    let mut conversation = conversation();
    let data = draft(
        10,
        None,
        vec![
            message(100, Role::User, vec![text("parallel lookup"), image()]),
            message(
                101,
                Role::Assistant,
                vec![tool_use("parallel-a"), tool_use("parallel-b")],
            ),
            message(102, Role::Tool, vec![tool_result("parallel-a")]),
            message(103, Role::Tool, vec![tool_result("parallel-b")]),
            message(104, Role::Assistant, vec![text("parallel answer")]),
        ],
        vec![
            pairing(500, "parallel-a", 101, 102),
            pairing(501, "parallel-b", 101, 103),
        ],
    );

    conversation
        .commit_draft(data)
        .expect("commit parallel tool round");

    let turn = &conversation.turns()[0];
    assert_eq!(turn.pairings()[0].call_msg(), turn.pairings()[1].call_msg());
    assert_ne!(
        turn.pairings()[0].result_msg(),
        turn.pairings()[1].result_msg()
    );
    assert_closed_invariants(turn);
}

#[test]
fn optional_provider_ids_are_inferred_only_from_unambiguous_message_anchors() {
    let mut conversation = conversation();
    let mut data = draft(
        10,
        None,
        vec![
            message(100, Role::User, vec![text("parallel lookup")]),
            message(
                101,
                Role::Assistant,
                vec![tool_use("optional-a"), tool_use("optional-b")],
            ),
            message(102, Role::Tool, vec![tool_result("optional-a")]),
            message(103, Role::Tool, vec![tool_result("optional-b")]),
            message(104, Role::Assistant, vec![text("answer")]),
        ],
        vec![
            pairing(500, "optional-a", 101, 102),
            pairing(501, "optional-b", 101, 103),
        ],
    );
    for pairing in &mut data.pairings {
        pairing.provider_call_id = None;
    }

    conversation
        .commit_draft(data)
        .expect("distinct result messages disambiguate optional provider ids");

    let turn = &conversation.turns()[0];
    assert_eq!(turn.pairings()[0].provider_call_id(), None);
    assert_eq!(turn.pairings()[1].provider_call_id(), None);
    assert_closed_invariants(turn);
}

#[test]
fn sequential_commits_set_parent_and_advance_version_without_reidentifying_history() {
    let mut conversation = conversation();
    let first_id = conversation
        .commit_draft(text_draft(10, None, 100))
        .expect("commit first turn");
    let first_messages = conversation.turns()[0]
        .messages()
        .iter()
        .map(crate::conversation::ConversationMessage::id)
        .collect::<Vec<_>>();

    let second_id = conversation
        .commit_draft(text_draft(11, Some(first_id), 200))
        .expect("commit second turn");

    assert_eq!(conversation.version(), 2);
    assert_eq!(conversation.turns()[1].id(), second_id);
    assert_eq!(conversation.turns()[1].parent(), Some(first_id));
    assert_eq!(
        conversation.turns()[0]
            .messages()
            .iter()
            .map(crate::conversation::ConversationMessage::id)
            .collect::<Vec<_>>(),
        first_messages
    );
    for turn in conversation.turns() {
        assert_closed_invariants(turn);
    }
}

#[test]
fn deserialized_turn_data_uses_the_same_validator_and_stable_live_shape() {
    let data = single_tool_draft(10, None, 100, 500, "serde-call");
    let encoded = serde_json::to_value(&data).expect("serialize turn data");
    assert!(encoded.get("completion").is_none());
    let decoded = serde_json::from_value::<TurnData>(encoded.clone())
        .expect("deserialize untrusted turn data");
    let mut conversation = conversation();

    conversation
        .commit_draft(decoded)
        .expect("validate deserialized turn data");

    let turn = &conversation.turns()[0];
    assert_closed_invariants(turn);
    assert_eq!(
        serde_json::to_value(turn).expect("serialize live turn"),
        encoded
    );
}

#[test]
fn complete_json_null_is_valid_when_no_pending_marker_exists() {
    let mut data = single_tool_draft(10, None, 100, 500, "null-input");
    let (message_id, mut payload) = data.messages[1].clone().into_parts();
    let ContentBlock::ToolUse { input, .. } = &mut payload.content[1] else {
        unreachable!("fixture contains a tool use")
    };
    *input = serde_json::Value::Null;
    data.messages[1] = ConversationMessage::new(message_id, payload);
    let mut conversation = conversation();

    conversation
        .commit_draft(data)
        .expect("JSON null is a complete parsed value");

    assert_closed_invariants(&conversation.turns()[0]);
}
