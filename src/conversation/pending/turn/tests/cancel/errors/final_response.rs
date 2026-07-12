use super::*;

#[test]
fn invalid_final_role_or_tool_use_keeps_the_original_complete_response_active() {
    let mut conversation = conversation();
    begin(&mut conversation, 90, 900);
    conversation
        .start_assistant_response(assistant_response(
            vec![text("original response")],
            2,
            1,
            StopReason::EndTurn,
            "req-original",
        ))
        .expect("hold a complete response in active pending state");
    let pending_before = pending_view(&conversation);
    let committed_before = committed_view(&conversation);

    let mut wrong_role = assistant_response(
        vec![text("not assistant")],
        1,
        1,
        StopReason::EndTurn,
        "req-wrong-role",
    );
    wrong_role.message.role = Role::User;
    assert_eq!(
        conversation
            .cancel_pending(CancelDisposition::commit_turn(
                Vec::new(),
                message_id(901),
                wrong_role,
                TurnMeta::default(),
            ))
            .expect_err("replacement must be an assistant response"),
        ConversationError::Cancel(CancelError::InvalidFinalResponse {
            source: PendingMessageError::InvalidResponseRole { actual: Role::User },
        })
    );
    assert_eq!(pending_view(&conversation), pending_before);
    assert_eq!(committed_view(&conversation), committed_before);

    assert_eq!(
        conversation
            .cancel_pending(CancelDisposition::commit_turn(
                Vec::new(),
                message_id(901),
                assistant_response(
                    vec![tool_use("reopened")],
                    1,
                    1,
                    StopReason::ToolUse,
                    "req-reopened",
                ),
                TurnMeta::default(),
            ))
            .expect_err("replacement cannot reopen a call"),
        ConversationError::Cancel(CancelError::FinalAssistantContainsToolUse {
            provider_call_id: "reopened".to_owned(),
        })
    );
    assert_eq!(pending_view(&conversation), pending_before);
    assert_eq!(committed_view(&conversation), committed_before);

    assert_eq!(
        conversation
            .finish_assistant(message_id(901))
            .expect("original complete response survived both failures"),
        AssistantFinish::ReadyToCommit
    );
    conversation
        .commit_pending(TurnMeta::default())
        .expect("original response still commits");
    assert_eq!(
        conversation.turns()[0].messages()[1].payload().content,
        vec![text("original response")]
    );
}

#[test]
fn validator_rejection_keeps_pending_and_allows_corrected_commit_retry() {
    let mut conversation = conversation();
    begin(&mut conversation, 100, 1000);
    let pending_before = pending_view(&conversation);
    let committed_before = committed_view(&conversation);
    let invalid_image = ContentBlock::Image {
        source: ImageSource::Url {
            url: "https://example.test/not-assistant.png".to_owned(),
            extra: Map::new(),
        },
        extra: Map::new(),
    };

    assert_eq!(
        conversation
            .cancel_pending(CancelDisposition::commit_turn(
                Vec::new(),
                message_id(1001),
                assistant_response(
                    vec![invalid_image],
                    1,
                    1,
                    StopReason::EndTurn,
                    "req-invalid-content",
                ),
                TurnMeta::default(),
            ))
            .expect_err("shared validator rejects invalid assistant block"),
        ConversationError::Commit(CommitError::InvalidRoleBlock {
            message_id: message_id(1001),
            role: Role::Assistant,
            block: ContentBlockKind::Image,
        })
    );
    assert_eq!(pending_view(&conversation), pending_before);
    assert_eq!(committed_view(&conversation), committed_before);

    assert_eq!(
        conversation
            .cancel_pending(CancelDisposition::commit_turn(
                Vec::new(),
                message_id(1001),
                assistant_response(
                    vec![text("valid retry")],
                    1,
                    1,
                    StopReason::EndTurn,
                    "req-valid-retry",
                ),
                TurnMeta::default(),
            ))
            .expect("failed candidate did not consume its id"),
        CancelOutcome::Committed {
            turn_id: turn_id(100),
        }
    );
    assert!(conversation.pending().is_none());
    assert_eq!(conversation.turns().len(), 1);
}
