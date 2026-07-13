//! Data-only persistence boundaries for Conversation state.
//!
//! Snapshotting is intentionally separate from restore. A
//! [`ConversationSnapshot`] records committed facts at a consistency point, but
//! a later restore task must still validate those facts before constructing
//! live runtime state.

mod snapshot;

pub use snapshot::{
    CONVERSATION_SNAPSHOT_SCHEMA_VERSION, ConversationSnapshot, ConversationSnapshotHistory,
};

#[cfg(test)]
mod tests;
