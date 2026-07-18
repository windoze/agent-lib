//! First-class blackboard vertical feature (design `agent-layer.md` §6.4).
//!
//! A [`Blackboard`] is an "agent 聊天群": an append-only, ordered message log with
//! **no enforcement mechanism** — no locks, no claims, no CAS. Any agent may
//! [`post`](Blackboard::post) a message or [`read_from`](Blackboard::read_from) a
//! cursor; who read what, and whether they act on it, the blackboard does not
//! track. Scenarios that need mutable task state and claim semantics use the
//! [`Plan`](super::Plan) instead (design §6.2).
//!
//! The modeled invariants (design §6.4):
//!
//! - **append-only.** History is immutable; there is no delete or overwrite path.
//! - **ordered / monotonic.** Each channel assigns messages a zero-based
//!   [`offset`](BoardMessage::offset) that increases by one per post, so a reader
//!   holding a cursor sees a stable, gap-free order.
//! - **namespaced.** Messages live in independent `channel`s so unrelated topics
//!   do not drown each other out.
//! - **attributed.** Every message records its `sender` so a reader can filter or
//!   attribute it.
//! - **best-effort.** The blackboard is an auxiliary channel, not the critical
//!   coordination path (that is the plan); it stores messages in memory and never
//!   acknowledges, retries, or dedupes.
//!
//! The live [`Blackboard`] keeps its channels behind a `Mutex` so it can be shared
//! (`Arc`) across agents. [`BoardMessage`] is the serde-friendly data shape.

use crate::agent::id::BlackboardId;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Mutex;

/// The channel a message with no explicit channel is posted to / read from.
pub const DEFAULT_CHANNEL: &str = "default";

/// A single append-only blackboard message.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoardMessage {
    /// Zero-based, monotonically increasing position within its channel.
    ///
    /// The offset is the message's logical clock: within a channel it orders
    /// messages and lets a reader keep a resumable cursor.
    pub offset: u64,
    /// Channel the message was posted to.
    pub channel: String,
    /// Author label of the message.
    pub sender: String,
    /// Message body.
    pub text: String,
}

/// A serde-friendly, data-only snapshot of a whole [`Blackboard`] (design §6.4).
///
/// The snapshot captures the board [`id`](Self::id) plus every channel's ordered
/// log, so restoring it with [`Blackboard::from_snapshot`] reproduces the board
/// identity, the channel set, and each message's offset. The type is data only:
/// it holds no lock and no runtime handle, so it is safe to persist.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlackboardSnapshot {
    /// Identity of the blackboard.
    pub id: BlackboardId,
    /// Channel name -> ordered message log. Only channels that hold at least one
    /// message are present, matching [`Blackboard::channels_list`].
    #[serde(default)]
    pub channels: BTreeMap<String, Vec<BoardMessage>>,
}

/// A live, shareable append-only blackboard (design §6.4).
///
/// Every channel is an independent, ordered log. Wrap the blackboard in an `Arc`
/// to share it across agents; each operation is a single-writer transaction so
/// concurrent posts still receive distinct, monotonic offsets within a channel.
#[derive(Debug)]
pub struct Blackboard {
    id: BlackboardId,
    channels: Mutex<BTreeMap<String, Vec<BoardMessage>>>,
}

impl Blackboard {
    /// Creates an empty blackboard for `id`.
    #[must_use]
    pub fn new(id: BlackboardId) -> Self {
        Self {
            id,
            channels: Mutex::new(BTreeMap::new()),
        }
    }

    /// Rebuilds a blackboard from a data-only [`BlackboardSnapshot`].
    ///
    /// The restored board keeps the snapshot's identity and every channel log
    /// verbatim, so a reader holding a prior offset cursor still sees the same
    /// stable, gap-free order and a fresh [`post`](Self::post) continues from the
    /// channel's current length.
    #[must_use]
    pub fn from_snapshot(snapshot: BlackboardSnapshot) -> Self {
        Self {
            id: snapshot.id,
            channels: Mutex::new(snapshot.channels),
        }
    }

    /// Returns the blackboard identity.
    #[must_use]
    pub const fn id(&self) -> BlackboardId {
        self.id
    }

    /// Locks the channel map, recovering the guard even if a prior holder
    /// panicked (an append-only log has no invariant a panic could corrupt).
    fn channels(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, Vec<BoardMessage>>> {
        self.channels
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    /// Appends a message from `sender` to `channel` and returns its offset.
    ///
    /// Offsets start at `0` within each channel and increase by one per message.
    pub fn post(
        &self,
        channel: impl Into<String>,
        sender: impl Into<String>,
        text: impl Into<String>,
    ) -> u64 {
        let channel = channel.into();
        let sender = sender.into();
        let text = text.into();
        let mut channels = self.channels();
        let log = channels.entry(channel.clone()).or_default();
        let offset = log.len() as u64;
        log.push(BoardMessage {
            offset,
            channel,
            sender,
            text,
        });
        offset
    }

    /// Appends a message from `sender` to the [`DEFAULT_CHANNEL`].
    pub fn post_default(&self, sender: impl Into<String>, text: impl Into<String>) -> u64 {
        self.post(DEFAULT_CHANNEL, sender, text)
    }

    /// Reads all messages in `channel` at `from` and beyond, in order.
    ///
    /// An unknown channel reads as empty. The returned messages keep their
    /// original offsets so a reader can advance its cursor to
    /// `last.offset + 1`.
    #[must_use]
    pub fn read_from(&self, channel: &str, from: u64) -> Vec<BoardMessage> {
        self.channels()
            .get(channel)
            .into_iter()
            .flatten()
            .filter(|message| message.offset >= from)
            .cloned()
            .collect()
    }

    /// Reads all [`DEFAULT_CHANNEL`] messages at `from` and beyond.
    #[must_use]
    pub fn read_default_from(&self, from: u64) -> Vec<BoardMessage> {
        self.read_from(DEFAULT_CHANNEL, from)
    }

    /// Returns a snapshot of every message in `channel`, in order.
    #[must_use]
    pub fn snapshot(&self, channel: &str) -> Vec<BoardMessage> {
        self.channels().get(channel).cloned().unwrap_or_default()
    }

    /// Returns the channel names that currently hold at least one message.
    #[must_use]
    pub fn channels_list(&self) -> Vec<String> {
        self.channels().keys().cloned().collect()
    }

    /// Captures a data-only [`BlackboardSnapshot`] of the whole board: its
    /// identity plus every channel's ordered log.
    ///
    /// Restoring the snapshot with [`from_snapshot`](Self::from_snapshot)
    /// reproduces the board identity, the channel set, and each message's offset.
    #[must_use]
    pub fn snapshot_all(&self) -> BlackboardSnapshot {
        BlackboardSnapshot {
            id: self.id,
            channels: self.channels().clone(),
        }
    }
}
