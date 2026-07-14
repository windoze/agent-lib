//! Deterministic concurrency observation tools for the agent effect layer.
//!
//! The reference driver fulfils the requirements a scope can serve concurrently
//! through a [`FuturesUnordered`](futures::stream::FuturesUnordered) set
//! (`fulfill_batch`, migration decision B). A test that wants to reason about
//! *overlap* — peak in-flight calls, or a stable out-of-order completion — needs
//! to shape when each concurrent call yields and resumes without leaning on a
//! real clock. Wall-clock sleeps are both slow and racy, so this module builds
//! the overlap out of cooperative scheduling primitives instead:
//!
//! - [`Delay`] is an `await`-able that yields the executor a fixed number of
//!   times before completing. Giving two concurrent calls different tick counts
//!   makes the shorter one finish first, deterministically and with no real
//!   time.
//! - [`Barrier`] holds every waiter until a threshold of them has arrived, then
//!   releases them together. It pins the peak in-flight count to the threshold
//!   regardless of how the executor interleaves polls.
//! - [`PeakInFlight`] is a gauge plus completion log: it records the high-water
//!   mark of overlapping calls and the order in which they completed.
//! - [`DelayingToolHandler`] wraps any [`ToolHandler`] to inject a [`Delay`] and
//!   record overlap in a [`PeakInFlight`], so a scripted tool turn can be driven
//!   into a known concurrency shape.
//!
//! Cancel-on-call and panic-on-call wrappers (milestone M5-2) and the scripted
//! subagent spawner (M5-3) build on these primitives.

use std::collections::VecDeque;
use std::future::{Future, IntoFuture};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

use agent_lib::agent::{RequirementResult, RunContext, ToolHandler};
use agent_lib::conversation::ToolCallId;
use agent_lib::model::tool::ToolCall;
use async_trait::async_trait;

// ----- Delay -----

/// An `await`-able that yields the executor `ticks` times before completing.
///
/// A [`Delay`] carries no real time: each tick re-schedules the current task
/// (waking it immediately) and returns [`Poll::Pending`], so the executor is
/// free to poll another concurrent future in between. A [`Delay::ready`] (zero
/// ticks) completes on the first poll.
///
/// Giving concurrent calls different tick counts produces a deterministic
/// completion order: the future with the fewest ticks finishes first.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Delay {
    ticks: usize,
}

impl Delay {
    /// A delay that completes on the first poll without yielding.
    #[must_use]
    pub const fn ready() -> Self {
        Self { ticks: 0 }
    }

    /// A delay that yields the executor `ticks` times before completing.
    #[must_use]
    pub const fn yields(ticks: usize) -> Self {
        Self { ticks }
    }

    /// Returns how many times this delay yields before completing.
    #[must_use]
    pub const fn ticks(self) -> usize {
        self.ticks
    }
}

impl IntoFuture for Delay {
    type Output = ();
    type IntoFuture = YieldTicks;

    fn into_future(self) -> YieldTicks {
        YieldTicks {
            remaining: self.ticks,
        }
    }
}

/// The future a [`Delay`] turns into: yields `remaining` more times, then ready.
#[derive(Clone, Copy, Debug)]
pub struct YieldTicks {
    remaining: usize,
}

impl Future for YieldTicks {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.remaining == 0 {
            Poll::Ready(())
        } else {
            self.remaining -= 1;
            // Re-schedule immediately so the executor keeps making progress; a
            // real clock is never consulted.
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    }
}

// ----- Barrier -----

/// A cooperative barrier that releases its waiters together.
///
/// The first `threshold` calls to [`wait`](Self::wait) block until all of them
/// have arrived; the arrival that reaches the threshold releases every waiter at
/// once. Because a batch of concurrent calls all park at the barrier before any
/// proceeds, the barrier pins the peak in-flight count to `threshold` regardless
/// of how the executor happens to interleave polls.
///
/// A barrier constructed with a `threshold` larger than the number of tasks that
/// actually reach it never releases, so use it only when the arriving count is
/// known.
#[derive(Clone, Debug)]
pub struct Barrier {
    inner: Arc<Mutex<BarrierState>>,
}

#[derive(Debug)]
struct BarrierState {
    threshold: usize,
    arrived: usize,
    released: bool,
    wakers: Vec<Waker>,
}

impl Barrier {
    /// Builds a barrier that releases once `threshold` waiters have arrived.
    ///
    /// A `threshold` of `0` is already released and never blocks.
    #[must_use]
    pub fn new(threshold: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(BarrierState {
                threshold,
                arrived: 0,
                released: threshold == 0,
                wakers: Vec::new(),
            })),
        }
    }

    /// Returns a future that blocks until the barrier releases.
    #[must_use]
    pub fn wait(&self) -> BarrierWait {
        BarrierWait {
            inner: Arc::clone(&self.inner),
            counted: false,
        }
    }

    /// Returns the release threshold.
    #[must_use]
    pub fn threshold(&self) -> usize {
        self.lock().threshold
    }

    /// Returns how many waiters have arrived so far.
    #[must_use]
    pub fn arrived(&self) -> usize {
        self.lock().arrived
    }

    /// Returns whether the barrier has released its waiters.
    #[must_use]
    pub fn is_released(&self) -> bool {
        self.lock().released
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BarrierState> {
        self.inner.lock().expect("barrier mutex poisoned")
    }
}

/// The future returned by [`Barrier::wait`].
///
/// Counts its arrival exactly once, then parks until the barrier releases.
#[derive(Debug)]
pub struct BarrierWait {
    inner: Arc<Mutex<BarrierState>>,
    counted: bool,
}

impl Future for BarrierWait {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let this = self.get_mut();
        let mut state = this.inner.lock().expect("barrier mutex poisoned");
        if state.released {
            return Poll::Ready(());
        }
        if !this.counted {
            this.counted = true;
            state.arrived += 1;
            if state.arrived >= state.threshold {
                state.released = true;
                for waker in state.wakers.drain(..) {
                    waker.wake();
                }
                return Poll::Ready(());
            }
        }
        state.wakers.push(cx.waker().clone());
        Poll::Pending
    }
}

// ----- PeakInFlight -----

/// A concurrency gauge plus completion log.
///
/// Every overlapping call brackets its work with [`enter`](Self::enter) and the
/// returned [`InFlightGuard`]: entering raises the in-flight count (and the
/// peak high-water mark), and completing the guard records the call in
/// completion order. Because the guard also decrements the in-flight count when
/// it is dropped without completing, a cancelled call cannot leak an inflated
/// gauge.
#[derive(Debug, Default)]
pub struct PeakInFlight {
    state: Mutex<PeakState>,
}

#[derive(Debug, Default)]
struct PeakState {
    in_flight: usize,
    peak: usize,
    begun: usize,
    completions: Vec<usize>,
}

impl PeakInFlight {
    /// Builds an empty gauge.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records the start of a call, returning a guard that ends it.
    ///
    /// Raises the in-flight count and, if it is a new high, the peak. The guard
    /// carries the zero-based begin index so a completion can be tied back to
    /// its start.
    #[must_use]
    pub fn enter(&self) -> InFlightGuard<'_> {
        let mut state = self.lock();
        let index = state.begun;
        state.begun += 1;
        state.in_flight += 1;
        if state.in_flight > state.peak {
            state.peak = state.in_flight;
        }
        InFlightGuard {
            gauge: self,
            index,
            settled: false,
        }
    }

    /// Returns the peak number of calls that were ever in flight at once.
    #[must_use]
    pub fn peak(&self) -> usize {
        self.lock().peak
    }

    /// Returns the number of calls currently in flight.
    #[must_use]
    pub fn in_flight(&self) -> usize {
        self.lock().in_flight
    }

    /// Returns how many calls have begun.
    #[must_use]
    pub fn begun(&self) -> usize {
        self.lock().begun
    }

    /// Returns how many calls have completed.
    #[must_use]
    pub fn completed(&self) -> usize {
        self.lock().completions.len()
    }

    /// Returns the begin indices of completed calls, in completion order.
    ///
    /// A stable out-of-order completion shows up here as a permutation of the
    /// begin indices: `[1, 0]` means the second call to [`enter`](Self::enter)
    /// completed before the first.
    #[must_use]
    pub fn completion_order(&self) -> Vec<usize> {
        self.lock().completions.clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, PeakState> {
        self.state.lock().expect("peak-in-flight mutex poisoned")
    }
}

/// A guard bracketing one in-flight call against a [`PeakInFlight`] gauge.
///
/// Completing the guard with [`complete`](Self::complete) records the call in
/// completion order. Dropping it without completing (for example when the call
/// is cancelled) still releases the in-flight slot but records no completion.
#[derive(Debug)]
pub struct InFlightGuard<'gauge> {
    gauge: &'gauge PeakInFlight,
    index: usize,
    settled: bool,
}

impl InFlightGuard<'_> {
    /// Returns the zero-based begin index of the bracketed call.
    #[must_use]
    pub fn index(&self) -> usize {
        self.index
    }

    /// Marks the call complete, recording it in the gauge's completion log.
    pub fn complete(mut self) {
        let mut state = self.gauge.lock();
        state.in_flight = state.in_flight.saturating_sub(1);
        state.completions.push(self.index);
        self.settled = true;
    }
}

impl Drop for InFlightGuard<'_> {
    fn drop(&mut self) {
        if !self.settled {
            // Dropped without completing (e.g. a cancelled call): release the
            // slot but record no completion.
            let mut state = self.gauge.lock();
            state.in_flight = state.in_flight.saturating_sub(1);
        }
    }
}

// ----- DelayingToolHandler -----

/// How a [`DelayingToolHandler`] chooses the [`Delay`] for each call.
#[derive(Debug)]
enum DelaySchedule {
    /// The same delay for every call.
    Fixed(Delay),
    /// A queue of delays consumed in dispatch order, falling back to `fallback`
    /// once drained.
    Ordered {
        queue: Mutex<VecDeque<Delay>>,
        fallback: Delay,
    },
}

impl DelaySchedule {
    fn next(&self) -> Delay {
        match self {
            DelaySchedule::Fixed(delay) => *delay,
            DelaySchedule::Ordered { queue, fallback } => queue
                .lock()
                .expect("delay schedule mutex poisoned")
                .pop_front()
                .unwrap_or(*fallback),
        }
    }
}

/// Wraps a [`ToolHandler`] to inject a [`Delay`] and record overlap.
///
/// The wrapper brackets each call in a shared [`PeakInFlight`] gauge, optionally
/// parks it at a [`Barrier`] so a whole batch reaches maximum overlap, and then
/// applies a [`Delay`] before delegating to the inner handler. Because a
/// scripted tool handler otherwise completes a call in a single poll (its work
/// is synchronous), the injected delay is what creates an observable window in
/// which multiple concurrent calls are in flight at once.
///
/// The gauge is readable through [`gauge`](Self::gauge),
/// [`peak_concurrency`](Self::peak_concurrency), and
/// [`completion_order`](Self::completion_order).
pub struct DelayingToolHandler<H> {
    inner: H,
    schedule: DelaySchedule,
    barrier: Option<Barrier>,
    gauge: Arc<PeakInFlight>,
}

impl<H> DelayingToolHandler<H> {
    /// Wraps `inner`, recording overlap but adding no delay.
    #[must_use]
    pub fn new(inner: H) -> Self {
        Self::with_delay(inner, Delay::ready())
    }

    /// Wraps `inner`, applying the same `delay` to every call.
    #[must_use]
    pub fn with_delay(inner: H, delay: Delay) -> Self {
        Self {
            inner,
            schedule: DelaySchedule::Fixed(delay),
            barrier: None,
            gauge: Arc::new(PeakInFlight::new()),
        }
    }

    /// Wraps `inner`, applying `delays` to calls in dispatch order.
    ///
    /// Once the schedule is drained, later calls complete without extra yields.
    #[must_use]
    pub fn with_delays(inner: H, delays: impl IntoIterator<Item = Delay>) -> Self {
        Self {
            inner,
            schedule: DelaySchedule::Ordered {
                queue: Mutex::new(delays.into_iter().collect()),
                fallback: Delay::ready(),
            },
            barrier: None,
            gauge: Arc::new(PeakInFlight::new()),
        }
    }

    /// Parks every call at a shared [`Barrier`] of `threshold` before delaying.
    ///
    /// Use this when exactly `threshold` calls run concurrently and the test
    /// wants the peak in-flight count pinned to that threshold.
    #[must_use]
    pub fn with_barrier(mut self, threshold: usize) -> Self {
        self.barrier = Some(Barrier::new(threshold));
        self
    }

    /// Returns the shared gauge recording overlap and completion order.
    #[must_use]
    pub fn gauge(&self) -> &Arc<PeakInFlight> {
        &self.gauge
    }

    /// Returns the barrier calls park at, if one was configured.
    #[must_use]
    pub fn barrier(&self) -> Option<&Barrier> {
        self.barrier.as_ref()
    }

    /// Returns the inner handler.
    #[must_use]
    pub fn inner(&self) -> &H {
        &self.inner
    }

    /// Returns the peak number of calls that were ever in flight at once.
    #[must_use]
    pub fn peak_concurrency(&self) -> usize {
        self.gauge.peak()
    }

    /// Returns the begin indices of completed calls, in completion order.
    #[must_use]
    pub fn completion_order(&self) -> Vec<usize> {
        self.gauge.completion_order()
    }
}

#[async_trait]
impl<H: ToolHandler> ToolHandler for DelayingToolHandler<H> {
    async fn fulfill(
        &self,
        call_id: ToolCallId,
        call: &ToolCall,
        ctx: &RunContext,
    ) -> RequirementResult {
        let guard = self.gauge.enter();
        if let Some(barrier) = &self.barrier {
            barrier.wait().await;
        }
        let delay = self.schedule.next();
        delay.await;
        let result = self.inner.fulfill(call_id, call, ctx).await;
        guard.complete();
        result
    }
}

#[cfg(test)]
mod tests {
    use super::{Barrier, Delay, DelayingToolHandler, PeakInFlight};
    use crate::fixtures::{root_context, tool_call};
    use crate::handlers::ScriptedToolHandler;
    use crate::ids::SeqIds;
    use crate::script::ToolStep;
    use agent_lib::agent::{RunContext, ToolHandler};
    use agent_lib::conversation::ToolCallId;
    use agent_lib::model::tool::ToolCall;
    use futures::stream::{FuturesUnordered, StreamExt};
    use serde_json::json;

    /// A `Delay` yields exactly `ticks` times before completing, consulting no
    /// clock.
    #[tokio::test]
    async fn delay_yields_a_fixed_number_of_times_without_real_time() {
        let mut polls = 0;
        let mut future = Box::pin(Delay::yields(3).into_future());
        std::future::poll_fn(|cx| {
            polls += 1;
            future.as_mut().poll(cx)
        })
        .await;
        // Three pending polls (one per yield) plus the ready poll.
        assert_eq!(polls, 4);
    }

    /// A zero-tick delay is ready on the first poll.
    #[tokio::test]
    async fn ready_delay_completes_immediately() {
        Delay::ready().await;
    }

    /// A barrier holds its waiters until the threshold arrives, then releases
    /// them together.
    #[tokio::test]
    async fn barrier_releases_waiters_together() {
        let barrier = Barrier::new(2);
        assert!(!barrier.is_released());

        let mut both = FuturesUnordered::new();
        both.push(barrier.wait());
        both.push(barrier.wait());

        both.next().await.expect("first waiter releases");
        both.next().await.expect("second waiter releases");
        assert!(barrier.is_released());
        assert_eq!(barrier.arrived(), 2);
    }

    /// Two overlapping brackets raise the gauge peak to two.
    #[test]
    fn peak_in_flight_tracks_overlap_and_completion_order() {
        let gauge = PeakInFlight::new();
        assert_eq!(gauge.peak(), 0);

        let first = gauge.enter();
        let second = gauge.enter();
        assert_eq!(gauge.in_flight(), 2);
        assert_eq!(gauge.peak(), 2);

        // Complete out of dispatch order: the second bracket finishes first.
        second.complete();
        first.complete();
        assert_eq!(gauge.in_flight(), 0);
        assert_eq!(gauge.peak(), 2, "the peak is a high-water mark");
        assert_eq!(gauge.completion_order(), vec![1, 0]);
    }

    /// A guard dropped without completing releases its slot but logs nothing.
    #[test]
    fn dropped_guard_releases_without_recording_completion() {
        let gauge = PeakInFlight::new();
        {
            let _cancelled = gauge.enter();
            assert_eq!(gauge.in_flight(), 1);
        }
        assert_eq!(gauge.in_flight(), 0);
        assert_eq!(gauge.completed(), 0);
        assert_eq!(gauge.peak(), 1);
    }

    fn weather_call(ids: &SeqIds, provider_id: &str, city: &str) -> (ToolCallId, ToolCall) {
        (
            ids.tool_call_id(),
            tool_call(provider_id, "get_weather", json!({ "city": city })),
        )
    }

    /// Two concurrent tool calls through a barrier reach a peak in-flight of two.
    #[tokio::test]
    async fn two_concurrent_tool_calls_peak_in_flight_is_two() {
        let ids = SeqIds::new();
        let ctx: RunContext = root_context(&ids);
        let inner = ScriptedToolHandler::from_steps([
            ToolStep::ok("call-a", "sunny"),
            ToolStep::ok("call-b", "cloudy"),
        ]);
        // The barrier pins overlap to two; the delay gives the brackets a window
        // to coexist.
        let handler = DelayingToolHandler::with_delay(inner, Delay::yields(1)).with_barrier(2);

        let (id_a, call_a) = weather_call(&ids, "call-a", "SH");
        let (id_b, call_b) = weather_call(&ids, "call-b", "BJ");

        let mut batch = FuturesUnordered::new();
        batch.push(handler.fulfill(id_a, &call_a, &ctx));
        batch.push(handler.fulfill(id_b, &call_b, &ctx));
        while batch.next().await.is_some() {}

        assert_eq!(handler.peak_concurrency(), 2);
        assert_eq!(handler.gauge().begun(), 2);
        assert_eq!(handler.gauge().completed(), 2);
    }

    /// Uneven per-call delays yield a stable out-of-order completion.
    #[tokio::test]
    async fn ordered_delays_produce_stable_out_of_order_completion() {
        let ids = SeqIds::new();
        let ctx: RunContext = root_context(&ids);
        let inner = ScriptedToolHandler::from_steps([
            ToolStep::ok("call-a", "sunny"),
            ToolStep::ok("call-b", "cloudy"),
        ]);
        // The first-dispatched call yields longer than the second, so the second
        // completes first regardless of dispatch order.
        let handler = DelayingToolHandler::with_delays(inner, [Delay::yields(3), Delay::yields(0)]);

        let (id_a, call_a) = weather_call(&ids, "call-a", "SH");
        let (id_b, call_b) = weather_call(&ids, "call-b", "BJ");

        let mut batch = FuturesUnordered::new();
        batch.push(handler.fulfill(id_a, &call_a, &ctx));
        batch.push(handler.fulfill(id_b, &call_b, &ctx));
        while batch.next().await.is_some() {}

        // Begin index 1 (the second dispatch) completed before begin index 0.
        assert_eq!(handler.completion_order(), vec![1, 0]);
        assert_eq!(handler.peak_concurrency(), 2);
    }
}
