//! Data-only persistence boundaries for Conversation state.
//!
//! [`ConversationSnapshot`] records committed facts at a consistency point.
//! [`ConversationRows`] decomposes the same facts into DB-neutral parent-tree
//! rows with immutable Turn/message facts and per-Conversation association
//! rows. Restore revalidates those data facts before constructing live history,
//! projection, and derived indexes; runtime-only pending state, accumulators,
//! clients, registries, and strategy objects stay outside the persisted shape.

mod rows;
mod snapshot;

pub use rows::{
    ArtifactRecord, CONVERSATION_ROW_SCHEMA_VERSION, ConversationLineageTurnRecord,
    ConversationRecord, ConversationRowInsertSet, ConversationRows, ConversationTurnRecord,
    MessageRecord, ProjectionRecord, ProjectionSpanKind, ProjectionSpanRecord, ToolPairingRecord,
    TurnRecord,
};
pub use snapshot::{
    CONVERSATION_SNAPSHOT_SCHEMA_VERSION, ConversationSnapshot, ConversationSnapshotHistory,
};

#[cfg(test)]
mod tests;
