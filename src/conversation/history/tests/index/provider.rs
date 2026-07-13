//! Provider-id multiplicity and optional persisted-id resolution tests.

use super::{
    super::{ToolCallLocationKind, assert_index_matches_rebuild, call_id, conversation},
    commit_one_call_turn,
};

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
