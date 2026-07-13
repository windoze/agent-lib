//! Incremental versus rebuilt indexing across pending and commit transitions.

use super::super::{
    ToolCallLocationKind, assert_index_matches_rebuild, begin, call_id, conversation, freeze,
    message_id, response, text, tool_response, tool_use,
};
use crate::{
    conversation::{AssistantFinish, PendingTurnPhase, ToolCallMapping, TurnMeta},
    model::normalized::StopReason,
};

#[test]
fn incremental_index_matches_rebuild_through_parallel_and_serial_calls() {
    let mut conversation = conversation();
    assert!(conversation.tool_call_index().is_empty());

    begin(&mut conversation, 10, 1_000);
    assert_index_matches_rebuild(&conversation);
    assert_eq!(
        freeze(
            &mut conversation,
            response(
                vec![tool_use("parallel-a"), tool_use("parallel-b")],
                StopReason::ToolUse,
            ),
            1_001,
        ),
        AssistantFinish::RequiresToolCallMappings
    );
    assert_index_matches_rebuild(&conversation);

    let unmapped = conversation.tool_call_index().iter().collect::<Vec<_>>();
    assert_eq!(unmapped.len(), 2);
    assert_eq!(unmapped[0].kind(), ToolCallLocationKind::Pending);
    assert_eq!(unmapped[0].provider_call_id(), "parallel-a");
    assert_eq!(unmapped[1].provider_call_id(), "parallel-b");
    assert!(unmapped.iter().all(|location| location.call_id().is_none()));
    assert!(
        unmapped
            .iter()
            .all(|location| location.call_message_id() == message_id(1_001))
    );

    let index_before_rejected_mapping = conversation.tool_call_index().clone();
    conversation
        .register_tool_calls(vec![ToolCallMapping::new("unknown", call_id(9_999))])
        .expect_err("an inexact mapping must not mutate the transaction or index");
    assert_eq!(
        conversation.tool_call_index(),
        &index_before_rejected_mapping
    );

    conversation
        .register_tool_calls(vec![
            ToolCallMapping::new("parallel-b", call_id(5_001)),
            ToolCallMapping::new("parallel-a", call_id(5_000)),
        ])
        .expect("register exact mappings in caller-independent order");
    assert_index_matches_rebuild(&conversation);
    assert_eq!(
        conversation
            .tool_call_index()
            .by_call_id(call_id(5_000))
            .expect("mapped first call")
            .provider_call_id(),
        "parallel-a"
    );

    conversation
        .append_tool_response(message_id(1_002), tool_response("parallel-b"))
        .expect("parallel results may arrive out of call order");
    assert_index_matches_rebuild(&conversation);
    assert_eq!(
        conversation
            .tool_call_index()
            .by_call_id(call_id(5_001))
            .expect("second call")
            .result_message_id(),
        Some(message_id(1_002))
    );
    assert_eq!(
        conversation
            .tool_call_index()
            .by_call_id(call_id(5_000))
            .expect("first call")
            .result_message_id(),
        None
    );

    conversation
        .append_tool_response(message_id(1_003), tool_response("parallel-a"))
        .expect("close the remaining parallel call");
    assert_index_matches_rebuild(&conversation);
    assert_eq!(
        conversation.pending().expect("pending turn").phase(),
        PendingTurnPhase::AwaitingAssistant
    );

    assert_eq!(
        freeze(
            &mut conversation,
            response(vec![tool_use("serial-c")], StopReason::ToolUse),
            1_004,
        ),
        AssistantFinish::RequiresToolCallMappings
    );
    assert_index_matches_rebuild(&conversation);
    let serial_unmapped = conversation
        .tool_call_index()
        .by_provider_call_id("serial-c")
        .next()
        .expect("new serial call is indexed before mapping");
    assert_eq!(serial_unmapped.call_id(), None);
    assert_eq!(serial_unmapped.call_message_id(), message_id(1_004));

    conversation
        .register_tool_calls(vec![ToolCallMapping::new("serial-c", call_id(5_002))])
        .expect("map the serial call");
    conversation
        .append_tool_response(message_id(1_005), tool_response("serial-c"))
        .expect("close the serial call");
    assert_eq!(
        freeze(
            &mut conversation,
            response(vec![text("complete")], StopReason::EndTurn),
            1_006,
        ),
        AssistantFinish::ReadyToCommit
    );
    assert_index_matches_rebuild(&conversation);
    assert!(
        conversation
            .tool_call_index()
            .iter()
            .all(|location| location.kind() == ToolCallLocationKind::Pending)
    );

    conversation
        .commit_pending(TurnMeta::default())
        .expect("commit the multi-round turn");
    assert_index_matches_rebuild(&conversation);
    let committed = conversation.tool_call_index().iter().collect::<Vec<_>>();
    assert_eq!(committed.len(), 3);
    assert!(
        committed
            .iter()
            .all(|location| location.kind() == ToolCallLocationKind::Committed)
    );
    assert!(
        committed
            .iter()
            .all(|location| location.result_message_id().is_some())
    );
}
