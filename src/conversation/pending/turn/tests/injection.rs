use super::{
    assistant_response, begin, call_id, conversation, freeze_response, mapping, message_id,
    pending_view, text, tool_response, user,
};
use crate::{
    conversation::{
        AssistantFinish, BoundaryError, Conversation, ConversationConfig, ConversationError,
        ConversationId, MessageMeta, PendingTurnError, PendingTurnPhase, TurnMeta,
    },
    model::{
        message::{Message, Role},
        normalized::StopReason,
    },
};
use serde_json::{Map, json};

fn injection_meta(source: &str) -> MessageMeta {
    MessageMeta::new(
        Some(source.to_owned()),
        Map::from_iter([("pivot_id".to_owned(), json!(source))]),
    )
}

fn prepare_closed_tool_step(
    conversation: &mut Conversation,
    turn_seed: u128,
    user_seed: u128,
    assistant_seed: u128,
    result_seed: u128,
    call_seed: u128,
    provider_call_id: &str,
) {
    begin(conversation, turn_seed, user_seed);
    assert_eq!(
        freeze_response(
            conversation,
            assistant_response(
                vec![text("lookup"), super::tool_use(provider_call_id)],
                5,
                2,
                StopReason::ToolUse,
                "req-tool",
            ),
            assistant_seed,
        ),
        AssistantFinish::RequiresToolCallMappings
    );
    conversation
        .register_tool_calls(vec![mapping(provider_call_id, call_seed)])
        .expect("register tool call mapping");
    conversation
        .append_tool_response(
            message_id(result_seed),
            tool_response(provider_call_id, "tool result"),
        )
        .expect("append complete tool result");
    assert_eq!(
        conversation.pending().expect("pending").phase(),
        PendingTurnPhase::AwaitingAssistant
    );
}

#[test]
fn tool_result_step_boundary_accepts_injected_user_and_metadata() {
    let mut conversation = conversation();
    prepare_closed_tool_step(&mut conversation, 40, 400, 401, 402, 700, "pivot-call");
    let boundary = conversation.head();

    conversation
        .inject_user_message(
            boundary,
            message_id(403),
            user("please refine with this constraint"),
            injection_meta("human"),
        )
        .expect("inject user at closed tool-result boundary");
    conversation
        .inject_user_message(
            boundary,
            message_id(404),
            user("and keep it short"),
            injection_meta("coordinator"),
        )
        .expect("inject a second user at the same step boundary");

    let pending = conversation.pending().expect("pending after injection");
    assert_eq!(
        pending
            .messages()
            .iter()
            .map(|message| message.payload().role)
            .collect::<Vec<_>>(),
        vec![
            Role::User,
            Role::Assistant,
            Role::Tool,
            Role::User,
            Role::User
        ]
    );
    let injected = &pending.messages()[3];
    assert_eq!(
        injected.meta().expect("injection meta").source(),
        Some("human")
    );
    assert_eq!(
        injected.meta().expect("injection meta").extra()["pivot_id"],
        json!("human")
    );

    assert_eq!(
        freeze_response(
            &mut conversation,
            assistant_response(vec![text("final")], 8, 3, StopReason::EndTurn, "req-final"),
            405,
        ),
        AssistantFinish::ReadyToCommit
    );
    conversation
        .commit_pending(TurnMeta::default())
        .expect("commit injected turn");

    let turn = &conversation.turns()[0];
    assert_eq!(
        turn.messages()
            .iter()
            .map(|message| message.payload().role)
            .collect::<Vec<_>>(),
        vec![
            Role::User,
            Role::Assistant,
            Role::Tool,
            Role::User,
            Role::User,
            Role::Assistant
        ]
    );
    assert_eq!(
        turn.messages()[3].meta().and_then(MessageMeta::source),
        Some("human")
    );
    assert_eq!(
        turn.messages()[4].meta().and_then(MessageMeta::source),
        Some("coordinator")
    );
    assert_eq!(turn.pairings().len(), 1);
    assert_eq!(turn.pairings()[0].call_id(), call_id(700));
    assert_eq!(turn.pairings()[0].result_msg(), message_id(402));
}

#[test]
fn pure_text_turn_cannot_receive_same_turn_injection() {
    let mut conversation = conversation();
    begin(&mut conversation, 50, 500);
    assert_eq!(
        freeze_response(
            &mut conversation,
            assistant_response(vec![text("done")], 3, 1, StopReason::EndTurn, "req-text"),
            501,
        ),
        AssistantFinish::ReadyToCommit
    );
    let before = pending_view(&conversation);
    let error = conversation
        .inject_user_message(
            conversation.head(),
            message_id(502),
            user("late pivot"),
            injection_meta("human"),
        )
        .expect_err("pure text final turn has no pending step boundary");

    assert_eq!(
        error,
        ConversationError::PendingTurn(PendingTurnError::InvalidTransition {
            operation: "inject a user message",
            expected: "awaiting_assistant after closed tool results",
            actual: PendingTurnPhase::ReadyToCommit,
        })
    );
    assert_eq!(pending_view(&conversation), before);

    conversation
        .commit_pending(TurnMeta::default())
        .expect("commit original text turn");
    conversation
        .begin_turn(super::turn_id(51), message_id(502), user("late pivot"))
        .expect("the pivot can become the next turn's initial user");
    assert_eq!(
        conversation
            .pending()
            .expect("new pending")
            .messages()
            .len(),
        1
    );
}

#[test]
fn injected_payload_must_be_user_role() {
    let mut conversation = conversation();
    prepare_closed_tool_step(&mut conversation, 60, 600, 601, 602, 701, "role-call");
    let before = pending_view(&conversation);
    let error = conversation
        .inject_user_message(
            conversation.head(),
            message_id(603),
            Message {
                role: Role::Assistant,
                content: vec![text("not a user")],
            },
            injection_meta("bad-role"),
        )
        .expect_err("assistant role is rejected");

    assert_eq!(
        error,
        ConversationError::PendingTurn(PendingTurnError::InvalidUserRole {
            actual: Role::Assistant,
        })
    );
    assert_eq!(pending_view(&conversation), before);
}

#[test]
fn stale_and_foreign_boundaries_are_rejected_before_injection() {
    let mut stale_source = conversation();
    let stale = stale_source.head();
    begin(&mut stale_source, 70, 700);
    freeze_response(
        &mut stale_source,
        assistant_response(vec![text("done")], 1, 1, StopReason::EndTurn, "req-one"),
        701,
    );
    stale_source
        .commit_pending(TurnMeta::default())
        .expect("advance structural version");
    prepare_closed_tool_step(&mut stale_source, 71, 710, 711, 712, 702, "stale-call");
    let before = pending_view(&stale_source);

    let error = stale_source
        .inject_user_message(
            stale,
            message_id(713),
            user("pivot"),
            injection_meta("stale"),
        )
        .expect_err("old token is stale");
    assert_eq!(
        error,
        ConversationError::Boundary(BoundaryError::StaleBoundary {
            boundary_version: 0,
            current_version: 1,
        })
    );
    assert_eq!(pending_view(&stale_source), before);

    let foreign = Conversation::new(
        ConversationId::new(uuid::Uuid::from_u128(super::UUID_BASE + 9_999)),
        ConversationConfig::default(),
    );
    let foreign_boundary = foreign.head();
    let error = stale_source
        .inject_user_message(
            foreign_boundary,
            message_id(714),
            user("pivot"),
            injection_meta("foreign"),
        )
        .expect_err("foreign token is rejected");
    assert_eq!(
        error,
        ConversationError::Boundary(BoundaryError::OwnerMismatch {
            expected: stale_source.id(),
            actual: foreign.id(),
        })
    );
    assert_eq!(pending_view(&stale_source), before);
}

#[test]
fn redo_suffix_boundary_cannot_be_used_as_a_pending_step_boundary() {
    let mut conversation = conversation();
    for seed in [120, 121] {
        begin(&mut conversation, seed, seed * 10);
        freeze_response(
            &mut conversation,
            assistant_response(vec![text("done")], 1, 1, StopReason::EndTurn, "req-text"),
            seed * 10 + 1,
        );
        conversation
            .commit_pending(TurnMeta::default())
            .expect("commit text turn");
    }

    let first_turn_boundary = conversation
        .boundary_after(super::turn_id(120))
        .expect("boundary after first turn");
    conversation
        .revert_to(first_turn_boundary)
        .expect("revert to expose redo suffix");
    let redo_suffix_boundary = conversation
        .valid_boundaries()
        .into_iter()
        .find(|boundary| boundary.turn_count() == 2)
        .expect("fresh redo suffix boundary");

    prepare_closed_tool_step(
        &mut conversation,
        122,
        1_220,
        1_221,
        1_222,
        705,
        "redo-call",
    );
    let before = pending_view(&conversation);
    let error = conversation
        .inject_user_message(
            redo_suffix_boundary,
            message_id(1_223),
            user("pivot"),
            injection_meta("redo"),
        )
        .expect_err("redo suffix is not the pending head");

    assert_eq!(
        error,
        ConversationError::Boundary(BoundaryError::NotCurrentHead {
            boundary_turn_count: 2,
            head_turn_count: 1,
        })
    );
    assert_eq!(pending_view(&conversation), before);
}

#[test]
fn active_partial_and_open_call_reject_injection_atomically() {
    let mut active = conversation();
    begin(&mut active, 80, 800);
    active
        .start_assistant()
        .expect("active partial assistant starts");
    let active_before = pending_view(&active);
    let error = active
        .inject_user_message(
            active.head(),
            message_id(801),
            user("interrupt"),
            injection_meta("human"),
        )
        .expect_err("active partial cannot be interrupted");
    assert_eq!(
        error,
        ConversationError::PendingTurn(PendingTurnError::InvalidTransition {
            operation: "inject a user message",
            expected: "awaiting_assistant after closed tool results",
            actual: PendingTurnPhase::AssistantInProgress,
        })
    );
    assert_eq!(pending_view(&active), active_before);

    let mut open = conversation();
    begin(&mut open, 81, 810);
    freeze_response(
        &mut open,
        assistant_response(
            vec![super::tool_use("open-call")],
            2,
            1,
            StopReason::ToolUse,
            "req-open",
        ),
        811,
    );
    open.register_tool_calls(vec![mapping("open-call", 703)])
        .expect("register open call");
    let open_before = pending_view(&open);
    let error = open
        .inject_user_message(
            open.head(),
            message_id(812),
            user("interrupt"),
            injection_meta("human"),
        )
        .expect_err("open call must close first");
    assert_eq!(
        error,
        ConversationError::PendingTurn(PendingTurnError::InvalidTransition {
            operation: "inject a user message",
            expected: "awaiting_assistant after closed tool results",
            actual: PendingTurnPhase::AwaitingToolResults,
        })
    );
    assert_eq!(pending_view(&open), open_before);
}

#[test]
fn duplicate_message_id_rejects_injection_without_mutation() {
    let mut conversation = conversation();
    prepare_closed_tool_step(&mut conversation, 90, 900, 901, 902, 704, "duplicate-call");
    let before = pending_view(&conversation);
    let error = conversation
        .inject_user_message(
            conversation.head(),
            message_id(900),
            user("duplicate id"),
            injection_meta("human"),
        )
        .expect_err("pending message ids remain unique");

    assert_eq!(
        error,
        ConversationError::PendingTurn(PendingTurnError::DuplicateMessageId {
            message_id: message_id(900),
        })
    );
    assert_eq!(pending_view(&conversation), before);
}

#[test]
fn user_before_any_assistant_is_not_a_step_boundary() {
    let mut conversation = conversation();
    begin(&mut conversation, 100, 1_000);
    let before = pending_view(&conversation);
    let error = conversation
        .inject_user_message(
            conversation.head(),
            message_id(1_001),
            user("too early"),
            injection_meta("human"),
        )
        .expect_err("initial user is not a tool-result step boundary");

    assert_eq!(
        error,
        ConversationError::PendingTurn(PendingTurnError::InvalidTransition {
            operation: "inject a user message",
            expected: "a closed tool-result step boundary",
            actual: PendingTurnPhase::AwaitingAssistant,
        })
    );
    assert_eq!(pending_view(&conversation), before);
}
