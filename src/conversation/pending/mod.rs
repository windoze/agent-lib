//! Transaction-local message state that has not entered closed history.
//!
//! A [`PendingMessage`] owns the only mutable Client accumulator for one LLM
//! response. It exposes no partial [`Message`](crate::model::message::Message):
//! only a successful, caller-identified freeze produces a [`FrozenMessage`].

mod message;

pub use message::{FrozenMessage, PendingMessage};
