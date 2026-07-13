//! Public-API state-machine acceptance tests for Conversation Core.

#[path = "conversation_state_machine/support.rs"]
mod support;

use agent_lib::{
    conversation::{
        AssistantFinish, CancelDisposition, CancelOutcome, CancelledToolResult, Conversation,
        ConversationError, ConversationSnapshot, PendingTurnPhase, Span, ToolCallLocationKind,
        ToolCallMapping,
    },
    model::{content::ContentBlock, normalized::StopReason, tool::ToolStatus},
};
use serde_json::json;
use support::*;

#[test]
fn parallel_tool_cancel_resume_keeps_state_machine_usable() {
    let mut conversation = conversation(1);
    commit_text_turn(&mut conversation, 101, "warmup");
    assert_state_machine_invariants("after warmup", &conversation);

    let raw_before_pending = raw_snapshots(&conversation);
    begin(&mut conversation, 102, "parallel-cancel");
    stream_parallel_tool_uses(&mut conversation, 10_201, "call-left", "call-right");
    assert_state_machine_invariants("after streamed parallel freeze", &conversation);
    assert_eq!(
        conversation.pending().expect("pending").phase(),
        PendingTurnPhase::AwaitingToolCallMappings
    );

    conversation
        .register_tool_calls(vec![
            ToolCallMapping::new("call-left", call_id(10_202)),
            ToolCallMapping::new("call-right", call_id(10_203)),
        ])
        .expect("register parallel mappings");
    assert_state_machine_invariants("after mapping parallel calls", &conversation);
    assert_eq!(
        conversation
            .tool_call_index()
            .by_provider_call_id("call-right")
            .next()
            .expect("pending right call")
            .kind(),
        ToolCallLocationKind::Pending
    );

    conversation
        .append_tool_response(
            message_id(10_204),
            tool_response("call-left", "left completed", ToolStatus::Ok),
        )
        .expect("append left tool result");
    assert_state_machine_invariants("after one parallel result", &conversation);

    assert_eq!(
        conversation
            .cancel_pending(CancelDisposition::ResumeTurn {
                cancelled_results: vec![CancelledToolResult::new(
                    "call-right",
                    call_id(10_203),
                    message_id(10_205),
                )],
            })
            .expect("resume after cancelling open parallel call"),
        CancelOutcome::Resumed {
            turn_id: turn_id(102),
        }
    );
    assert_state_machine_invariants("after cancel resume", &conversation);
    assert_previous_raw_snapshots_unchanged(
        "cancel resume must not mutate committed history",
        &raw_before_pending,
        &conversation,
    );
    assert!(
        conversation
            .pending_context()
            .expect("pending context")
            .messages()
            .iter()
            .flat_map(|message| &message.content)
            .any(|block| matches!(
                block,
                ContentBlock::ToolResult {
                    tool_use_id,
                    status: ToolStatus::Cancelled,
                    ..
                } if tool_use_id == "call-right"
            )),
        "cancel resume must preserve a cancelled tool result in pending context"
    );

    assert_eq!(
        finish_complete_response(
            &mut conversation,
            assistant_response(
                vec![text("assistant:final-after-cancel")],
                usage(4, 2),
                StopReason::EndTurn,
                "final-after-cancel",
            ),
            10_206,
        ),
        AssistantFinish::ReadyToCommit
    );
    conversation
        .commit_pending(explicit_meta(102, "parallel-cancel"))
        .expect("commit resumed cancelled turn");
    assert_state_machine_invariants("after committing resumed cancel turn", &conversation);

    let committed_cancelled = conversation
        .raw_turn(turn_id(102))
        .expect("cancelled turn committed")
        .messages()
        .iter()
        .flat_map(|message| message.payload().content.iter())
        .any(|block| {
            matches!(
                block,
                ContentBlock::ToolResult {
                    tool_use_id,
                    status: ToolStatus::Cancelled,
                    ..
                } if tool_use_id == "call-right"
            )
        });
    assert!(
        committed_cancelled,
        "committed history must retain ToolStatus::Cancelled"
    );
    assert_eq!(
        conversation
            .tool_call_index()
            .by_call_id(call_id(10_203))
            .expect("cancelled call in committed index")
            .kind(),
        ToolCallLocationKind::Committed
    );

    snapshot_restore_via_json_and_rows("parallel cancel committed", &conversation);
    assert_can_commit_followup("post-cancel followup", &mut conversation, 103);
}

#[test]
fn compacted_revert_fork_parent_child_restore_matrix_stays_isolated() {
    let mut parent = conversation(2);
    commit_text_turn(&mut parent, 201, "alpha");
    commit_tool_turn(&mut parent, 202, "call-parent-tool", 20_200, "beta-tool");
    commit_text_turn(&mut parent, 203, "gamma");
    commit_text_turn(&mut parent, 204, "delta");
    commit_text_turn(&mut parent, 205, "epsilon");
    assert_state_machine_invariants("parent before compaction", &parent);
    let raw_before_compaction = raw_snapshots(&parent);

    apply_raw_compaction(&mut parent, 0, 2, 20_300, "tier-a", "summary:201-202");
    apply_raw_compaction(&mut parent, 2, 4, 20_301, "tier-b", "summary:203-204");
    apply_span_compaction(
        &mut parent,
        0,
        4,
        20_302,
        "consolidated",
        "summary:201-204-consolidated",
    );
    assert_state_machine_invariants("parent after consolidated compaction", &parent);
    assert_previous_raw_snapshots_unchanged(
        "compaction must not rewrite raw facts",
        &raw_before_compaction,
        &parent,
    );
    assert!(matches!(
        parent.projection().spans().first(),
        Some(Span::Compacted { .. })
    ));
    assert!(
        text_values(&parent)
            .iter()
            .any(|text| text == "summary:201-204-consolidated")
    );
    snapshot_restore_via_json_and_rows("parent after compaction", &parent);

    let redo = parent
        .revert_to(parent.valid_boundaries()[2])
        .expect("revert into compacted cover")
        .old_head();
    assert_state_machine_invariants("parent reverted inside compacted cover", &parent);
    let reverted_texts = text_values(&parent);
    assert!(
        reverted_texts.iter().all(|text| !text.contains("summary")
            && !text.contains("203")
            && !text.contains("204")),
        "reverted view must not leak future summary or future turns: {reverted_texts:?}"
    );

    let mut child = parent
        .fork_at(parent.head(), conversation_id(20_900))
        .expect("fork from reverted compacted cover prefix");
    assert_state_machine_invariants("child at fork point", &child);
    assert_eq!(
        child
            .origin()
            .expect("child origin")
            .fork_point()
            .turn_count(),
        2
    );
    assert_eq!(
        child.projection().artifacts().len(),
        0,
        "child forked inside a cover should fall back to raw prefix"
    );
    assert!(
        text_values(&child)
            .iter()
            .all(|text| !text.contains("summary")
                && !text.contains("203")
                && !text.contains("204")),
        "child must not inherit parent future summary or suffix"
    );
    snapshot_restore_via_json_and_rows("child immediately after fork", &child);

    commit_text_turn(&mut child, 206, "child-local");
    apply_raw_compaction(
        &mut child,
        0,
        2,
        20_303,
        "child-tier",
        "summary:child-201-202",
    );
    assert_state_machine_invariants("child after local compaction", &child);
    snapshot_restore_via_json_and_rows("child after local compaction", &child);

    parent.revert_to(redo).expect("redo parent full cover");
    assert_state_machine_invariants("parent after redo", &parent);
    commit_text_turn(&mut parent, 207, "parent-local");
    apply_raw_compaction(&mut parent, 4, 6, 20_304, "parent-tail", "summary:205-207");
    assert_state_machine_invariants("parent after tail compaction", &parent);
    snapshot_restore_via_json_and_rows("parent after local compaction", &parent);

    assert!(
        parent.raw_turn(turn_id(206)).is_none(),
        "parent must not observe child suffix"
    );
    assert!(
        child.raw_turn(turn_id(207)).is_none(),
        "child must not observe parent suffix"
    );
    assert!(
        text_values(&parent)
            .iter()
            .all(|text| !text.contains("child-local")),
        "parent effective view must stay isolated from child"
    );
    assert!(
        text_values(&child)
            .iter()
            .all(|text| !text.contains("parent-local")),
        "child effective view must stay isolated from parent"
    );

    assert_can_commit_followup("parent final usability", &mut parent, 208);
    assert_can_commit_followup("child final usability", &mut child, 209);
}

#[test]
fn stale_boundary_and_bad_snapshot_fail_atomically_then_original_continues() {
    let mut conversation = conversation(3);
    commit_text_turn(&mut conversation, 301, "first");
    commit_text_turn(&mut conversation, 302, "second");
    assert_state_machine_invariants("initial stale-boundary fixture", &conversation);

    let stale_head = conversation.head();
    let before_revert = runtime_state(&conversation);
    let new_head = conversation
        .revert_to(conversation.valid_boundaries()[1])
        .expect("real revert")
        .new_head();
    assert_ne!(stale_head.version(), new_head.version());
    assert_state_machine_invariants("after real revert", &conversation);
    assert!(
        conversation.validate_boundary(&stale_head).is_err(),
        "old head token must be stale after structural version change"
    );
    assert_ne!(
        runtime_state(&conversation),
        before_revert,
        "real revert should move observable head"
    );

    let state_before_failed_revert = runtime_state(&conversation);
    let failed = conversation.revert_to(stale_head);
    assert!(
        failed.is_err(),
        "consuming a stale boundary must fail instead of redoing by accident"
    );
    assert_eq!(
        runtime_state(&conversation),
        state_before_failed_revert,
        "failed stale-boundary operation must be atomic"
    );

    let snapshot = conversation.snapshot().expect("snapshot reverted state");
    let mut corrupted = serde_json::to_value(&snapshot).expect("snapshot to JSON value");
    corrupted["schema_version"] = json!(999_u32);
    let corrupted_snapshot: ConversationSnapshot =
        serde_json::from_value(corrupted).expect("corrupted schema JSON shape still decodes");
    let state_before_bad_restore = runtime_state(&conversation);
    let restore_error = Conversation::restore(corrupted_snapshot);
    assert!(
        matches!(restore_error, Err(ConversationError::Restore(_))),
        "bad snapshot must be rejected by restore gate: {restore_error:?}"
    );
    assert_eq!(
        runtime_state(&conversation),
        state_before_bad_restore,
        "failed restore must not mutate the original conversation"
    );

    let redo = conversation.valid_boundaries()[2];
    conversation
        .revert_to(redo)
        .expect("fresh redo boundary works");
    assert_state_machine_invariants("after fresh redo", &conversation);
    assert_can_commit_followup(
        "after failed stale/bad-snapshot paths",
        &mut conversation,
        303,
    );
}
