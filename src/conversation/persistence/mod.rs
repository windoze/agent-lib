//! Data-only persistence boundaries for Conversation state.
//!
//! [`ConversationSnapshot`] records committed facts at a consistency point.
//! Restore revalidates those data facts before constructing live history,
//! projection, and derived indexes; runtime-only pending state, accumulators,
//! clients, registries, and strategy objects stay outside the persisted shape.

mod snapshot;

pub use snapshot::{
    CONVERSATION_SNAPSHOT_SCHEMA_VERSION, ConversationSnapshot, ConversationSnapshotHistory,
};

#[cfg(test)]
mod tests;
