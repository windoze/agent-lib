use super::*;

#[test]
fn final_tool_use_cannot_commit_and_validator_failure_keeps_both_states() {
    let mut tool_turn = conversation();
    begin(&mut tool_turn, 60, 600);
    freeze_response(
        &mut tool_turn,
        assistant_response(
            vec![tool_use("still-open")],
            1,
            1,
            StopReason::ToolUse,
            "req-open",
        ),
        601,
    );
    let tool_pending = pending_view(&tool_turn);
    let tool_committed = committed_view(&tool_turn);
    let error = tool_turn
        .commit_pending(TurnMeta::default())
        .expect_err("tool use is not a final assistant");
    assert!(matches!(
        error,
        ConversationError::PendingTurn(PendingTurnError::InvalidTransition {
            actual: PendingTurnPhase::AwaitingToolCallMappings,
            ..
        })
    ));
    assert_eq!(pending_view(&tool_turn), tool_pending);
    assert_eq!(committed_view(&tool_turn), tool_committed);

    let mut invalid_content = conversation();
    begin(&mut invalid_content, 61, 610);
    freeze_response(
        &mut invalid_content,
        assistant_response(
            vec![ContentBlock::Image {
                source: ImageSource::Url {
                    url: "https://example.test/not-assistant.png".to_owned(),
                    extra: Map::new(),
                },
                extra: Map::new(),
            }],
            1,
            1,
            StopReason::EndTurn,
            "req-invalid",
        ),
        611,
    );
    let pending_before = pending_view(&invalid_content);
    let committed_before = committed_view(&invalid_content);
    let validation_error = invalid_content
        .commit_pending(TurnMeta::default())
        .expect_err("M1 validator rejects invalid assistant content");
    assert_eq!(
        validation_error,
        ConversationError::Commit(CommitError::InvalidRoleBlock {
            message_id: message_id(611),
            role: Role::Assistant,
            block: ContentBlockKind::Image,
        })
    );
    assert_eq!(pending_view(&invalid_content), pending_before);
    assert_eq!(committed_view(&invalid_content), committed_before);
}

#[test]
fn pending_operations_at_a_committed_boundary_are_classified() {
    let mut conversation = conversation();

    assert_eq!(
        conversation
            .commit_pending(TurnMeta::default())
            .expect_err("nothing to commit"),
        ConversationError::PendingTurn(PendingTurnError::NoPending)
    );
    assert_eq!(
        conversation
            .start_assistant()
            .expect_err("nothing to advance"),
        ConversationError::PendingTurn(PendingTurnError::NoPending)
    );
}

#[test]
fn tool_response_status_is_preserved_in_pending_and_closed_messages() {
    let mut conversation = conversation();
    begin(&mut conversation, 70, 700);
    freeze_response(
        &mut conversation,
        assistant_response(
            vec![tool_use("denied-call")],
            1,
            1,
            StopReason::ToolUse,
            "req-denied",
        ),
        701,
    );
    conversation
        .register_tool_calls(vec![mapping("denied-call", 800)])
        .expect("map denied call");
    conversation
        .append_tool_response(
            message_id(702),
            ToolResponse {
                tool_call_id: "denied-call".to_owned(),
                content: vec![text("approval denied")],
                status: ToolStatus::Denied,
                extra: Map::new(),
            },
        )
        .expect("append denied result");
    freeze_response(
        &mut conversation,
        assistant_response(
            vec![text("cannot continue")],
            1,
            1,
            StopReason::EndTurn,
            "req-end",
        ),
        703,
    );
    conversation
        .commit_pending(TurnMeta::default())
        .expect("commit denied result");

    let ContentBlock::ToolResult { status, .. } =
        &conversation.turns()[0].messages()[2].payload().content[0]
    else {
        panic!("expected tool result");
    };
    assert_eq!(*status, ToolStatus::Denied);
}
