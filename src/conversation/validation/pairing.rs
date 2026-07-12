//! Explicit tool-pairing to content-block cross-validation.

use super::sequence::BlockFacts;
use crate::conversation::{CommitError, MessageId, PairingMessageKind, ToolCallId, turn::TurnData};
use std::collections::HashSet;

/// Cross-checks every explicit pairing against call and result content.
pub(super) fn validate_pairings(
    data: &TurnData,
    facts: &BlockFacts,
    current_messages: &HashSet<MessageId>,
    committed_messages: &HashSet<MessageId>,
) -> Result<(), CommitError> {
    validate_pairing_references(data, facts, current_messages, committed_messages)?;
    let paired_provider_ids = resolve_provider_call_ids(data, facts)?;

    for (pairing, provider_call_id) in data.pairings.iter().zip(&paired_provider_ids) {
        let result_msg = pairing
            .result_msg
            .expect("pairing references are complete before provider-id resolution");

        let Some(call_msg) = facts.calls.get(provider_call_id).copied() else {
            return Err(CommitError::OrphanToolPairing {
                call_id: pairing.call_id,
                provider_call_id: provider_call_id.to_owned(),
            });
        };
        if call_msg != pairing.call_msg {
            return Err(CommitError::PairingMessageMismatch {
                call_id: pairing.call_id,
                provider_call_id: provider_call_id.to_owned(),
                kind: PairingMessageKind::Call,
                expected: call_msg,
                actual: pairing.call_msg,
            });
        }

        let Some(actual_result_msg) = facts.results.get(provider_call_id).copied() else {
            return Err(CommitError::DanglingProviderCall {
                provider_call_id: provider_call_id.to_owned(),
                call_msg,
            });
        };
        if actual_result_msg != result_msg {
            return Err(CommitError::PairingMessageMismatch {
                call_id: pairing.call_id,
                provider_call_id: provider_call_id.to_owned(),
                kind: PairingMessageKind::Result,
                expected: actual_result_msg,
                actual: result_msg,
            });
        }
    }

    for provider_call_id in &facts.call_order {
        if !paired_provider_ids
            .iter()
            .any(|paired| paired == provider_call_id)
        {
            return Err(CommitError::MissingToolPairing {
                provider_call_id: provider_call_id.clone(),
            });
        }
    }
    Ok(())
}

/// Validates pairing message references and rejects incomplete result sides.
fn validate_pairing_references(
    data: &TurnData,
    facts: &BlockFacts,
    current_messages: &HashSet<MessageId>,
    committed_messages: &HashSet<MessageId>,
) -> Result<(), CommitError> {
    for pairing in &data.pairings {
        validate_pairing_reference(
            pairing.call_id,
            PairingMessageKind::Call,
            pairing.call_msg,
            current_messages,
            committed_messages,
        )?;
        let Some(result_msg) = pairing.result_msg else {
            let Some(provider_call_id) = pairing
                .provider_call_id
                .as_deref()
                .filter(|provider_call_id| !provider_call_id.is_empty())
            else {
                return Err(CommitError::MissingProviderCallId {
                    call_id: pairing.call_id,
                });
            };
            let call_msg = facts
                .calls
                .get(provider_call_id)
                .copied()
                .unwrap_or(pairing.call_msg);
            return Err(CommitError::DanglingProviderCall {
                provider_call_id: provider_call_id.to_owned(),
                call_msg,
            });
        };
        validate_pairing_reference(
            pairing.call_id,
            PairingMessageKind::Result,
            result_msg,
            current_messages,
            committed_messages,
        )?;
    }
    Ok(())
}

/// Resolves optional provider ids only when message anchors make one answer unique.
fn resolve_provider_call_ids(
    data: &TurnData,
    facts: &BlockFacts,
) -> Result<Vec<String>, CommitError> {
    let mut claimed = HashSet::with_capacity(data.pairings.len());
    for pairing in &data.pairings {
        if let Some(provider_call_id) = &pairing.provider_call_id {
            if provider_call_id.is_empty() {
                return Err(CommitError::MissingProviderCallId {
                    call_id: pairing.call_id,
                });
            }
            if !claimed.insert(provider_call_id.clone()) {
                return Err(CommitError::DuplicateProviderCallId {
                    provider_call_id: provider_call_id.clone(),
                });
            }
        }
    }

    let mut resolved = Vec::with_capacity(data.pairings.len());
    for pairing in &data.pairings {
        if let Some(provider_call_id) = &pairing.provider_call_id {
            resolved.push(provider_call_id.clone());
            continue;
        }

        let result_msg = pairing
            .result_msg
            .expect("pairing references are complete before provider-id resolution");
        let mut candidates = facts.call_order.iter().filter(|provider_call_id| {
            !claimed.contains(*provider_call_id)
                && facts.calls.get(*provider_call_id) == Some(&pairing.call_msg)
                && facts.results.get(*provider_call_id) == Some(&result_msg)
        });
        let Some(provider_call_id) = candidates.next().cloned() else {
            return Err(CommitError::MissingProviderCallId {
                call_id: pairing.call_id,
            });
        };
        if candidates.next().is_some() {
            return Err(CommitError::MissingProviderCallId {
                call_id: pairing.call_id,
            });
        }
        claimed.insert(provider_call_id.clone());
        resolved.push(provider_call_id);
    }
    Ok(resolved)
}

/// Distinguishes an unknown pairing reference from a cross-turn reference.
fn validate_pairing_reference(
    call_id: ToolCallId,
    kind: PairingMessageKind,
    message_id: MessageId,
    current_messages: &HashSet<MessageId>,
    committed_messages: &HashSet<MessageId>,
) -> Result<(), CommitError> {
    if current_messages.contains(&message_id) {
        return Ok(());
    }
    if committed_messages.contains(&message_id) {
        return Err(CommitError::CrossTurnPairing {
            call_id,
            kind,
            message_id,
        });
    }
    Err(CommitError::UnknownPairingMessage {
        call_id,
        kind,
        message_id,
    })
}
