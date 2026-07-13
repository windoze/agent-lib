//! Index synchronization across cancellation dispositions.

use super::super::{
    ToolCallLocationKind, assert_index_matches_rebuild, begin, call_id, conversation, freeze,
    message_id, response, text, tool_use, turn_id,
};
use crate::{
    conversation::{
        AssistantFinish, CancelDisposition, CancelOutcome, CancelledToolResult, TurnMeta,
    },
    model::normalized::StopReason,
};

#[test]
fn cancel_resume_commit_and_discard_keep_the_derived_suffix_synchronized() {
    let mut conversation = conversation();
    begin(&mut conversation, 30, 3_000);
    assert_eq!(
        freeze(
            &mut conversation,
            response(vec![tool_use("cancelled-call")], StopReason::ToolUse),
            3_001,
        ),
        AssistantFinish::RequiresToolCallMappings
    );
    assert_eq!(conversation.tool_call_index().len(), 1);
    assert_eq!(
        conversation
            .tool_call_index()
            .iter()
            .next()
            .expect("unmapped pending call")
            .call_id(),
        None
    );

    assert_eq!(
        conversation
            .cancel_pending(CancelDisposition::ResumeTurn {
                cancelled_results: vec![CancelledToolResult::new(
                    "cancelled-call",
                    call_id(7_000),
                    message_id(3_002),
                )],
            })
            .expect("cancel and resume"),
        CancelOutcome::Resumed {
            turn_id: turn_id(30),
        }
    );
    assert_index_matches_rebuild(&conversation);
    let resumed = conversation
        .tool_call_index()
        .by_call_id(call_id(7_000))
        .expect("synthetically closed call");
    assert_eq!(resumed.kind(), ToolCallLocationKind::Pending);
    assert_eq!(resumed.result_message_id(), Some(message_id(3_002)));

    assert_eq!(
        conversation
            .cancel_pending(CancelDisposition::commit_turn(
                Vec::new(),
                message_id(3_003),
                response(vec![text("cancelled cleanly")], StopReason::EndTurn),
                TurnMeta::default(),
            ))
            .expect("commit the coherent cancelled turn"),
        CancelOutcome::Committed {
            turn_id: turn_id(30),
        }
    );
    assert_index_matches_rebuild(&conversation);
    assert_eq!(
        conversation
            .tool_call_index()
            .by_call_id(call_id(7_000))
            .expect("committed cancelled call")
            .kind(),
        ToolCallLocationKind::Committed
    );

    begin(&mut conversation, 31, 3_100);
    assert_eq!(
        freeze(
            &mut conversation,
            response(vec![tool_use("discarded-call")], StopReason::ToolUse),
            3_101,
        ),
        AssistantFinish::RequiresToolCallMappings
    );
    assert_eq!(conversation.tool_call_index().len(), 2);
    conversation
        .cancel_pending(CancelDisposition::DiscardTurn)
        .expect("discard the second transaction");
    assert_index_matches_rebuild(&conversation);
    assert_eq!(conversation.tool_call_index().len(), 1);
    assert!(
        conversation
            .tool_call_index()
            .by_provider_call_id("discarded-call")
            .next()
            .is_none()
    );
}
