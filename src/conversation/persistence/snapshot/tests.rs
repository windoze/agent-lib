//! Long-chain restore validation, rebuild, and drop tests (M3-4).
//!
//! Recursive parent-chain walks overflow the default test-thread stack at
//! this chain length, so these fixtures prove the restore gate, the history
//! rebuild, and history destruction all stay iterative on long lineages.

use super::{raw_turn_index, validate_parent_graph};
use crate::{
    conversation::{
        ConversationMessage, MessageId, RestoreError, Turn, TurnId, TurnMeta,
        history::History,
        turn::{TurnCompletion, TurnData},
        validation,
    },
    model::message::{Message, Role},
};
use uuid::Uuid;

/// Chain length large enough that recursive validation, rebuild, or drop
/// would overflow the default test-thread stack.
const CHAIN_LEN: usize = 100_000;

const UUID_BASE: u128 = 0x018f_0d9c_7b6a_7c12_8f90_0000_0000_0000;

/// Creates one deterministic external Turn identity.
fn turn_id(seed: u128) -> TurnId {
    TurnId::new(Uuid::from_u128(UUID_BASE + seed))
}

/// Creates one deterministic external Message identity.
fn message_id(seed: u128) -> MessageId {
    MessageId::new(Uuid::from_u128(UUID_BASE + seed))
}

/// Creates one minimal two-message turn fact with the smallest legal payload.
fn turn_data(seed: u128, parent: Option<TurnId>) -> TurnData {
    TurnData {
        id: turn_id(seed),
        messages: vec![
            ConversationMessage::new(
                message_id(seed * 2),
                Message {
                    role: Role::User,
                    content: Vec::new(),
                },
            ),
            ConversationMessage::new(
                message_id(seed * 2 + 1),
                Message {
                    role: Role::Assistant,
                    content: Vec::new(),
                },
            ),
        ],
        pairings: Vec::new(),
        parent,
        meta: TurnMeta::default(),
        completion: TurnCompletion::Complete,
    }
}

/// Certifies a linear turn chain in O(n).
///
/// Each link is validated in isolation because the cross-turn identity checks
/// inside the restore gate are quadratic by design (they mirror the commit
/// gate); re-running them per link would make this fixture far too slow. The
/// recursion-free properties under test live in the parent-graph validation
/// and the history rebuild, not in the per-turn identity checks.
fn minimal_chain(len: usize, root_parent: Option<TurnId>) -> Vec<Turn> {
    (0..len)
        .map(|index| {
            let parent = match index.checked_sub(1) {
                Some(previous) => Some(turn_id(previous as u128)),
                None => root_parent,
            };
            let data = turn_data(index as u128, parent);
            validation::validate_turn_data(data, std::iter::empty(), parent)
                .expect("minimal turn fact validates")
        })
        .collect()
}

#[test]
fn parent_graph_validation_handles_a_long_chain_iteratively() {
    let turns = minimal_chain(CHAIN_LEN, None);
    let raw_index = raw_turn_index(&turns);
    validate_parent_graph(&turns, &raw_index).expect("long linear chain validates");
}

#[test]
fn parent_graph_validation_reports_a_cycle_spanning_a_long_chain() {
    // Close the chain into one cycle through every link: the root turn points
    // at the tip, so the iterative walk must traverse the whole chain before
    // it meets the first re-visited turn.
    let tip = turn_id((CHAIN_LEN - 1) as u128);
    let turns = minimal_chain(CHAIN_LEN, Some(tip));
    let raw_index = raw_turn_index(&turns);
    let error = validate_parent_graph(&turns, &raw_index)
        .expect_err("a cycle through the whole chain is rejected");
    assert!(
        matches!(
            error,
            RestoreError::ParentCycle { turn_id: reported, .. } if reported == turn_id(0)
        ),
        "expected the cycle to be reported at the re-visited root, got {error:?}"
    );
}

#[test]
fn restored_long_chain_builds_and_drops_iteratively() {
    let turns = minimal_chain(CHAIN_LEN, None);
    let lineage: Vec<TurnId> = turns.iter().map(Turn::id).collect();
    let history = History::from_restored(turns, &lineage, CHAIN_LEN);
    assert_eq!(history.lineage_len(), CHAIN_LEN);
    assert_eq!(history.active_len(), CHAIN_LEN);
    assert_eq!(history.tip_id(), Some(turn_id((CHAIN_LEN - 1) as u128)));
    // Dropping must unwind the raw cons list and the parent-pointer chain
    // iteratively; recursion over this many links would overflow the stack.
    drop(history);
}

#[test]
fn dropping_a_shared_long_chain_preserves_the_surviving_handle() {
    let turns = minimal_chain(CHAIN_LEN, None);
    let lineage: Vec<TurnId> = turns.iter().map(Turn::id).collect();
    let history = History::from_restored(turns, &lineage, CHAIN_LEN);

    // A fork shares every node, then extends its own lineage so both handles
    // own distinct lineage allocations over the same nodes.
    let mut fork = history
        .shared_prefix(CHAIN_LEN)
        .expect("full prefix shares");
    let tip = turn_id((CHAIN_LEN - 1) as u128);
    let extension = validation::validate_turn_data(
        turn_data(CHAIN_LEN as u128, Some(tip)),
        std::iter::empty(),
        Some(tip),
    )
    .expect("extension turn fact validates");
    fork.append(extension);

    // Dropping the original handle must stop unchaining at the shared links
    // and leave the fork fully usable.
    drop(history);
    assert_eq!(fork.lineage_len(), CHAIN_LEN + 1);
    assert_eq!(fork.tip_id(), Some(turn_id(CHAIN_LEN as u128)));
    assert!(fork.raw_turn(tip).is_some());
    drop(fork);
}
