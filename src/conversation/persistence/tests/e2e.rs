//! End-to-end persistence acceptance over snapshot, rows, restore, and views.

use super::*;
use crate::{
    conversation::{
        CancelDisposition, CancelOutcome, CancelledToolResult, MessageMeta, ToolCallLocationKind,
    },
    model::tool::ToolStatus,
};

fn assert_persistence_paths_restore(label: &str, conversation: &Conversation) {
    let before = runtime_state(conversation);
    let snapshot = conversation
        .snapshot()
        .unwrap_or_else(|error| panic!("{label}: snapshot failed: {error:?}"));
    let snapshot_json = serde_json::to_value(&snapshot)
        .unwrap_or_else(|error| panic!("{label}: snapshot json failed: {error:?}"));
    json_keys_do_not_contain_runtime_objects(&snapshot_json);

    let encoded_snapshot = serde_json::to_string(&snapshot)
        .unwrap_or_else(|error| panic!("{label}: encode snapshot failed: {error:?}"));
    let decoded_snapshot: ConversationSnapshot = serde_json::from_str(&encoded_snapshot)
        .unwrap_or_else(|error| panic!("{label}: decode snapshot failed: {error:?}"));
    assert_eq!(
        decoded_snapshot, snapshot,
        "{label}: JSON snapshot round trip changed facts"
    );
    let restored_from_json = Conversation::restore(decoded_snapshot)
        .unwrap_or_else(|error| panic!("{label}: restore from JSON snapshot failed: {error:?}"));
    assert_restored_conversation(label, conversation, &restored_from_json, &before);

    let mut rows = snapshot
        .to_rows()
        .unwrap_or_else(|error| panic!("{label}: snapshot to rows failed: {error:?}"));
    let encoded_rows = serde_json::to_string(&rows)
        .unwrap_or_else(|error| panic!("{label}: encode rows failed: {error:?}"));
    let mut decoded_rows: ConversationRows = serde_json::from_str(&encoded_rows)
        .unwrap_or_else(|error| panic!("{label}: decode rows failed: {error:?}"));
    scramble_rows(&mut rows);
    scramble_rows(&mut decoded_rows);

    for (path, rows) in [("rows", rows), ("serde rows", decoded_rows)] {
        let rebuilt_snapshot = ConversationSnapshot::from_rows(rows).unwrap_or_else(|error| {
            panic!("{label}: rebuild snapshot from {path} failed: {error:?}")
        });
        assert_eq!(
            rebuilt_snapshot, snapshot,
            "{label}: {path} rebuilt different snapshot facts"
        );
        let restored = Conversation::restore(rebuilt_snapshot)
            .unwrap_or_else(|error| panic!("{label}: restore from {path} failed: {error:?}"));
        assert_restored_conversation(label, conversation, &restored, &before);
    }
}

fn assert_restored_conversation(
    label: &str,
    original: &Conversation,
    restored: &Conversation,
    before: &RuntimeState,
) {
    assert_eq!(
        runtime_state(restored),
        *before,
        "{label}: runtime facts changed after restore"
    );
    assert_eq!(restored.id(), original.id(), "{label}: id changed");
    assert_eq!(
        restored.config(),
        original.config(),
        "{label}: config changed"
    );
    assert_eq!(
        restored.origin(),
        original.origin(),
        "{label}: fork origin changed"
    );
    assert!(
        restored.pending().is_none(),
        "{label}: restore must not recreate pending state"
    );
    assert_eq!(
        restored.effective_view(),
        original.effective_view(),
        "{label}: effective view changed"
    );
    assert_eq!(
        restored.effective_view().system(),
        original.effective_view().system(),
        "{label}: system prompt changed"
    );
    assert_eq!(
        restored.valid_boundaries(),
        original.valid_boundaries(),
        "{label}: boundary tokens changed"
    );
    assert_eq!(
        restored.tool_call_index(),
        &ToolCallIndex::rebuild(restored.turns(), restored.pending()),
        "{label}: tool-call index was not rebuilt from restored facts"
    );
    assert_eq!(
        turn_usage(restored),
        turn_usage(original),
        "{label}: usage facts changed"
    );
    assert_projection_provenance(label, original, restored);
}

fn assert_projection_provenance(label: &str, original: &Conversation, restored: &Conversation) {
    assert_eq!(
        restored.projection(),
        original.projection(),
        "{label}: projection changed"
    );
    for artifact in original.projection().artifacts() {
        let restored_artifact = restored
            .projection()
            .artifact(artifact.id())
            .unwrap_or_else(|| panic!("{label}: missing artifact {:?}", artifact.id()));
        assert_eq!(
            restored_artifact.provenance(),
            artifact.provenance(),
            "{label}: artifact provenance changed"
        );
    }
}

fn turn_usage(conversation: &Conversation) -> Vec<Usage> {
    conversation
        .raw_turns()
        .into_iter()
        .map(|turn| turn.meta().usage().clone())
        .collect()
}

fn visible_texts(conversation: &Conversation) -> Vec<String> {
    conversation
        .effective_view()
        .messages()
        .iter()
        .flat_map(message_texts)
        .collect()
}

fn message_texts(message: &Message) -> Vec<String> {
    message.content.iter().flat_map(block_texts).collect()
}

fn block_texts(block: &ContentBlock) -> Vec<String> {
    match block {
        ContentBlock::Text { text, .. } | ContentBlock::Thinking { text, .. } => {
            vec![text.clone()]
        }
        ContentBlock::ToolResult { content, .. } => content.iter().flat_map(block_texts).collect(),
        ContentBlock::ToolUse { id, name, .. } => vec![format!("tool_use:{name}:{id}")],
        ContentBlock::Image { .. } => Vec::new(),
    }
}

fn explicit_meta(seed: u128, source: &str) -> TurnMeta {
    TurnMeta::new(
        Usage::default(),
        Some(format!("2026-07-13T00:{:02}:00Z", seed % 60)),
        Some(source.to_owned()),
        Map::from_iter([("fixture_seed".to_owned(), json!(seed))]),
    )
}

fn finish_ready(conversation: &mut Conversation, response: Response, message_seed: u128) {
    assert_eq!(
        freeze_response(conversation, response, message_seed),
        AssistantFinish::ReadyToCommit
    );
}

fn finish_requires_mappings(
    conversation: &mut Conversation,
    response: Response,
    message_seed: u128,
) {
    assert_eq!(
        freeze_response(conversation, response, message_seed),
        AssistantFinish::RequiresToolCallMappings
    );
}

fn commit_serial_tool_turn(conversation: &mut Conversation, seed: u128) {
    let first_call = format!("call-serial-{seed}-a");
    let second_call = format!("call-serial-{seed}-b");

    begin(conversation, seed);
    finish_requires_mappings(
        conversation,
        assistant_response(
            vec![text("serial first lookup"), tool_use(&first_call)],
            8,
            3,
            StopReason::ToolUse,
            "serial-first",
        ),
        seed * 10 + 1,
    );
    conversation
        .register_tool_calls(vec![ToolCallMapping::new(
            &first_call,
            call_id(seed * 100 + 1),
        )])
        .expect("register first serial call");
    conversation
        .append_tool_response(
            message_id(seed * 10 + 2),
            tool_response(&first_call, "serial first result"),
        )
        .expect("append first serial result");

    finish_requires_mappings(
        conversation,
        assistant_response(
            vec![text("serial second lookup"), tool_use(&second_call)],
            6,
            3,
            StopReason::ToolUse,
            "serial-second",
        ),
        seed * 10 + 3,
    );
    conversation
        .register_tool_calls(vec![ToolCallMapping::new(
            &second_call,
            call_id(seed * 100 + 2),
        )])
        .expect("register second serial call");
    conversation
        .append_tool_response(
            message_id(seed * 10 + 4),
            tool_response(&second_call, "serial second result"),
        )
        .expect("append second serial result");

    finish_ready(
        conversation,
        assistant_response(
            vec![text(format!("serial final:{seed}"))],
            4,
            2,
            StopReason::EndTurn,
            "serial-final",
        ),
        seed * 10 + 5,
    );
    conversation
        .commit_pending(explicit_meta(seed, "serial-fixture"))
        .expect("commit serial tool turn");
}

fn commit_parallel_tool_turn(conversation: &mut Conversation, seed: u128) {
    let left_call = format!("call-parallel-{seed}-left");
    let right_call = format!("call-parallel-{seed}-right");

    begin(conversation, seed);
    finish_requires_mappings(
        conversation,
        assistant_response(
            vec![
                text("parallel lookup"),
                tool_use(&left_call),
                tool_use(&right_call),
            ],
            10,
            4,
            StopReason::ToolUse,
            "parallel-calls",
        ),
        seed * 10 + 1,
    );
    conversation
        .register_tool_calls(vec![
            ToolCallMapping::new(&left_call, call_id(seed * 100 + 1)),
            ToolCallMapping::new(&right_call, call_id(seed * 100 + 2)),
        ])
        .expect("register parallel calls");
    conversation
        .append_tool_response(
            message_id(seed * 10 + 2),
            tool_response(&right_call, "parallel right result"),
        )
        .expect("append right result first");
    conversation
        .append_tool_response(
            message_id(seed * 10 + 3),
            tool_response(&left_call, "parallel left result"),
        )
        .expect("append left result second");

    finish_ready(
        conversation,
        assistant_response(
            vec![text(format!("parallel final:{seed}"))],
            5,
            2,
            StopReason::EndTurn,
            "parallel-final",
        ),
        seed * 10 + 4,
    );
    conversation
        .commit_pending(explicit_meta(seed, "parallel-fixture"))
        .expect("commit parallel tool turn");
}

/// Commits a tool turn that carries one user message injected at the closed
/// tool-result step boundary with envelope metadata, returning the injected
/// message id and metadata for later assertions.
fn commit_injected_user_turn(
    conversation: &mut Conversation,
    seed: u128,
) -> (MessageId, MessageMeta) {
    let provider_call = format!("call-inject-{seed}");
    begin(conversation, seed);
    finish_requires_mappings(
        conversation,
        assistant_response(
            vec![text("injected lookup"), tool_use(&provider_call)],
            6,
            3,
            StopReason::ToolUse,
            "injected-tool",
        ),
        seed * 10 + 1,
    );
    conversation
        .register_tool_calls(vec![ToolCallMapping::new(
            &provider_call,
            call_id(seed * 100 + 1),
        )])
        .expect("register injected turn call");
    conversation
        .append_tool_response(
            message_id(seed * 10 + 2),
            tool_response(&provider_call, "injected tool result"),
        )
        .expect("append injected turn result");

    let injected_id = message_id(seed * 10 + 3);
    let injected_meta = MessageMeta::new(
        Some("pivot:human".to_owned()),
        Map::from_iter([("injected_by".to_owned(), json!("fixture"))]),
    );
    conversation
        .inject_user_message(
            conversation.head(),
            injected_id,
            user(format!("injected constraint:{seed}")),
            injected_meta.clone(),
        )
        .expect("inject user message at closed tool-result boundary");

    finish_ready(
        conversation,
        assistant_response(
            vec![text(format!("injected final:{seed}"))],
            4,
            2,
            StopReason::EndTurn,
            "injected-final",
        ),
        seed * 10 + 4,
    );
    conversation
        .commit_pending(explicit_meta(seed, "injected-fixture"))
        .expect("commit injected user turn");
    (injected_id, injected_meta)
}

fn compact_raw_range(
    conversation: &mut Conversation,
    start: usize,
    end: usize,
    artifact_seed: u128,
    strategy_version: &str,
    label: &str,
) {
    let covers = range(conversation, start, end);
    let produced_by = strategy(strategy_version);
    let artifact = summary_artifact(
        conversation,
        covers.clone(),
        artifact_seed,
        produced_by.clone(),
        label,
    );
    let plan = CompactionPlan::new(
        conversation,
        vec![CompactionStep::raw(covers, artifact.id(), produced_by)],
        vec![artifact],
    );
    conversation
        .apply_compaction(&plan)
        .expect("raw compaction applies");
}

fn compact_span_range(
    conversation: &mut Conversation,
    start: usize,
    end: usize,
    artifact_seed: u128,
    strategy_version: &str,
    label: &str,
) {
    let covers = range(conversation, start, end);
    let produced_by = strategy(strategy_version);
    let artifact = summary_artifact(
        conversation,
        covers.clone(),
        artifact_seed,
        produced_by.clone(),
        label,
    );
    let plan = CompactionPlan::new(
        conversation,
        vec![CompactionStep::spans(covers, artifact.id(), produced_by)],
        vec![artifact],
    );
    conversation
        .apply_compaction(&plan)
        .expect("span compaction applies");
}

fn assert_provider_locations(
    conversation: &Conversation,
    provider_call_id: &str,
    expected_call: ToolCallId,
    expected_turn: TurnId,
) {
    let locations = conversation
        .tool_call_index()
        .by_provider_call_id(provider_call_id)
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(locations.len(), 1, "expected one provider location");
    let location = &locations[0];
    assert_eq!(location.kind(), ToolCallLocationKind::Committed);
    assert_eq!(location.turn_id(), expected_turn);
    assert_eq!(location.call_id(), Some(expected_call));
    assert!(location.result_message_id().is_some());
    assert_eq!(
        conversation
            .tool_call_index()
            .by_call_id(expected_call)
            .expect("framework id location"),
        location
    );
}

#[test]
fn snapshot_and_rows_restore_effective_view_across_compaction_revert_fork_and_tools() {
    let mut parent = conversation(300);
    commit_text_turn(&mut parent, 301);
    commit_serial_tool_turn(&mut parent, 302);
    commit_parallel_tool_turn(&mut parent, 303);
    commit_text_turn(&mut parent, 304);
    commit_text_turn(&mut parent, 305);
    assert_persistence_paths_restore("base multi-tool history", &parent);
    assert_provider_locations(&parent, "call-serial-302-a", call_id(30_201), turn_id(302));
    assert_provider_locations(
        &parent,
        "call-parallel-303-right",
        call_id(30_302),
        turn_id(303),
    );

    let raw_turn_ids_before = parent
        .raw_turns()
        .into_iter()
        .map(Turn::id)
        .collect::<Vec<_>>();
    compact_raw_range(&mut parent, 0, 2, 30_000, "tier-a", "summary turns 301-302");
    compact_raw_range(&mut parent, 2, 4, 30_001, "tier-b", "summary turns 303-304");
    compact_span_range(
        &mut parent,
        0,
        4,
        30_002,
        "consolidated",
        "summary turns 301-304 consolidated",
    );
    assert_eq!(
        parent
            .raw_turns()
            .into_iter()
            .map(Turn::id)
            .collect::<Vec<_>>(),
        raw_turn_ids_before,
        "compaction must not rewrite raw turn ids"
    );
    assert_eq!(parent.projection().artifacts().len(), 3);
    assert!(matches!(
        parent.projection().spans()[0],
        Span::Compacted { .. }
    ));
    assert_persistence_paths_restore("consolidated parent", &parent);
    assert!(
        visible_texts(&parent)
            .iter()
            .any(|text| text == "summary turns 301-304 consolidated")
    );

    let redo = parent
        .revert_to(parent.valid_boundaries()[2])
        .expect("revert inside consolidated span")
        .old_head();
    let clipped_texts = visible_texts(&parent);
    assert!(
        clipped_texts.iter().any(|text| text == "question:301"),
        "reverted view should render visible raw prefix"
    );
    assert!(
        clipped_texts.iter().any(|text| text == "serial final:302"),
        "reverted view should include the complete visible serial tool turn"
    );
    assert!(
        clipped_texts.iter().all(|text| !text.contains("summary")
            && !text.contains("303")
            && !text.contains("304")),
        "head inside a compacted cover must not leak future summary or turns: {clipped_texts:?}"
    );
    assert_persistence_paths_restore("reverted inside compacted cover", &parent);

    parent
        .revert_to(redo)
        .expect("redo to full compacted cover");
    assert!(
        visible_texts(&parent)
            .iter()
            .any(|text| text == "summary turns 301-304 consolidated"),
        "redo should restore artifact rendering"
    );
    assert_persistence_paths_restore("redone parent", &parent);

    let fork_point = parent.valid_boundaries()[3];
    let mut child = parent
        .fork_at(fork_point, conversation_id(30_900))
        .expect("fork child from inside compacted cover");
    assert_eq!(
        child.origin().expect("child origin").fork_point(),
        fork_point
    );
    assert_eq!(child.projection().artifacts().len(), 0);
    assert!(
        visible_texts(&child).iter().all(|text| {
            !text.contains("summary") && !text.contains("304") && !text.contains("305")
        }),
        "child must not inherit parent summary or parent suffix"
    );
    assert_persistence_paths_restore("forked child before local suffix", &child);

    commit_text_turn(&mut parent, 306);
    commit_parallel_tool_turn(&mut child, 307);
    assert_persistence_paths_restore("parent after child fork and local suffix", &parent);
    assert_persistence_paths_restore("child after local parallel suffix", &child);
    assert!(parent.raw_turn(turn_id(307)).is_none());
    assert!(child.raw_turn(turn_id(306)).is_none());
    assert_provider_locations(
        &child,
        "call-parallel-307-left",
        call_id(30_701),
        turn_id(307),
    );
}

#[test]
fn pending_snapshot_rejection_can_be_followed_by_cancel_commit_or_discard_then_restore() {
    let mut discard = conversation(310);
    commit_text_turn(&mut discard, 311);
    begin(&mut discard, 312);
    discard
        .start_assistant()
        .expect("start active assistant before discard");
    discard
        .push_assistant_event(StreamEvent::MessageStart {
            role: Role::Assistant,
        })
        .expect("push message start");
    discard
        .push_assistant_event(StreamEvent::BlockStart {
            id: BlockId::new("discard-text"),
            kind: BlockKind::Text,
        })
        .expect("push text start");
    discard
        .push_assistant_event(StreamEvent::BlockDelta {
            id: BlockId::new("discard-text"),
            delta: Delta::Text("partial discard".to_owned()),
        })
        .expect("push partial text");
    assert_snapshot_rejected_without_state_change(&discard);
    assert_eq!(
        discard
            .cancel_pending(CancelDisposition::DiscardTurn)
            .expect("discard pending"),
        CancelOutcome::Discarded {
            turn_id: turn_id(312),
        }
    );
    assert!(discard.pending().is_none());
    assert!(
        visible_texts(&discard)
            .iter()
            .all(|text| !text.contains("partial discard"))
    );
    assert_persistence_paths_restore("discard after pending snapshot rejection", &discard);

    let mut commit = conversation(320);
    commit_text_turn(&mut commit, 321);
    begin(&mut commit, 322);
    finish_requires_mappings(
        &mut commit,
        assistant_response(
            vec![text("need cancellable tool"), tool_use("call-cancelled")],
            7,
            3,
            StopReason::ToolUse,
            "cancellable-tool",
        ),
        3221,
    );
    commit
        .register_tool_calls(vec![ToolCallMapping::new(
            "call-cancelled",
            call_id(32_200),
        )])
        .expect("register cancellable call");
    assert_snapshot_rejected_without_state_change(&commit);

    assert_eq!(
        commit
            .cancel_pending(CancelDisposition::commit_turn(
                vec![CancelledToolResult::new(
                    "call-cancelled",
                    call_id(32_200),
                    message_id(3222),
                )],
                message_id(3223),
                assistant_response(
                    vec![text("final after cancellation")],
                    3,
                    2,
                    StopReason::EndTurn,
                    "cancel-final",
                ),
                explicit_meta(322, "cancel-commit-fixture"),
            ))
            .expect("commit cancelled pending"),
        CancelOutcome::Committed {
            turn_id: turn_id(322),
        }
    );
    assert!(commit.pending().is_none());
    let cancelled_result = commit
        .raw_turn(turn_id(322))
        .expect("cancelled turn")
        .messages()
        .iter()
        .flat_map(|message| message.payload().content.iter())
        .find_map(|block| match block {
            ContentBlock::ToolResult {
                tool_use_id,
                status,
                ..
            } if tool_use_id == "call-cancelled" => Some(status),
            _ => None,
        })
        .expect("cancelled tool result block");
    assert_eq!(cancelled_result, &ToolStatus::Cancelled);
    assert!(
        visible_texts(&commit)
            .iter()
            .any(|text| text == "final after cancellation")
    );
    assert_persistence_paths_restore("commit after pending snapshot rejection", &commit);
}

#[test]
fn rows_round_trip_preserves_injected_user_message_meta() {
    let mut conversation = conversation(330);
    commit_text_turn(&mut conversation, 331);
    let (injected_id, injected_meta) = commit_injected_user_turn(&mut conversation, 332);

    let injected = conversation
        .raw_turn(turn_id(332))
        .expect("injected turn")
        .messages()
        .iter()
        .find(|message| message.id() == injected_id)
        .expect("injected message retained");
    assert_eq!(injected.meta(), Some(&injected_meta));

    // The full persistence acceptance (snapshot JSON, rows, scrambled rows,
    // restore) must carry the envelope metadata end to end.
    assert_persistence_paths_restore("injected user message meta", &conversation);

    let snapshot = conversation.snapshot().expect("snapshot");
    let rows = snapshot.to_rows().expect("rows");
    let row = rows
        .messages
        .iter()
        .find(|row| row.message_id == injected_id)
        .expect("injected message row");
    assert_eq!(row.meta, Some(injected_meta.clone()));

    let encoded = serde_json::to_string(&rows).expect("encode rows");
    assert!(
        encoded.contains("pivot:human"),
        "rows JSON carries the injected message meta source"
    );

    // Rows exported before the `meta` column existed still deserialize, with
    // the metadata defaulting to absent.
    let mut legacy_json: Value = serde_json::from_str(&encoded).expect("rows JSON value");
    for message in legacy_json["messages"]
        .as_array_mut()
        .expect("message row array")
    {
        message
            .as_object_mut()
            .expect("message row object")
            .remove("meta");
    }
    let legacy_rows: ConversationRows =
        serde_json::from_value(legacy_json).expect("legacy rows decode without meta");
    let legacy_row = legacy_rows
        .messages
        .iter()
        .find(|row| row.message_id == injected_id)
        .expect("injected legacy row");
    assert_eq!(legacy_row.meta, None);
}
