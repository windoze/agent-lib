//! First-class mailbox vertical feature (design `external-agent.md` §3.5).
//!
//! A [`Mailbox`] is the *optional* directed-message layer that complements the
//! broadcast [`Blackboard`](super::Blackboard) and the stateful
//! [`Plan`](super::Plan): it carries agent-to-agent direct messages. External
//! runtimes (Claude Code Agent Teams, etc.) ship their own private JSON inboxes,
//! but a bridged agent must route through **this** library primitive rather than
//! writing an external runtime's private mailbox directly (design §3.5), so the
//! same protocol is observable, testable, and replayable across runtimes.
//!
//! The model is a per-recipient inbox of immutable, append-only messages. Each
//! message gets a mailbox-global, monotonically increasing
//! [`seq`](MailMessage::seq) so a recipient can keep a resumable read cursor. Like
//! the blackboard, delivery is best-effort and in-memory — no acknowledgements or
//! retries.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Mutex;

/// A single directed mailbox message.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MailMessage {
    /// Mailbox-global, monotonically increasing sequence number.
    pub seq: u64,
    /// Sender label.
    pub from: String,
    /// Recipient label.
    pub to: String,
    /// Message body.
    pub text: String,
}

/// The mutable mailbox state: per-recipient inboxes plus the shared sequence.
#[derive(Debug, Default)]
struct MailboxState {
    /// Next sequence number to assign.
    next_seq: u64,
    /// Recipient label -> ordered inbox.
    inboxes: BTreeMap<String, Vec<MailMessage>>,
}

/// A live, shareable directed mailbox (design §3.5).
///
/// Wrap it in an `Arc` to share across agents. Every [`send`](Self::send) is a
/// single-writer transaction, so concurrent sends receive distinct, monotonic
/// sequence numbers.
#[derive(Debug, Default)]
pub struct Mailbox {
    state: Mutex<MailboxState>,
}

impl Mailbox {
    /// Creates an empty mailbox.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Locks the state, recovering the guard even if a prior holder panicked
    /// (an append-only inbox has no invariant a panic could corrupt).
    fn state(&self) -> std::sync::MutexGuard<'_, MailboxState> {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    /// Delivers a message from `from` to `to`'s inbox and returns its sequence.
    pub fn send(
        &self,
        from: impl Into<String>,
        to: impl Into<String>,
        text: impl Into<String>,
    ) -> u64 {
        let from = from.into();
        let to = to.into();
        let text = text.into();
        let mut state = self.state();
        let seq = state.next_seq;
        state.next_seq += 1;
        state
            .inboxes
            .entry(to.clone())
            .or_default()
            .push(MailMessage {
                seq,
                from,
                to,
                text,
            });
        seq
    }

    /// Returns a snapshot of `recipient`'s whole inbox, in delivery order.
    #[must_use]
    pub fn inbox(&self, recipient: &str) -> Vec<MailMessage> {
        self.state()
            .inboxes
            .get(recipient)
            .cloned()
            .unwrap_or_default()
    }

    /// Returns `recipient`'s messages with a sequence at or after `from`.
    ///
    /// A recipient advances its cursor to `last.seq + 1` to read only new mail.
    #[must_use]
    pub fn read_from(&self, recipient: &str, from: u64) -> Vec<MailMessage> {
        self.state()
            .inboxes
            .get(recipient)
            .into_iter()
            .flatten()
            .filter(|message| message.seq >= from)
            .cloned()
            .collect()
    }
}
