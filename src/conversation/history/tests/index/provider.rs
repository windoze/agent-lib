//! Provider-id multiplicity and optional persisted-id resolution tests.

use super::{
    super::{
        ToolCallLocationKind, assert_index_matches_rebuild, call_id, conversation, message_id,
        text, tool_use, turn_id,
    },
    commit_one_call_turn,
};
use crate::{
    conversation::{
        ConversationMessage, TurnMeta,
        turn::{ToolPairingData, TurnCompletion, TurnData},
    },
    model::{
        content::ContentBlock,
        message::{Message, Role},
        tool::ToolStatus,
    },
};
use serde_json::Map;

/// Creates one complete tool-result block anchored to a provider call id.
fn tool_result(provider_call_id: &str) -> ContentBlock {
    ContentBlock::ToolResult {
        tool_use_id: provider_call_id.to_owned(),
        content: vec![text(format!("result:{provider_call_id}"))],
        status: ToolStatus::Ok,
        extra: Map::new(),
    }
}

/// Freezes one complete test message under a deterministic external id.
fn message(seed: u128, role: Role, content: Vec<ContentBlock>) -> ConversationMessage {
    ConversationMessage::new(message_id(seed), Message { role, content })
}

#[test]
fn provider_lookup_returns_repeated_ids_in_current_lineage_order() {
    let mut conversation = conversation();
    commit_one_call_turn(&mut conversation, 20, 2_000, "reused-provider-id", 6_000);
    commit_one_call_turn(&mut conversation, 21, 2_100, "reused-provider-id", 6_001);

    assert_index_matches_rebuild(&conversation);
    let framework_ids = conversation
        .tool_call_index()
        .by_provider_call_id("reused-provider-id")
        .map(|location| location.call_id().expect("committed call is mapped"))
        .collect::<Vec<_>>();
    assert_eq!(framework_ids, vec![call_id(6_000), call_id(6_001)]);
}

#[test]
fn rebuild_resolves_a_validated_pairing_without_persisted_provider_id() {
    let mut source = conversation();
    commit_one_call_turn(&mut source, 25, 2_500, "anchored-provider-id", 6_500);

    let mut encoded = serde_json::to_value(&source.turns()[0]).expect("serialize closed turn");
    encoded["pairings"][0]["provider_call_id"] = serde_json::Value::Null;
    let data: crate::conversation::turn::TurnData =
        serde_json::from_value(encoded).expect("decode validator DTO");

    let mut restored = conversation();
    restored
        .commit_draft(data)
        .expect("the message anchors uniquely recover the provider id");
    assert_eq!(restored.turns()[0].pairings()[0].provider_call_id(), None);
    assert_index_matches_rebuild(&restored);

    let location = restored
        .tool_call_index()
        .by_provider_call_id("anchored-provider-id")
        .next()
        .expect("derived lookup resolves the certified content anchor");
    assert_eq!(location.call_id(), Some(call_id(6_500)));
    assert_eq!(location.kind(), ToolCallLocationKind::Committed);
}

#[test]
fn rebuild_replays_the_validators_claimed_exclusion_for_optional_ids() {
    // One assistant message anchors tool uses `claimed-b` and `derived-a` (in
    // this content order) and one tool message anchors both results. The
    // explicit pairing claims `claimed-b` first, so the validator resolves the
    // `None` pairing to `derived-a` even though `claimed-b` precedes it in
    // content order. The index must replay the same claimed-exclusion rule:
    // resolving per pairing in content order would hand the `None` pairing
    // the already-claimed `claimed-b` (and trip the uniqueness debug assert).
    let data = TurnData {
        id: turn_id(30),
        messages: vec![
            message(3_000, Role::User, vec![text("question")]),
            message(
                3_001,
                Role::Assistant,
                vec![tool_use("claimed-b"), tool_use("derived-a")],
            ),
            message(
                3_002,
                Role::Tool,
                vec![tool_result("claimed-b"), tool_result("derived-a")],
            ),
            message(3_003, Role::Assistant, vec![text("final")]),
        ],
        pairings: vec![
            ToolPairingData {
                call_id: call_id(7_000),
                provider_call_id: Some("claimed-b".to_owned()),
                call_msg: message_id(3_001),
                result_msg: Some(message_id(3_002)),
            },
            ToolPairingData {
                call_id: call_id(7_001),
                provider_call_id: None,
                call_msg: message_id(3_001),
                result_msg: Some(message_id(3_002)),
            },
        ],
        parent: None,
        meta: TurnMeta::default(),
        completion: TurnCompletion::Complete,
    };

    let mut restored = conversation();
    restored
        .commit_draft(data)
        .expect("the validator resolves the None pairing to the unclaimed id");
    assert_eq!(restored.turns()[0].pairings()[1].provider_call_id(), None);
    assert_index_matches_rebuild(&restored);

    let claimed = restored
        .tool_call_index()
        .by_provider_call_id("claimed-b")
        .next()
        .expect("the explicit pairing keeps its provider id");
    assert_eq!(claimed.call_id(), Some(call_id(7_000)));
    let derived = restored
        .tool_call_index()
        .by_provider_call_id("derived-a")
        .next()
        .expect("the None pairing resolves to the unclaimed candidate");
    assert_eq!(derived.call_id(), Some(call_id(7_001)));
    assert_eq!(derived.kind(), ToolCallLocationKind::Committed);
}
