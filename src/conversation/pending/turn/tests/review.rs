//! Milestone 2 review tests spanning every pending phase and cancellation mode.

use super::*;
use crate::{
    client::ClientError,
    conversation::{
        CancelDisposition, CancelError, CancelOutcome, CancelledToolResult, ConversationError,
        PendingMessageError, TurnMeta,
    },
};

/// Test-only names for the five externally observable pending phases.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReviewPhase {
    AwaitingAssistant,
    AssistantInProgress,
    AwaitingToolCallMappings,
    AwaitingToolResults,
    ReadyToCommit,
}

impl ReviewPhase {
    /// Returns the public phase represented by this fixture selector.
    const fn public(self) -> PendingTurnPhase {
        match self {
            Self::AwaitingAssistant => PendingTurnPhase::AwaitingAssistant,
            Self::AssistantInProgress => PendingTurnPhase::AssistantInProgress,
            Self::AwaitingToolCallMappings => PendingTurnPhase::AwaitingToolCallMappings,
            Self::AwaitingToolResults => PendingTurnPhase::AwaitingToolResults,
            Self::ReadyToCommit => PendingTurnPhase::ReadyToCommit,
        }
    }
}

/// Every public phase, used to keep the disposition audit exhaustive.
const REVIEW_PHASES: [ReviewPhase; 5] = [
    ReviewPhase::AwaitingAssistant,
    ReviewPhase::AssistantInProgress,
    ReviewPhase::AwaitingToolCallMappings,
    ReviewPhase::AwaitingToolResults,
    ReviewPhase::ReadyToCommit,
];

/// One pending conversation plus the exact identities needed to close it.
struct PhaseFixture {
    conversation: Conversation,
    cancelled_results: Vec<CancelledToolResult>,
    final_message_id: MessageId,
    final_message_seed: u128,
    next_seed: u128,
}

/// Builds a conversation at one precise phase without touching committed history.
fn phase_fixture(phase: ReviewPhase, seed: u128) -> PhaseFixture {
    let mut conversation = conversation();
    begin(&mut conversation, seed, seed + 1);
    let provider_call_id = format!("review-call-{seed}");
    let cancelled_results = match phase {
        ReviewPhase::AwaitingAssistant => Vec::new(),
        ReviewPhase::AssistantInProgress => {
            conversation
                .start_assistant_response(assistant_response(
                    vec![text("complete but not frozen")],
                    2,
                    1,
                    StopReason::EndTurn,
                    &format!("req-active-{seed}"),
                ))
                .expect("install complete response as the one active message");
            Vec::new()
        }
        ReviewPhase::AwaitingToolCallMappings | ReviewPhase::AwaitingToolResults => {
            assert_eq!(
                freeze_response(
                    &mut conversation,
                    assistant_response(
                        vec![tool_use(&provider_call_id)],
                        3,
                        1,
                        StopReason::ToolUse,
                        &format!("req-tool-{seed}"),
                    ),
                    seed + 2,
                ),
                AssistantFinish::RequiresToolCallMappings
            );
            if phase == ReviewPhase::AwaitingToolResults {
                conversation
                    .register_tool_calls(vec![mapping(&provider_call_id, seed + 5)])
                    .expect("register the review call");
            }
            vec![CancelledToolResult::new(
                provider_call_id,
                call_id(seed + 5),
                message_id(seed + 3),
            )]
        }
        ReviewPhase::ReadyToCommit => {
            assert_eq!(
                freeze_response(
                    &mut conversation,
                    assistant_response(
                        vec![text("already final")],
                        1,
                        1,
                        StopReason::EndTurn,
                        &format!("req-ready-{seed}"),
                    ),
                    seed + 2,
                ),
                AssistantFinish::ReadyToCommit
            );
            Vec::new()
        }
    };

    assert_eq!(
        conversation.pending().expect("pending fixture").phase(),
        phase.public()
    );
    assert!(conversation.turns().is_empty());

    PhaseFixture {
        conversation,
        cancelled_results,
        final_message_id: message_id(seed + 4),
        final_message_seed: seed + 4,
        next_seed: seed + 20,
    }
}

/// Completes a fresh text turn, proving the previous cancellation did not poison state.
fn commit_next_text_turn(conversation: &mut Conversation, seed: u128) {
    begin(conversation, seed, seed + 1);
    assert_eq!(
        freeze_response(
            conversation,
            assistant_response(
                vec![text("next turn remains usable")],
                1,
                1,
                StopReason::EndTurn,
                &format!("req-next-{seed}"),
            ),
            seed + 2,
        ),
        AssistantFinish::ReadyToCommit
    );
    conversation
        .commit_pending(TurnMeta::default())
        .expect("commit a new turn after cancellation");
}

/// Finishes the retained turn after `ResumeTurn`, then exercises another feed.
fn finish_resumed_turn(fixture: &mut PhaseFixture) {
    assert_eq!(
        freeze_response(
            &mut fixture.conversation,
            assistant_response(
                vec![text("replacement after cancellation")],
                1,
                1,
                StopReason::EndTurn,
                "req-resumed-final",
            ),
            fixture.final_message_seed,
        ),
        AssistantFinish::ReadyToCommit
    );
    fixture
        .conversation
        .commit_pending(TurnMeta::default())
        .expect("commit the resumed pending turn");
    commit_next_text_turn(&mut fixture.conversation, fixture.next_seed);
}

/// Verifies that whole-turn discard is valid from every pending phase.
#[test]
fn discard_turn_accepts_every_pending_phase_and_allows_a_new_feed() {
    for (index, phase) in REVIEW_PHASES.into_iter().enumerate() {
        let mut fixture = phase_fixture(phase, 1_000 + index as u128 * 100);
        let committed_before = committed_view(&fixture.conversation);
        let turn_id = fixture
            .conversation
            .pending()
            .expect("pending before discard")
            .id();

        assert_eq!(
            fixture
                .conversation
                .cancel_pending(CancelDisposition::DiscardTurn)
                .expect("discard is legal from every phase"),
            CancelOutcome::Discarded { turn_id },
            "phase {phase:?}"
        );
        assert!(fixture.conversation.pending().is_none(), "phase {phase:?}");
        assert_eq!(
            committed_view(&fixture.conversation),
            committed_before,
            "phase {phase:?}"
        );
        commit_next_text_turn(&mut fixture.conversation, fixture.next_seed);
    }
}

/// Verifies resume semantics for every phase, including the final-state rejection.
#[test]
fn resume_turn_closes_open_calls_except_after_a_final_assistant() {
    for (index, phase) in REVIEW_PHASES.into_iter().enumerate() {
        let mut fixture = phase_fixture(phase, 2_000 + index as u128 * 100);
        let pending_before = pending_view(&fixture.conversation);
        let committed_before = committed_view(&fixture.conversation);
        let turn_id = fixture
            .conversation
            .pending()
            .expect("pending before resume")
            .id();
        let result = fixture
            .conversation
            .cancel_pending(CancelDisposition::ResumeTurn {
                cancelled_results: fixture.cancelled_results.clone(),
            });

        if phase == ReviewPhase::ReadyToCommit {
            assert_eq!(
                result.expect_err("a final assistant cannot be resumed past"),
                ConversationError::Cancel(CancelError::InvalidTransition {
                    disposition: "resume a cancelled turn",
                    actual: PendingTurnPhase::ReadyToCommit,
                })
            );
            assert_eq!(pending_view(&fixture.conversation), pending_before);
            assert_eq!(committed_view(&fixture.conversation), committed_before);
            fixture
                .conversation
                .commit_pending(TurnMeta::default())
                .expect("the rejected ready turn remains committable");
            commit_next_text_turn(&mut fixture.conversation, fixture.next_seed);
            continue;
        }

        assert_eq!(
            result.expect("all non-final phases can resume"),
            CancelOutcome::Resumed { turn_id },
            "phase {phase:?}"
        );
        let pending = fixture.conversation.pending().expect("resumed pending");
        assert_eq!(pending.phase(), PendingTurnPhase::AwaitingAssistant);
        assert_eq!(pending.open_calls().count(), 0);
        assert_eq!(committed_view(&fixture.conversation), committed_before);
        finish_resumed_turn(&mut fixture);
    }
}

/// Verifies atomic replacement-and-commit semantics for every eligible phase.
#[test]
fn commit_turn_accepts_every_non_final_phase_and_rejects_ready_to_commit() {
    for (index, phase) in REVIEW_PHASES.into_iter().enumerate() {
        let mut fixture = phase_fixture(phase, 3_000 + index as u128 * 100);
        let pending_before = pending_view(&fixture.conversation);
        let committed_before = committed_view(&fixture.conversation);
        let turn_id = fixture
            .conversation
            .pending()
            .expect("pending before cancel commit")
            .id();
        let result = fixture
            .conversation
            .cancel_pending(CancelDisposition::commit_turn(
                fixture.cancelled_results.clone(),
                fixture.final_message_id,
                assistant_response(
                    vec![text("atomic cancellation terminal")],
                    1,
                    1,
                    StopReason::EndTurn,
                    "req-cancel-commit",
                ),
                TurnMeta::default(),
            ));

        if phase == ReviewPhase::ReadyToCommit {
            assert_eq!(
                result.expect_err("a second final assistant is forbidden"),
                ConversationError::Cancel(CancelError::InvalidTransition {
                    disposition: "commit a cancelled turn",
                    actual: PendingTurnPhase::ReadyToCommit,
                })
            );
            assert_eq!(pending_view(&fixture.conversation), pending_before);
            assert_eq!(committed_view(&fixture.conversation), committed_before);
            fixture
                .conversation
                .commit_pending(TurnMeta::default())
                .expect("the rejected ready turn remains committable");
        } else {
            assert_eq!(
                result.expect("all non-final phases can cancel and commit"),
                CancelOutcome::Committed { turn_id },
                "phase {phase:?}"
            );
            assert!(fixture.conversation.pending().is_none());
            assert_eq!(fixture.conversation.turns().len(), 1);
        }
        commit_next_text_turn(&mut fixture.conversation, fixture.next_seed);
    }
}

/// Proves an accumulator error can be cancelled without leaking or poisoning state.
#[test]
fn terminal_active_message_is_dropped_before_a_replacement_feed() {
    let mut conversation = conversation();
    begin(&mut conversation, 4_000, 4_001);
    conversation
        .start_assistant()
        .expect("start the sole active accumulator");
    let block_id = BlockId::new("review-terminal-partial");
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
            delta: Delta::Text("must never freeze".to_owned()),
        },
    ] {
        conversation
            .push_assistant_event(event)
            .expect("accumulate a partial block before the provider error");
    }
    let error = conversation
        .push_assistant_event(StreamEvent::Error(ClientError::Network(
            "review disconnect".to_owned(),
        )))
        .expect_err("provider error makes the active message terminal");
    assert!(matches!(
        error,
        ConversationError::PendingMessage(PendingMessageError::Accumulator(_))
    ));
    assert_eq!(
        conversation
            .pending()
            .expect("terminal pending turn")
            .phase(),
        PendingTurnPhase::AssistantInProgress
    );
    assert_eq!(
        conversation
            .pending()
            .expect("terminal pending turn")
            .messages()
            .len(),
        1,
        "the partial assistant never crossed the freeze boundary"
    );

    assert_eq!(
        conversation
            .cancel_pending(CancelDisposition::ResumeTurn {
                cancelled_results: Vec::new(),
            })
            .expect("drop terminal active state"),
        CancelOutcome::Resumed {
            turn_id: turn_id(4_000),
        }
    );
    assert_eq!(
        conversation.pending().expect("resumed pending").phase(),
        PendingTurnPhase::AwaitingAssistant
    );

    assert_eq!(
        freeze_response(
            &mut conversation,
            assistant_response(
                vec![text("clean replacement")],
                1,
                1,
                StopReason::EndTurn,
                "req-clean-replacement",
            ),
            4_002,
        ),
        AssistantFinish::ReadyToCommit
    );
    conversation
        .commit_pending(TurnMeta::default())
        .expect("commit after cancelling terminal state");
    let turn = &conversation.turns()[0];
    assert_eq!(turn.messages().len(), 2);
    assert_eq!(
        turn.messages()[1].payload().content,
        vec![text("clean replacement")]
    );
    commit_next_text_turn(&mut conversation, 4_020);
}

mod status_chain;
