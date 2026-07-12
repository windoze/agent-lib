use super::*;

#[test]
fn no_pending_and_ready_to_commit_cancellation_states_are_classified() {
    let mut conversation = conversation();
    assert_eq!(
        conversation
            .cancel_pending(CancelDisposition::DiscardTurn)
            .expect_err("nothing to cancel"),
        ConversationError::Cancel(CancelError::NoPending)
    );

    begin(&mut conversation, 71, 710);
    freeze_response(
        &mut conversation,
        assistant_response(
            vec![text("already final")],
            1,
            1,
            StopReason::EndTurn,
            "req-ready",
        ),
        711,
    );
    let pending_before = pending_view(&conversation);
    let committed_before = committed_view(&conversation);

    assert_eq!(
        conversation
            .cancel_pending(CancelDisposition::ResumeTurn {
                cancelled_results: Vec::new(),
            })
            .expect_err("ready turn cannot resume past its final assistant"),
        ConversationError::Cancel(CancelError::InvalidTransition {
            disposition: "resume a cancelled turn",
            actual: PendingTurnPhase::ReadyToCommit,
        })
    );
    assert_eq!(pending_view(&conversation), pending_before);
    assert_eq!(committed_view(&conversation), committed_before);

    assert_eq!(
        conversation
            .cancel_pending(CancelDisposition::commit_turn(
                Vec::new(),
                message_id(712),
                assistant_response(
                    vec![text("second final")],
                    1,
                    1,
                    StopReason::EndTurn,
                    "req-second-final",
                ),
                TurnMeta::default(),
            ))
            .expect_err("ready turn uses normal commit instead"),
        ConversationError::Cancel(CancelError::InvalidTransition {
            disposition: "commit a cancelled turn",
            actual: PendingTurnPhase::ReadyToCommit,
        })
    );
    assert_eq!(pending_view(&conversation), pending_before);
    assert_eq!(committed_view(&conversation), committed_before);

    assert!(matches!(
        conversation
            .cancel_pending(CancelDisposition::DiscardTurn)
            .expect("ready pending can still be discarded"),
        CancelOutcome::Discarded { .. }
    ));
    commit_text_turn(&mut conversation, 72, 720, 721);
}

#[test]
fn missing_duplicate_unknown_and_reused_result_ids_leave_pending_unchanged() {
    let mut conversation = registered_parallel_turn();
    let pending_before = pending_view(&conversation);
    let committed_before = committed_view(&conversation);

    assert_eq!(
        conversation
            .cancel_pending(CancelDisposition::ResumeTurn {
                cancelled_results: vec![cancelled_result("parallel-a", 900, 702)],
            })
            .expect_err("every open call needs a result identity"),
        ConversationError::Cancel(CancelError::MissingCancellationResult {
            provider_call_id: "parallel-b".to_owned(),
        })
    );
    assert_eq!(pending_view(&conversation), pending_before);
    assert_eq!(committed_view(&conversation), committed_before);

    assert_eq!(
        conversation
            .cancel_pending(CancelDisposition::ResumeTurn {
                cancelled_results: vec![
                    cancelled_result("parallel-a", 900, 702),
                    cancelled_result("parallel-a", 900, 703),
                ],
            })
            .expect_err("provider call cannot receive two synthetic results"),
        ConversationError::Cancel(CancelError::DuplicateCancellationResult {
            provider_call_id: "parallel-a".to_owned(),
        })
    );
    assert_eq!(pending_view(&conversation), pending_before);

    assert_eq!(
        conversation
            .cancel_pending(CancelDisposition::ResumeTurn {
                cancelled_results: vec![
                    cancelled_result("parallel-a", 900, 702),
                    cancelled_result("not-open", 901, 703),
                ],
            })
            .expect_err("unknown call cannot be synthesized"),
        ConversationError::Cancel(CancelError::UnknownCancellationResult {
            provider_call_id: "not-open".to_owned(),
        })
    );
    assert_eq!(pending_view(&conversation), pending_before);

    assert_eq!(
        conversation
            .cancel_pending(CancelDisposition::ResumeTurn {
                cancelled_results: vec![
                    cancelled_result("parallel-a", 900, 702),
                    cancelled_result("parallel-b", 901, 702),
                ],
            })
            .expect_err("synthetic messages need unique ids"),
        ConversationError::Cancel(CancelError::DuplicateMessageId {
            message_id: message_id(702),
        })
    );
    assert_eq!(pending_view(&conversation), pending_before);
    assert_eq!(committed_view(&conversation), committed_before);

    conversation
        .cancel_pending(CancelDisposition::ResumeTurn {
            cancelled_results: vec![
                cancelled_result("parallel-a", 900, 702),
                cancelled_result("parallel-b", 901, 703),
            ],
        })
        .expect("corrected identities remain retryable");
    freeze_response(
        &mut conversation,
        assistant_response(
            vec![text("recovered")],
            1,
            1,
            StopReason::EndTurn,
            "req-recovered",
        ),
        704,
    );
    conversation
        .commit_pending(TurnMeta::default())
        .expect("retry commits without poisoned state");
}

#[test]
fn duplicated_provider_ids_in_frozen_tool_uses_cannot_be_synthetically_paired() {
    let mut conversation = conversation();
    begin(&mut conversation, 82, 820);
    freeze_response(
        &mut conversation,
        assistant_response(
            vec![tool_use("duplicate"), tool_use("duplicate")],
            1,
            1,
            StopReason::ToolUse,
            "req-duplicate-provider",
        ),
        821,
    );
    let pending_before = pending_view(&conversation);
    let committed_before = committed_view(&conversation);

    assert_eq!(
        conversation
            .cancel_pending(CancelDisposition::ResumeTurn {
                cancelled_results: vec![cancelled_result("duplicate", 970, 822)],
            })
            .expect_err("one provider id cannot identify two frozen calls"),
        ConversationError::Cancel(CancelError::DuplicateProviderCallId {
            provider_call_id: "duplicate".to_owned(),
        })
    );
    assert_eq!(pending_view(&conversation), pending_before);
    assert_eq!(committed_view(&conversation), committed_before);
    conversation
        .cancel_pending(CancelDisposition::DiscardTurn)
        .expect("ambiguous pending turn remains discardable");
}
