use super::{
    assistant_response, begin, call_id, conversation, freeze_response, mapping, message_id,
    push_streamed_tool_response, text, tool_response, turn_id,
};
use crate::{
    conversation::{AssistantFinish, PendingTurnPhase, TurnMeta},
    model::{message::Role, normalized::StopReason, usage::Usage},
};
use serde_json::{Map, json};
use std::collections::HashSet;

#[test]
fn pure_text_turn_commits_atomically_with_response_metadata() {
    let mut conversation = conversation();
    begin(&mut conversation, 10, 100);

    assert_eq!(
        freeze_response(
            &mut conversation,
            assistant_response(vec![text("answer")], 7, 3, StopReason::EndTurn, "req-final"),
            101,
        ),
        AssistantFinish::ReadyToCommit
    );
    assert!(conversation.turns().is_empty());
    assert_eq!(conversation.version(), 0);
    assert_eq!(
        conversation.pending().expect("pending").phase(),
        PendingTurnPhase::ReadyToCommit
    );

    let turn_id = conversation
        .commit_pending(TurnMeta::new(
            Usage {
                reasoning: 2,
                ..Usage::default()
            },
            Some("2026-07-13T08:00:00+08:00".to_owned()),
            Some("unit-test".to_owned()),
            Map::from_iter([("trace".to_owned(), json!("turn-10"))]),
        ))
        .expect("commit final turn");

    assert_eq!(turn_id, super::turn_id(10));
    assert!(conversation.pending().is_none());
    assert_eq!(conversation.version(), 1);
    let turn = &conversation.turns()[0];
    assert_eq!(turn.id(), turn_id);
    assert_eq!(turn.parent(), None);
    assert_eq!(turn.messages().len(), 2);
    assert_eq!(turn.messages()[0].payload().role, Role::User);
    assert_eq!(turn.messages()[1].payload().role, Role::Assistant);
    assert!(turn.pairings().is_empty());
    assert_eq!(
        turn.meta().usage(),
        &Usage {
            input: 7,
            output: 3,
            reasoning: 2,
            ..Usage::default()
        }
    );
    assert_eq!(turn.meta().timestamp(), Some("2026-07-13T08:00:00+08:00"));
    assert_eq!(turn.meta().source(), Some("unit-test"));
    assert_eq!(turn.meta().extra()["trace"], json!("turn-10"));
    assert_eq!(turn.meta().responses().len(), 1);
    assert_eq!(turn.meta().responses()[0].message_id(), message_id(101));
    assert_eq!(
        *turn.meta().responses()[0].stop_reason().value(),
        StopReason::EndTurn
    );
    assert_eq!(
        turn.meta().responses()[0].extra()["request_id"],
        json!("req-final")
    );
}

#[test]
fn two_serial_tool_rounds_mix_complete_and_streaming_responses() {
    let mut conversation = conversation();
    begin(&mut conversation, 20, 200);

    assert_eq!(
        freeze_response(
            &mut conversation,
            assistant_response(
                vec![text("first lookup"), super::tool_use("call-one")],
                10,
                2,
                StopReason::ToolUse,
                "req-one",
            ),
            201,
        ),
        AssistantFinish::RequiresToolCallMappings
    );
    conversation
        .register_tool_calls(vec![mapping("call-one", 500)])
        .expect("map first call");
    assert_eq!(
        conversation
            .append_tool_response(message_id(202), tool_response("call-one", "result one"))
            .expect("append first result"),
        call_id(500)
    );

    push_streamed_tool_response(&mut conversation, "call-two", 20, 3, "req-two");
    assert_eq!(
        conversation
            .finish_assistant(message_id(203))
            .expect("freeze streamed tool call"),
        AssistantFinish::RequiresToolCallMappings
    );
    conversation
        .register_tool_calls(vec![mapping("call-two", 501)])
        .expect("map second call");
    conversation
        .append_tool_response(message_id(204), tool_response("call-two", "result two"))
        .expect("append second result");

    assert_eq!(
        freeze_response(
            &mut conversation,
            assistant_response(vec![text("final")], 30, 4, StopReason::EndTurn, "req-final"),
            205,
        ),
        AssistantFinish::ReadyToCommit
    );

    let pending = conversation.pending().expect("pending turn");
    assert_eq!(pending.messages().len(), 6);
    assert_eq!(pending.tool_calls().len(), 2);
    assert_eq!(pending.open_calls().count(), 0);
    assert_eq!(
        pending.usage(),
        &Usage {
            input: 60,
            output: 9,
            ..Usage::default()
        }
    );
    assert_eq!(pending.responses().len(), 3);
    assert_eq!(
        pending
            .responses()
            .iter()
            .map(|meta| meta.message_id())
            .collect::<Vec<_>>(),
        vec![message_id(201), message_id(203), message_id(205)]
    );

    conversation
        .commit_pending(TurnMeta::default())
        .expect("commit serial tool turn");
    let turn = &conversation.turns()[0];
    assert_eq!(turn.id(), turn_id(20));
    assert_eq!(
        turn.messages()
            .iter()
            .map(|message| message.payload().role)
            .collect::<Vec<_>>(),
        vec![
            Role::User,
            Role::Assistant,
            Role::Tool,
            Role::Assistant,
            Role::Tool,
            Role::Assistant,
        ]
    );
    assert_eq!(turn.pairings().len(), 2);
    assert_eq!(turn.pairings()[0].call_id(), call_id(500));
    assert_eq!(turn.pairings()[0].provider_call_id(), Some("call-one"));
    assert_eq!(turn.pairings()[0].call_msg(), message_id(201));
    assert_eq!(turn.pairings()[0].result_msg(), message_id(202));
    assert_eq!(turn.pairings()[1].call_id(), call_id(501));
    assert_eq!(turn.pairings()[1].provider_call_id(), Some("call-two"));
    assert_eq!(turn.pairings()[1].call_msg(), message_id(203));
    assert_eq!(turn.pairings()[1].result_msg(), message_id(204));
    assert_eq!(turn.meta().responses().len(), 3);
}

#[test]
fn parallel_calls_map_by_provider_id_and_close_in_separate_messages() {
    let mut conversation = conversation();
    begin(&mut conversation, 30, 300);

    freeze_response(
        &mut conversation,
        assistant_response(
            vec![super::tool_use("parallel-a"), super::tool_use("parallel-b")],
            8,
            2,
            StopReason::ToolUse,
            "req-parallel",
        ),
        301,
    );
    assert_eq!(
        conversation
            .pending()
            .expect("pending mappings")
            .unmapped_provider_call_ids(),
        &["parallel-a".to_owned(), "parallel-b".to_owned()]
    );

    conversation
        .register_tool_calls(vec![mapping("parallel-b", 601), mapping("parallel-a", 600)])
        .expect("mapping order is independent of block order");
    let pending = conversation.pending().expect("pending results");
    assert_eq!(
        pending
            .tool_calls()
            .iter()
            .map(|call| (call.provider_call_id(), call.call_id()))
            .collect::<Vec<_>>(),
        vec![("parallel-a", call_id(600)), ("parallel-b", call_id(601))]
    );
    assert_eq!(pending.open_calls().count(), 2);

    conversation
        .append_tool_response(message_id(302), tool_response("parallel-b", "result b"))
        .expect("close second call first");
    assert_eq!(
        conversation.pending().expect("one open call").phase(),
        PendingTurnPhase::AwaitingToolResults
    );
    assert_eq!(
        conversation
            .pending()
            .expect("one open call")
            .open_calls()
            .map(|call| call.provider_call_id())
            .collect::<Vec<_>>(),
        vec!["parallel-a"]
    );

    conversation
        .append_tool_response(message_id(303), tool_response("parallel-a", "result a"))
        .expect("close remaining call");
    assert_eq!(
        conversation.pending().expect("step boundary").phase(),
        PendingTurnPhase::AwaitingAssistant
    );
    freeze_response(
        &mut conversation,
        assistant_response(vec![text("done")], 4, 1, StopReason::EndTurn, "req-done"),
        304,
    );
    conversation
        .commit_pending(TurnMeta::default())
        .expect("commit parallel turn");

    let turn = &conversation.turns()[0];
    assert_eq!(turn.messages().len(), 5);
    assert_eq!(turn.pairings()[0].result_msg(), message_id(303));
    assert_eq!(turn.pairings()[1].result_msg(), message_id(302));
    let message_ids = turn
        .messages()
        .iter()
        .map(|message| message.id())
        .collect::<HashSet<_>>();
    assert_eq!(message_ids.len(), turn.messages().len(), "I4 message ids");
    let call_ids = turn
        .pairings()
        .iter()
        .map(|pairing| pairing.call_id())
        .collect::<HashSet<_>>();
    assert_eq!(call_ids.len(), turn.pairings().len(), "I4 tool call ids");
    assert!(
        turn.pairings()
            .iter()
            .all(|pairing| message_ids.contains(&pairing.call_msg())
                && message_ids.contains(&pairing.result_msg())),
        "I1 pairings are closed inside the turn"
    );
}
