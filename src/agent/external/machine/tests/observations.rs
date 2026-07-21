//! Buffered observation notifications, resume dedup against the
//! persisted high-water mark, artifact recording, and cursor restore.

use super::*;

#[test]
fn external_agent_emits_observation_notifications() {
    // A Completed decision point replays its buffered observations, in order, as
    // `Notification::ExternalAgent` events on the resuming step (design §5.5).
    let mut direct = machine();
    let opened = direct.step(StepInput::external(user_input("refactor the parser")));
    let batch = observation_batch("done");
    let completed = direct.step(StepInput::resume(external_resolution(
        opened.requirements[0].id,
        completed_with(3, sequenced(1, batch.clone())),
    )));

    assert!(completed.is_quiescent());
    assert!(completed.requirements.is_empty());
    assert_eq!(direct.cursor().kind(), LoopCursorKind::Done);
    // Exactly the buffered observations, preserving order and count.
    assert_eq!(external_events(&completed.notifications), batch);

    // The machine records `last_event_seq` in its retained session facts and
    // dedups observations *per event* on resume: a replayed decision point whose
    // events all fall at or below the consumed sequence emits nothing, and an
    // overlapping batch straddling the boundary replays only its unseen suffix
    // (design §5.5).
    let mut looped = machine();
    let opened = looped.step(StepInput::external(user_input("refactor the parser")));

    // First pause buffers seqs 1..=3 with no prior consumed sequence, so all
    // three events are emitted and seq 3 becomes the consumed high-water mark.
    let first_batch = observation_batch("first");
    let first_pause = looped.step(StepInput::resume(external_resolution(
        opened.requirements[0].id,
        paused_with("act-1", 3, sequenced(1, first_batch.clone())),
    )));
    assert_eq!(external_events(&first_pause.notifications), first_batch);

    // Answer the interaction so the turn loops back to AwaitingSession.
    let responded = looped.step(StepInput::resume(interaction_resolution(
        first_pause.requirements[0].id,
        "go ahead",
    )));

    // A replayed pause reporting the same events (seqs 1..=3) is a duplicate:
    // every event is at or below the consumed sequence, so nothing is re-emitted.
    let replay_pause = looped.step(StepInput::resume(external_resolution(
        responded.requirements[0].id,
        paused_with("act-1", 3, sequenced(1, observation_batch("first"))),
    )));
    assert!(
        replay_pause.notifications.is_empty(),
        "observations at or below the consumed sequence must not be replayed"
    );

    // Answer again; the next pause overlaps the consumed boundary: seqs 3..=5
    // against a consumed mark of 3. Only the strictly-greater suffix (seqs 4 and
    // 5) is replayed, proving dedup is per event rather than per batch.
    let responded_again = looped.step(StepInput::resume(interaction_resolution(
        replay_pause.requirements[0].id,
        "go ahead",
    )));
    let overlap_batch = observation_batch("overlap");
    let overlap_pause = looped.step(StepInput::resume(external_resolution(
        responded_again.requirements[0].id,
        paused_with("act-1", 5, sequenced(3, overlap_batch.clone())),
    )));
    assert_eq!(
        external_events(&overlap_pause.notifications),
        overlap_batch[1..].to_vec(),
        "only observations beyond the consumed sequence are replayed"
    );

    // A final Completed reporting a fresh sequence (seqs 6..=8) beyond the
    // consumed one (5) replays its new observations in full.
    let responded_final = looped.step(StepInput::resume(interaction_resolution(
        overlap_pause.requirements[0].id,
        "go ahead",
    )));
    let final_batch = observation_batch("final");
    let final_completed = looped.step(StepInput::resume(external_resolution(
        responded_final.requirements[0].id,
        completed_with(8, sequenced(6, final_batch.clone())),
    )));

    assert_eq!(looped.cursor().kind(), LoopCursorKind::Done);
    assert_eq!(external_events(&final_completed.notifications), final_batch);
}

#[test]
fn restored_machine_dedups_against_the_persisted_high_water() {
    // Cross-process resume (review M-EXT-1): the dedup high-water mark lives in
    // the persisted state, so it survives a snapshot restore. An adapter-side
    // resume must continue the seq line past that mark (the adapter seeds its
    // decoder from `ExternalSessionRef::last_event_seq`) — seqs continuing at
    // 51 are emitted, while a batch at or below the persisted mark stays
    // deduped.
    let mut machine = machine();
    let opened = machine.step(StepInput::external(user_input("refactor the parser")));
    let before_batch = observation_batch("before-restart");
    let completed = machine.step(StepInput::resume(external_resolution(
        opened.requirements[0].id,
        completed_with(50, sequenced(48, before_batch.clone())),
    )));
    assert_eq!(external_events(&completed.notifications), before_batch);
    assert_eq!(machine.cursor().kind(), LoopCursorKind::Done);

    // Persist and restore across the process boundary; the consumed mark of 50
    // rides the retained session facts.
    let encoded = serde_json::to_value(machine.state()).expect("serialize state");
    let decoded: ExternalAgentState = serde_json::from_value(encoded).expect("deserialize state");
    let mut restored = ExternalAgentMachine::new(decoded, Arc::new(SeqRequirementIds::default()));

    // The follow-up turn asks the driver to resume the persisted session, so
    // the request carries the old high-water mark for the adapter to seed from.
    let follow_up = restored.step(StepInput::external(user_input_seq("now add tests", 1)));
    assert_eq!(
        need_session_request(&follow_up)
            .session
            .as_ref()
            .and_then(|session| session.last_event_seq),
        Some(50),
        "the resume request carries the persisted high-water mark"
    );

    // A resumed session whose decoder continued the seq line at 51 emits its
    // first post-resume observations instead of dropping them.
    let after_batch = observation_batch("after-restart");
    let completed = restored.step(StepInput::resume(external_resolution(
        follow_up.requirements[0].id,
        completed_with(53, sequenced(51, after_batch.clone())),
    )));
    assert_eq!(restored.cursor().kind(), LoopCursorKind::Done);
    assert_eq!(
        external_events(&completed.notifications),
        after_batch,
        "the first observations of a resumed session survive the persisted mark"
    );

    // A decoder that wrongly restarted at 0 would produce only seqs at or below
    // the mark; those stay deduped, which is exactly the silent gap M-EXT-1
    // reported — this assertion pins the dedup side of the contract.
    let third = restored.step(StepInput::external(user_input_seq("and format", 2)));
    let replayed = restored.step(StepInput::resume(external_resolution(
        third.requirements[0].id,
        completed_with(53, sequenced(51, observation_batch("replay"))),
    )));
    assert!(
        replayed.notifications.is_empty(),
        "observations at or below the persisted mark must not be replayed"
    );
}

#[test]
fn external_agent_records_artifacts() {
    // A completed session folds `ExternalAgentOutput.artifacts` into the retained
    // trace on `ExternalAgentState`, preserving order (design §11).
    let mut direct = machine();
    assert!(
        direct.state().artifacts().is_empty(),
        "a fresh machine records no artifacts"
    );

    let opened = direct.step(StepInput::external(user_input("refactor the parser")));
    let artifacts = sample_artifacts();
    let completed = direct.step(StepInput::resume(external_resolution(
        opened.requirements[0].id,
        completed_with_artifacts(artifacts.clone()),
    )));

    assert!(completed.is_quiescent());
    assert_eq!(direct.cursor().kind(), LoopCursorKind::Done);
    assert_eq!(direct.state().artifacts(), artifacts.as_slice());

    // Only redacted references are recorded — a kind, an untrusted summary, and
    // opaque path/reference handles — never inline artifact content (§12).
    for artifact in direct.state().artifacts() {
        if let Some(reference) = artifact.reference.as_deref() {
            assert!(
                reference.starts_with("blob://"),
                "reference must be an opaque handle, not inline content: {reference}"
            );
        }
    }

    // The recorded references survive the state persistence boundary unchanged.
    let encoded = serde_json::to_value(direct.state()).expect("serialize state");
    let decoded: ExternalAgentState = serde_json::from_value(encoded).expect("deserialize state");
    assert_eq!(decoded.artifacts(), artifacts.as_slice());
}

#[test]
fn external_agent_records_no_artifacts_when_output_reports_none() {
    // A completion with an empty artifact list leaves the recorded trace empty and
    // keeps the artifacts field absent from the persisted state (backward-compatible
    // snapshot shape).
    let mut direct = machine();
    let opened = direct.step(StepInput::external(user_input("refactor the parser")));
    direct.step(StepInput::resume(external_resolution(
        opened.requirements[0].id,
        completed_with_artifacts(Vec::new()),
    )));

    assert!(direct.state().artifacts().is_empty());
    let encoded = serde_json::to_value(direct.state()).expect("serialize state");
    assert!(
        encoded.get("artifacts").is_none(),
        "an empty artifact list is skipped in the snapshot"
    );
}

#[test]
fn awaiting_tool_cursor_restores_without_a_terminal_view() {
    // A machine restored while a session is parked on a tool batch keeps the
    // resumable requirement addressing on its serializable cursor, but the
    // non-serialized batch scratch and driver-facing streaming view cannot be
    // rebuilt from state alone. `initial_loop_cursor` must therefore surface a
    // non-terminal `Idle` view (never a false `Done`/`Error`) so the driver does
    // not mistake a mid-flight batch for a finished turn. Faithfully rehydrating
    // the streaming/tool-wait view is the "恢复 mid-turn scratch" follow-up
    // tracked in PLAN.md.
    let batch_id = ExternalToolBatchId::new("batch-91");
    let requirement: RequirementId = "018f0d9c-7b6a-7c12-8f31-1234567890cf"
        .parse()
        .expect("requirement id");
    let call_id: ToolCallId = "018f0d9c-7b6a-7c12-8f31-1234567890ce"
        .parse()
        .expect("tool call id");
    let requirements = ToolWaitRequirements::root({
        let mut ids = std::collections::BTreeMap::new();
        ids.insert(call_id, requirement);
        ids
    });

    let mut state = ExternalAgentState::new(spec(), empty_conversation());
    state.set_cursor(ExternalAgentCursor::AwaitingTool {
        batch_id: batch_id.clone(),
        requirements: requirements.clone(),
    });

    // Persist and restore the state to prove the resumable addressing survives
    // the snapshot boundary while the volatile scratch does not.
    let encoded = serde_json::to_value(&state).expect("serialize state");
    assert_eq!(
        encoded["cursor"]["state"],
        serde_json::json!("awaiting_tool")
    );
    let decoded: ExternalAgentState = serde_json::from_value(encoded).expect("deserialize state");
    assert_eq!(
        decoded.cursor(),
        &ExternalAgentCursor::AwaitingTool {
            batch_id,
            requirements: requirements.clone(),
        }
    );
    assert_eq!(decoded.cursor().requirements(), Some(&requirements));

    let restored = ExternalAgentMachine::new(decoded, Arc::new(SeqRequirementIds::default()));

    // Degraded driver-facing view: non-terminal `Idle`, not a false terminal.
    let kind = restored.cursor().kind();
    assert_eq!(kind, LoopCursorKind::Idle);
    assert_ne!(kind, LoopCursorKind::Done);
    assert_ne!(kind, LoopCursorKind::Error);
    // The streaming view is not rebuilt, so the driver-facing cursor reports no
    // pending requirements; the outstanding ids remain recoverable from the
    // serializable external cursor above.
    assert!(restored.cursor().pending_requirement_ids().is_empty());
}
