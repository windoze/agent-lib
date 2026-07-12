use super::{Case, assert_rejected_case};
use crate::conversation::validation::tests::fixtures::{
    conversation, draft, message, message_id, pairing, single_tool_draft, text, tool_call_id,
    tool_result, tool_use, turn_id,
};
use crate::conversation::{
    CommitError, Conversation, ConversationError, PairingMessageKind, turn::ToolPairingData,
};
use crate::model::message::Role;

fn tool_history() -> Conversation {
    let mut history = conversation();
    history
        .commit_draft(single_tool_draft(10, None, 100, 500, "old-call"))
        .expect("seed committed tool turn");
    history
}

#[test]
fn explicit_pairing_mismatches_are_rejected_atomically() {
    let base = single_tool_draft(10, None, 100, 500, "paired-call");

    let mut missing_pairing = base.clone();
    missing_pairing.pairings.clear();

    let mut ambiguous_provider_ids = draft(
        10,
        None,
        vec![
            message(100, Role::User, vec![text("question")]),
            message(
                101,
                Role::Assistant,
                vec![tool_use("ambiguous-a"), tool_use("ambiguous-b")],
            ),
            message(
                102,
                Role::Tool,
                vec![tool_result("ambiguous-a"), tool_result("ambiguous-b")],
            ),
            message(103, Role::Assistant, vec![text("answer")]),
        ],
        vec![
            pairing(500, "ambiguous-a", 101, 102),
            pairing(501, "ambiguous-b", 101, 102),
        ],
    );
    for pairing in &mut ambiguous_provider_ids.pairings {
        pairing.provider_call_id = None;
    }

    let mut pending_pairing = base.clone();
    pending_pairing.pairings[0].result_msg = None;

    let mut orphan_pairing = base.clone();
    orphan_pairing.pairings[0].provider_call_id = Some("unknown-call".to_owned());

    let mut wrong_call_message = base.clone();
    wrong_call_message.pairings[0].call_msg = message_id(100);

    let mut wrong_result_message = base.clone();
    wrong_result_message.pairings[0].result_msg = Some(message_id(103));

    let mut unknown_call_message = base.clone();
    unknown_call_message.pairings[0].call_msg = message_id(999);

    let mut duplicate_provider_pairing = base.clone();
    duplicate_provider_pairing.pairings.push(ToolPairingData {
        call_id: tool_call_id(501),
        ..duplicate_provider_pairing.pairings[0].clone()
    });

    let cases = vec![
        Case {
            name: "missing explicit pairing",
            conversation: conversation(),
            data: missing_pairing,
            expected: ConversationError::Commit(CommitError::MissingToolPairing {
                provider_call_id: "paired-call".to_owned(),
            }),
        },
        Case {
            name: "pairing provider id cannot be inferred unambiguously",
            conversation: conversation(),
            data: ambiguous_provider_ids,
            expected: ConversationError::Commit(CommitError::MissingProviderCallId {
                call_id: tool_call_id(500),
            }),
        },
        Case {
            name: "pairing missing result message",
            conversation: conversation(),
            data: pending_pairing,
            expected: ConversationError::Commit(CommitError::DanglingProviderCall {
                provider_call_id: "paired-call".to_owned(),
                call_msg: message_id(101),
            }),
        },
        Case {
            name: "pairing names no call content",
            conversation: conversation(),
            data: orphan_pairing,
            expected: ConversationError::Commit(CommitError::OrphanToolPairing {
                call_id: tool_call_id(500),
                provider_call_id: "unknown-call".to_owned(),
            }),
        },
        Case {
            name: "pairing points to wrong call message",
            conversation: conversation(),
            data: wrong_call_message,
            expected: ConversationError::Commit(CommitError::PairingMessageMismatch {
                call_id: tool_call_id(500),
                provider_call_id: "paired-call".to_owned(),
                kind: PairingMessageKind::Call,
                expected: message_id(101),
                actual: message_id(100),
            }),
        },
        Case {
            name: "pairing points to wrong result message",
            conversation: conversation(),
            data: wrong_result_message,
            expected: ConversationError::Commit(CommitError::PairingMessageMismatch {
                call_id: tool_call_id(500),
                provider_call_id: "paired-call".to_owned(),
                kind: PairingMessageKind::Result,
                expected: message_id(102),
                actual: message_id(103),
            }),
        },
        Case {
            name: "pairing points to unknown call message",
            conversation: conversation(),
            data: unknown_call_message,
            expected: ConversationError::Commit(CommitError::UnknownPairingMessage {
                call_id: tool_call_id(500),
                kind: PairingMessageKind::Call,
                message_id: message_id(999),
            }),
        },
        Case {
            name: "duplicate provider id in pairing table",
            conversation: conversation(),
            data: duplicate_provider_pairing,
            expected: ConversationError::Commit(CommitError::DuplicateProviderCallId {
                provider_call_id: "paired-call".to_owned(),
            }),
        },
    ];

    for case in cases {
        assert_rejected_case(case);
    }
}

#[test]
fn cross_turn_pairing_references_are_rejected_on_both_sides() {
    let mut cross_call = single_tool_draft(11, Some(turn_id(10)), 200, 501, "new-call");
    cross_call.pairings[0].call_msg = message_id(101);

    let mut cross_result = single_tool_draft(11, Some(turn_id(10)), 200, 501, "new-call");
    cross_result.pairings[0].result_msg = Some(message_id(102));

    for case in [
        Case {
            name: "cross-turn call reference",
            conversation: tool_history(),
            data: cross_call,
            expected: ConversationError::Commit(CommitError::CrossTurnPairing {
                call_id: tool_call_id(501),
                kind: PairingMessageKind::Call,
                message_id: message_id(101),
            }),
        },
        Case {
            name: "cross-turn result reference",
            conversation: tool_history(),
            data: cross_result,
            expected: ConversationError::Commit(CommitError::CrossTurnPairing {
                call_id: tool_call_id(501),
                kind: PairingMessageKind::Result,
                message_id: message_id(102),
            }),
        },
    ] {
        assert_rejected_case(case);
    }
}
