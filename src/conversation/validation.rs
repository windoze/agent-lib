//! Closed-turn validation and the canonical role/tool state machine.
//!
//! This module owns the only certificate that can materialize a live
//! [`Turn`](super::Turn). Both in-memory drafts and deserialized [`TurnData`]
//! therefore pass through the same I1--I4 checks.

mod identity;
mod pairing;
mod sequence;

use self::{
    identity::{
        committed_message_ids, validate_completion, validate_message_ids, validate_parent,
        validate_tool_call_ids, validate_turn_identity,
    },
    pairing::validate_pairings,
    sequence::validate_role_sequence,
};
use crate::conversation::{CommitError, Turn, TurnId, turn::TurnData};

/// A validator-issued certificate whose field cannot be forged by siblings.
pub(super) struct ValidatedTurnData(TurnData);

impl ValidatedTurnData {
    /// Returns certified data to the `Turn` module for final materialization.
    pub(super) fn into_data(self) -> TurnData {
        self.0
    }
}

/// Validates a candidate against its conversation and returns a live turn.
pub(super) fn validate_turn_data(
    data: TurnData,
    committed: &[Turn],
    expected_parent: Option<TurnId>,
) -> Result<Turn, CommitError> {
    validate_completion(&data)?;
    validate_turn_identity(&data, committed)?;
    validate_parent(&data, expected_parent)?;

    let committed_messages = committed_message_ids(committed);
    let current_messages = validate_message_ids(&data, &committed_messages)?;
    validate_tool_call_ids(&data, committed)?;

    let facts = validate_role_sequence(&data)?;
    validate_pairings(&data, &facts, &current_messages, &committed_messages)?;

    Ok(Turn::from_validated(ValidatedTurnData(data)))
}

#[cfg(test)]
mod tests;
