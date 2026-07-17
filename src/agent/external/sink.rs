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
//! the decision point — a live UI tail. Design §10.1 reserves a separate,
//! non-blocking bypass for that: a handler may forward each observed event to an
//! [`ExternalEventSink`] as it decodes it. The sink is fed the same **sequenced**
//! [`ExternalObservedEvent`](super::ExternalObservedEvent) the handler buffers,
//! so a live consumer and the buffered `observations` share one `seq` line and a
//! host can align or dedup the two channels against a single replay marker. It is
//! deliberately decoupled from the sans-io machine step: the machine never holds
//! a sink, because only a `Requirement` may block a continuation and the sink
//! must not.

use super::ExternalObservedEvent;

/// A non-blocking, best-effort sink for sequenced
/// [`ExternalObservedEvent`]s surfaced live.
///
/// A handler running an external session may offer each observed event to a sink
/// as the runtime produces it, so a UI can tail tokens/commands/patches before
/// the blocking decision point is reached. Each event carries the same
/// [`seq`](super::ExternalObservedEvent::seq) the handler records in the buffered
/// `observations`, so the live tail and the replay stream are aligned on one
/// monotonic marker. The bypass is **discardable**:
///
/// - It must **never block the continuation.** Only a
///   [`Requirement`](crate::agent::Requirement) may pause an agent; a sink is a
///   side channel, so an implementation must return promptly and should drop
///   rather than back-pressure the session.
/// - It may **drop events** freely. The sink offers no delivery guarantee: under
///   load, or when a host never attaches one, skipping events here is expected
///   and correct.
/// - **Exact-once replay is not the sink's job.** Exactly-once, event-by-event
///   delivery is guaranteed solely by
///   [`ExternalSessionResult::observations`](super::ExternalSessionResult) and the
///   [`ExternalAgentMachine`](super::ExternalAgentMachine) replay that dedups them
///   by `seq`. A sink is a lossy live mirror of that authoritative stream, never a
///   substitute for it.
/// - Events offered here carry **untrusted** runtime output and never widen the
///   host's permission boundary.
///
/// This first version ships the interface and a no-op [`DiscardEventSink`];
/// wiring a concrete realtime source lands with the scheduler work in a later
/// milestone.
///
/// # Examples
///
/// ```
/// use agent_lib::agent::external::{
///     DiscardEventSink, ExternalAgentEvent, ExternalEventSink, ExternalObservedEvent,
/// };
///
/// let sink = DiscardEventSink;
/// sink.emit(&ExternalObservedEvent::new(0, ExternalAgentEvent::SessionCompleted));
/// ```
pub trait ExternalEventSink: Send + Sync {
    /// Offers one sequenced observation to the sink as the runtime produces it.
    ///
    /// Implementations must return promptly and must not block the session's
    /// continuation; dropping the event is an acceptable outcome under load. The
    /// `seq` on the observation matches the one the handler buffers for replay,
    /// so a consumer can correlate the live tail with the authoritative
    /// `observations`.
    fn emit(&self, event: &ExternalObservedEvent);
}

/// An [`ExternalEventSink`] that discards every event.
///
/// This is the default "no realtime bypass" behavior: the buffered
/// `observations` returned at each decision point remain the single, exact-once
/// source of external-agent notifications.
#[derive(Clone, Copy, Debug, Default)]
pub struct DiscardEventSink;

impl ExternalEventSink for DiscardEventSink {
    fn emit(&self, _event: &ExternalObservedEvent) {}
}

#[cfg(test)]
mod tests {
    use super::{DiscardEventSink, ExternalEventSink};
    use crate::agent::external::{ExternalAgentEvent, ExternalObservedEvent};
    use std::sync::Mutex;

    fn sample_observations() -> Vec<ExternalObservedEvent> {
        ExternalObservedEvent::unsequenced_for_tests(vec![
            ExternalAgentEvent::SessionStarted { session_id: None },
            ExternalAgentEvent::TextDelta {
                text: "hello".to_owned(),
            },
            ExternalAgentEvent::SessionCompleted,
        ])
    }

    #[test]
    fn discard_sink_accepts_and_drops_events() {
        let sink = DiscardEventSink;
        // A discarding sink is a no-op bypass: it never blocks and never panics,
        // regardless of the sequenced observation offered.
        for observed in sample_observations() {
            sink.emit(&observed);
        }

        // Usable behind a trait object, mirroring how a handler would hold one.
        let dynamic: &dyn ExternalEventSink = &sink;
        dynamic.emit(&ExternalObservedEvent::new(
            7,
            ExternalAgentEvent::SessionCompleted,
        ));
    }

    /// Test-only sink that records every sequenced observation it is offered.
    ///
    /// It exists purely to prove the live bypass is a passive mirror: emitting to
    /// it captures the `seq`-tagged events without touching the buffered
    /// observations a handler would return in `ExternalSessionResult`.
    #[derive(Default)]
    struct CollectingSink {
        seen: Mutex<Vec<ExternalObservedEvent>>,
    }

    impl ExternalEventSink for CollectingSink {
        fn emit(&self, event: &ExternalObservedEvent) {
            self.seen
                .lock()
                .expect("sink mutex poisoned")
                .push(event.clone());
        }
    }

    #[test]
    fn collecting_sink_records_sequenced_events_for_tests() {
        let sink = CollectingSink::default();

        // Simulate a handler's dual-channel loop: it buffers every observation for
        // exact-once replay *and* mirrors it to the live sink. The two channels
        // are independent — feeding the sink must not perturb the buffer.
        let mut buffered: Vec<ExternalObservedEvent> = Vec::new();
        for observed in sample_observations() {
            sink.emit(&observed);
            buffered.push(observed);
        }

        let recorded = sink.seen.lock().expect("sink mutex poisoned").clone();

        // The sink captured exactly the events it was offered, preserving the
        // runtime `seq` order — this is the marker a host aligns replay against.
        assert_eq!(recorded, sample_observations());
        assert_eq!(
            recorded.iter().map(|o| o.seq).collect::<Vec<_>>(),
            vec![0, 1, 2],
        );

        // The buffered observations — the exact-once source of truth — are
        // untouched by the live bypass: they still hold the full stream.
        assert_eq!(buffered, sample_observations());
    }
}
