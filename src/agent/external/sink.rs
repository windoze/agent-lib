//! Optional non-blocking bypass for surfacing external-agent events live.
//!
//! The blocking continuation of an external session is always expressed through
//! the [`ExternalSessionResult`](super::ExternalSessionResult): a handler
//! advances the session to its next decision point, buffers every event it saw
//! as a sequenced [`ExternalObservedEvent`](super::ExternalObservedEvent) in
//! `observations`, and the
//! [`ExternalAgentMachine`](super::ExternalAgentMachine) replays them as
//! [`Notification::ExternalAgent`](crate::agent::Notification::ExternalAgent) on
//! resume (design §5.5). That path is exact-once — deduplicated per event by
//! [`seq`](super::ExternalObservedEvent::seq) — and drives control flow.
//!
//! Some hosts additionally want to *see* tokens, commands, or patches **before**
//! the decision point — a live UI tail. Design §5.5 reserves a separate,
//! non-blocking bypass for that: a handler may forward each event to an
//! [`ExternalEventSink`] as it arrives. This trait is the interface placeholder
//! for that bypass. It is deliberately decoupled from the sans-io machine step:
//! the machine never holds a sink, because only a `Requirement` may block a
//! continuation and the sink must not.

use super::ExternalAgentEvent;

/// A non-blocking, best-effort sink for [`ExternalAgentEvent`]s surfaced live.
///
/// A handler running an external session may offer each event to a sink as the
/// runtime produces it, so a UI can tail tokens/commands/patches before the
/// blocking decision point is reached. The bypass is **discardable**:
///
/// - It must **never block the continuation.** Only a
///   [`Requirement`](crate::agent::Requirement) may pause an agent; a sink is a
///   side channel, so an implementation should drop rather than back-pressure.
/// - It may be **skipped entirely.** When no sink is attached (the common case)
///   the buffered `observations` on the next
///   [`ExternalSessionResult`](super::ExternalSessionResult) remain the single
///   source of truth, replayed exactly once as
///   [`Notification::ExternalAgent`](crate::agent::Notification::ExternalAgent).
/// - Events offered here carry **untrusted** runtime output and never widen the
///   host's permission boundary.
///
/// This first version ships only the interface and a no-op
/// [`DiscardEventSink`]; wiring a concrete realtime source lands with the
/// scheduler work in a later milestone.
///
/// # Examples
///
/// ```
/// use agent_lib::agent::external::{DiscardEventSink, ExternalAgentEvent, ExternalEventSink};
///
/// let sink = DiscardEventSink;
/// sink.emit(&ExternalAgentEvent::SessionCompleted);
/// ```
pub trait ExternalEventSink: Send + Sync {
    /// Offers one event to the sink as the runtime produces it.
    ///
    /// Implementations must return promptly and must not block the session's
    /// continuation; dropping the event is an acceptable outcome under load.
    fn emit(&self, event: &ExternalAgentEvent);
}

/// An [`ExternalEventSink`] that discards every event.
///
/// This is the default "no realtime bypass" behavior: the buffered
/// `observations` returned at each decision point remain the single, exact-once
/// source of external-agent notifications.
#[derive(Clone, Copy, Debug, Default)]
pub struct DiscardEventSink;

impl ExternalEventSink for DiscardEventSink {
    fn emit(&self, _event: &ExternalAgentEvent) {}
}

#[cfg(test)]
mod tests {
    use super::{DiscardEventSink, ExternalEventSink};
    use crate::agent::external::ExternalAgentEvent;

    #[test]
    fn discard_sink_accepts_and_drops_events() {
        let sink = DiscardEventSink;
        // A discarding sink is a no-op bypass: it never blocks and never panics,
        // regardless of the event offered.
        sink.emit(&ExternalAgentEvent::SessionStarted { session_id: None });
        sink.emit(&ExternalAgentEvent::TextDelta {
            text: "hello".to_owned(),
        });
        sink.emit(&ExternalAgentEvent::SessionCompleted);

        // Usable behind a trait object, mirroring how a handler would hold one.
        let dynamic: &dyn ExternalEventSink = &sink;
        dynamic.emit(&ExternalAgentEvent::SessionCompleted);
    }
}
