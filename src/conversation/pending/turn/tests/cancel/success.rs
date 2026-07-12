use super::*;

#[test]
fn discard_drops_partial_text_and_preserves_committed_history_before_next_feed() {
    let mut conversation = conversation();
    commit_text_turn(&mut conversation, 10, 100, 101);
    let committed_before = committed_view(&conversation);

    begin(&mut conversation, 11, 110);
    conversation
        .start_assistant()
        .expect("start partial text response");
    let block_id = BlockId::new("discarded-text");
    for event in [
        StreamEvent::MessageStart {
            role: Role::Assistant,
        },
        StreamEvent::BlockStart {
            id: block_id.clone(),
            kind: BlockKind::Text,
        },
        StreamEvent::BlockDelta {
            id: block_id,
            delta: Delta::Text("never frozen".to_owned()),
        },
    ] {
        conversation
            .push_assistant_event(event)
            .expect("accumulate cancellable text");
    }

    assert_eq!(
        conversation
            .cancel_pending(CancelDisposition::DiscardTurn)
            .expect("discard partial turn"),
        CancelOutcome::Discarded {
            turn_id: turn_id(11),
        }
    );
    assert!(conversation.pending().is_none());
    assert_eq!(committed_view(&conversation), committed_before);

    commit_text_turn(&mut conversation, 12, 120, 121);
    assert_eq!(conversation.turns().len(), 2);
    assert_eq!(conversation.turns()[1].parent(), Some(turn_id(10)));
}

#[test]
fn resume_drops_three_fragment_partial_tool_json_without_freezing_it() {
    let mut conversation = conversation();
    begin(&mut conversation, 20, 200);
    conversation
        .start_assistant()
        .expect("start partial tool response");
    let block_id = BlockId::new("partial-tool-json");
    for event in [
        StreamEvent::MessageStart {
            role: Role::Assistant,
        },
        StreamEvent::BlockStart {
            id: block_id.clone(),
            kind: BlockKind::ToolInput {
                tool_name: "lookup".to_owned(),
                tool_call_id: "partial-call".to_owned(),
            },
        },
        StreamEvent::BlockDelta {
            id: block_id.clone(),
            delta: Delta::Json("{\"query\"".to_owned()),
        },
        StreamEvent::BlockDelta {
            id: block_id.clone(),
            delta: Delta::Json(":\"Shang".to_owned()),
        },
        StreamEvent::BlockDelta {
            id: block_id,
            delta: Delta::Json("hai".to_owned()),
        },
    ] {
        conversation
            .push_assistant_event(event)
            .expect("accumulate incomplete tool JSON");
    }

    assert_eq!(
        conversation
            .cancel_pending(CancelDisposition::ResumeTurn {
                cancelled_results: Vec::new(),
            })
            .expect("resume after dropping incomplete tool JSON"),
        CancelOutcome::Resumed {
            turn_id: turn_id(20),
        }
    );
    let pending = conversation.pending().expect("resumed pending turn");
    assert_eq!(pending.phase(), PendingTurnPhase::AwaitingAssistant);
    assert_eq!(
        pending.messages().len(),
        1,
        "partial never became a message"
    );
    assert!(pending.tool_calls().is_empty());

    freeze_response(
        &mut conversation,
        assistant_response(
            vec![text("replacement")],
            2,
            1,
            StopReason::EndTurn,
            "req-replacement",
        ),
        201,
    );
    conversation
        .commit_pending(TurnMeta::default())
        .expect("commit resumed turn");
    assert_eq!(conversation.turns()[0].messages().len(), 2);
    assert!(conversation.turns()[0].pairings().is_empty());

    commit_text_turn(&mut conversation, 21, 210, 211);
}

#[test]
fn resume_maps_unregistered_parallel_calls_and_persists_cancelled_results() {
    let mut conversation = conversation();
    begin(&mut conversation, 30, 300);
    freeze_response(
        &mut conversation,
        assistant_response(
            vec![tool_use("parallel-a"), tool_use("parallel-b")],
            8,
            2,
            StopReason::ToolUse,
            "req-parallel",
        ),
        301,
    );
    assert_eq!(
        conversation.pending().expect("unmapped calls").phase(),
        PendingTurnPhase::AwaitingToolCallMappings
    );

    assert_eq!(
        conversation
            .cancel_pending(CancelDisposition::ResumeTurn {
                cancelled_results: vec![
                    cancelled_result("parallel-b", 601, 303),
                    cancelled_result("parallel-a", 600, 302),
                ],
            })
            .expect("cancel and map frozen calls"),
        CancelOutcome::Resumed {
            turn_id: turn_id(30),
        }
    );
    let pending = conversation.pending().expect("coherent resumed turn");
    assert_eq!(pending.phase(), PendingTurnPhase::AwaitingAssistant);
    assert_eq!(pending.messages().len(), 4);
    assert_eq!(pending.open_calls().count(), 0);
    assert_cancelled_message(&pending.messages()[2], "parallel-a");
    assert_cancelled_message(&pending.messages()[3], "parallel-b");
    assert_eq!(pending.tool_calls()[0].call_id(), call_id(600));
    assert_eq!(
        pending.tool_calls()[0].result_message_id(),
        Some(message_id(302))
    );
    assert_eq!(pending.tool_calls()[1].call_id(), call_id(601));
    assert_eq!(
        pending.tool_calls()[1].result_message_id(),
        Some(message_id(303))
    );

    freeze_response(
        &mut conversation,
        assistant_response(
            vec![text("interrupted safely")],
            3,
            1,
            StopReason::EndTurn,
            "req-after-cancel",
        ),
        304,
    );
    conversation
        .commit_pending(TurnMeta::default())
        .expect("commit cancelled tool turn");
    let turn = &conversation.turns()[0];
    assert_eq!(turn.pairings().len(), 2);
    assert_eq!(turn.pairings()[0].result_msg(), message_id(302));
    assert_eq!(turn.pairings()[1].result_msg(), message_id(303));
    assert_cancelled_message(&turn.messages()[2], "parallel-a");
    assert_cancelled_message(&turn.messages()[3], "parallel-b");

    commit_text_turn(&mut conversation, 31, 310, 311);
}

#[test]
fn resume_cancels_only_calls_still_open_during_parallel_tool_execution() {
    let mut conversation = conversation();
    begin(&mut conversation, 40, 400);
    freeze_response(
        &mut conversation,
        assistant_response(
            vec![tool_use("open-a"), tool_use("done-b")],
            4,
            2,
            StopReason::ToolUse,
            "req-tools",
        ),
        401,
    );
    conversation
        .register_tool_calls(vec![mapping("open-a", 700), mapping("done-b", 701)])
        .expect("map parallel calls");
    conversation
        .append_tool_response(message_id(402), tool_response("done-b", "completed"))
        .expect("complete one call normally");

    conversation
        .cancel_pending(CancelDisposition::ResumeTurn {
            cancelled_results: vec![cancelled_result("open-a", 700, 403)],
        })
        .expect("cancel only remaining call");
    let pending = conversation
        .pending()
        .expect("resumed after tool execution");
    assert_eq!(pending.phase(), PendingTurnPhase::AwaitingAssistant);
    assert_eq!(pending.open_calls().count(), 0);
    assert_eq!(pending.messages().len(), 4);
    let ContentBlock::ToolResult { status, .. } = &pending.messages()[2].payload().content[0]
    else {
        panic!("expected completed tool result");
    };
    assert_eq!(*status, ToolStatus::Ok);
    assert_cancelled_message(&pending.messages()[3], "open-a");

    freeze_response(
        &mut conversation,
        assistant_response(
            vec![text("parallel batch closed")],
            2,
            1,
            StopReason::EndTurn,
            "req-final",
        ),
        404,
    );
    conversation
        .commit_pending(TurnMeta::default())
        .expect("commit mixed normal/cancelled results");
    assert_eq!(
        conversation.turns()[0].pairings()[0].result_msg(),
        message_id(403)
    );
    assert_eq!(
        conversation.turns()[0].pairings()[1].result_msg(),
        message_id(402)
    );
}

#[test]
fn commit_disposition_closes_parallel_calls_and_can_begin_another_turn() {
    let mut conversation = conversation();
    begin(&mut conversation, 50, 500);
    freeze_response(
        &mut conversation,
        assistant_response(
            vec![tool_use("commit-a"), tool_use("commit-b")],
            5,
            2,
            StopReason::ToolUse,
            "req-open",
        ),
        501,
    );
    conversation
        .register_tool_calls(vec![mapping("commit-a", 800), mapping("commit-b", 801)])
        .expect("map commit calls");

    assert_eq!(
        conversation
            .cancel_pending(CancelDisposition::commit_turn(
                vec![
                    cancelled_result("commit-b", 801, 503),
                    cancelled_result("commit-a", 800, 502),
                ],
                message_id(504),
                assistant_response(
                    vec![text("cancelled and closed")],
                    3,
                    1,
                    StopReason::EndTurn,
                    "req-cancel-final",
                ),
                TurnMeta::default(),
            ))
            .expect("cancel and commit atomically"),
        CancelOutcome::Committed {
            turn_id: turn_id(50),
        }
    );
    assert!(conversation.pending().is_none());
    assert_eq!(conversation.version(), 1);
    let turn = &conversation.turns()[0];
    assert_eq!(turn.messages().len(), 5);
    assert_eq!(turn.meta().usage().input, 8);
    assert_eq!(turn.meta().usage().output, 3);
    assert_eq!(turn.meta().responses().len(), 2);
    assert_cancelled_message(&turn.messages()[2], "commit-a");
    assert_cancelled_message(&turn.messages()[3], "commit-b");
    assert_eq!(turn.messages()[4].id(), message_id(504));

    commit_text_turn(&mut conversation, 51, 510, 511);
    assert_eq!(conversation.turns()[1].parent(), Some(turn_id(50)));
}

#[test]
fn commit_disposition_replaces_an_active_partial_without_parsing_or_retaining_it() {
    let mut conversation = conversation();
    begin(&mut conversation, 60, 600);
    conversation
        .start_assistant()
        .expect("start replacement candidate");
    let block_id = BlockId::new("abandoned-reasoning");
    for event in [
        StreamEvent::MessageStart {
            role: Role::Assistant,
        },
        StreamEvent::BlockStart {
            id: block_id.clone(),
            kind: BlockKind::Reasoning,
        },
        StreamEvent::BlockDelta {
            id: block_id,
            delta: Delta::Reasoning("discard me".to_owned()),
        },
    ] {
        conversation
            .push_assistant_event(event)
            .expect("accumulate partial reasoning");
    }

    conversation
        .cancel_pending(CancelDisposition::commit_turn(
            Vec::new(),
            message_id(601),
            assistant_response(
                vec![text("safe terminal response")],
                2,
                1,
                StopReason::EndTurn,
                "req-safe-terminal",
            ),
            TurnMeta::default(),
        ))
        .expect("replace partial with complete final assistant");
    let turn = &conversation.turns()[0];
    assert_eq!(turn.messages().len(), 2);
    assert_eq!(
        turn.messages()[1].payload().content,
        vec![text("safe terminal response")]
    );
    assert_eq!(turn.meta().responses().len(), 1);
}
