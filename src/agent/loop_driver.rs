//! Agent loop trait and feed-stream backpressure guard.
//!
//! The loop trait is a runtime boundary and is not serde data. A loop returns a
//! guarded event stream for each `feed` call; callers must finish or drop that
//! stream before starting another feed segment for the same Agent.

use crate::agent::{AgentError, AgentEvent, AgentInput, PivotMessage};
use async_trait::async_trait;
use futures::{Stream, stream::BoxStream};
use std::{
    fmt,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll},
};

mod default;

pub use default::{DefaultAgentLoop, LlmStepMode};

/// Boxed Agent event stream item type used by object-safe loop implementations.
pub type BoxAgentEventStream = BoxStream<'static, Result<AgentEvent, AgentError>>;

/// Boxed object-safe Agent loop.
pub type BoxAgentLoop = Box<dyn AgentLoop>;

/// Object-safe runtime abstraction for advancing an Agent.
///
/// One `feed` call represents one autonomous run segment and returns an event
/// stream. The stream is the backpressure boundary: implementations should use
/// [`AgentFeedGuard`] so another feed call is rejected until the prior stream
/// is consumed to EOF or dropped.
#[async_trait]
pub trait AgentLoop: Send {
    /// Starts one autonomous run segment and returns its event stream.
    ///
    /// # Errors
    ///
    /// Returns [`AgentError::FeedInProgress`] when a prior stream for the same
    /// Agent is still active. Other errors are classified by the loop
    /// implementation before any unchecked state is exposed.
    async fn feed(&mut self, input: AgentInput) -> Result<AgentEventStream, AgentError>;

    /// Queues a user-role pivot for a future step boundary.
    ///
    /// # Errors
    ///
    /// Returns a classified [`AgentError`] when the loop cannot accept the
    /// pivot in its current runtime state.
    fn interject(&self, message: PivotMessage) -> Result<(), AgentError>;
}

/// Shared guard that ensures only one feed stream is active.
#[derive(Clone, Debug, Default)]
pub struct AgentFeedGuard {
    active: Arc<AtomicBool>,
}

impl AgentFeedGuard {
    /// Creates an idle feed guard.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns whether a feed stream is currently active.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }

    /// Acquires a feed permit before starting runtime work.
    ///
    /// # Errors
    ///
    /// Returns [`AgentError::FeedInProgress`] if another permit is still held
    /// by a live event stream.
    pub fn try_acquire(&self) -> Result<AgentFeedPermit, AgentError> {
        self.active
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| AgentError::FeedInProgress)?;

        Ok(AgentFeedPermit {
            active: Arc::clone(&self.active),
            released: false,
        })
    }

    /// Wraps a boxed stream in a guard-acquired [`AgentEventStream`].
    ///
    /// # Errors
    ///
    /// Returns [`AgentError::FeedInProgress`] if another stream is active.
    pub fn guard_stream(
        &self,
        stream: BoxAgentEventStream,
    ) -> Result<AgentEventStream, AgentError> {
        let permit = self.try_acquire()?;
        Ok(AgentEventStream::new(stream, permit))
    }
}

/// Permit held by one active feed stream.
#[derive(Debug)]
pub struct AgentFeedPermit {
    active: Arc<AtomicBool>,
    released: bool,
}

impl AgentFeedPermit {
    fn release(&mut self) {
        if !self.released {
            self.active.store(false, Ordering::Release);
            self.released = true;
        }
    }
}

impl Drop for AgentFeedPermit {
    fn drop(&mut self) {
        self.release();
    }
}

/// Guarded event stream returned by [`AgentLoop::feed`].
///
/// The active-feed permit is released when the stream reaches EOF or when the
/// stream wrapper is dropped before EOF.
pub struct AgentEventStream {
    inner: BoxAgentEventStream,
    permit: AgentFeedPermit,
}

impl AgentEventStream {
    /// Creates a stream wrapper from an already acquired feed permit.
    #[must_use]
    pub const fn new(inner: BoxAgentEventStream, permit: AgentFeedPermit) -> Self {
        Self { inner, permit }
    }

    /// Converts this guarded stream back into a boxed stream.
    #[must_use]
    pub fn into_boxed(self) -> BoxAgentEventStream {
        Box::pin(self)
    }
}

impl Stream for AgentEventStream {
    type Item = Result<AgentEvent, AgentError>;

    fn poll_next(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        let poll = this.inner.as_mut().poll_next(context);
        if matches!(poll, Poll::Ready(None)) {
            this.permit.release();
        }
        poll
    }
}

impl fmt::Debug for AgentEventStream {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AgentEventStream")
            .field("active", &!self.permit.released)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::{AgentEventStream, AgentFeedGuard, AgentLoop, BoxAgentEventStream};
    use crate::agent::{AgentError, AgentEvent, AgentInput, AgentOutcome, PivotMessage, StepId};
    use async_trait::async_trait;
    use futures::{StreamExt, stream};

    fn step_id() -> StepId {
        "018f0d9c-7b6a-7c12-8f31-1234567890a8"
            .parse()
            .expect("step id")
    }

    fn input() -> AgentInput {
        AgentInput::resume(step_id())
    }

    fn done_stream() -> BoxAgentEventStream {
        stream::iter([Ok(AgentEvent::Done(AgentOutcome::Completed))]).boxed()
    }

    #[derive(Debug, Default)]
    struct FakeLoop {
        guard: AgentFeedGuard,
    }

    #[async_trait]
    impl AgentLoop for FakeLoop {
        async fn feed(&mut self, _input: AgentInput) -> Result<AgentEventStream, AgentError> {
            self.guard.guard_stream(done_stream())
        }

        fn interject(&self, _message: PivotMessage) -> Result<(), AgentError> {
            Ok(())
        }
    }

    fn assert_object_safe(_: &mut dyn AgentLoop) {}

    #[test]
    fn agent_loop_trait_is_object_safe() {
        let mut loop_impl = FakeLoop::default();
        assert_object_safe(&mut loop_impl);
    }

    #[tokio::test]
    async fn feed_guard_rejects_reentrant_feed_until_stream_is_dropped() {
        let mut loop_impl = FakeLoop::default();
        let stream = loop_impl.feed(input()).await.expect("first feed starts");

        let error = loop_impl
            .feed(input())
            .await
            .expect_err("second feed must be rejected while stream is live");
        assert_eq!(error, AgentError::FeedInProgress);
        assert!(loop_impl.guard.is_active());

        drop(stream);
        assert!(!loop_impl.guard.is_active());

        let _next = loop_impl
            .feed(input())
            .await
            .expect("feed can restart after drop");
    }

    #[tokio::test]
    async fn feed_guard_releases_when_stream_reaches_eof() {
        let mut loop_impl = FakeLoop::default();
        let mut stream = loop_impl.feed(input()).await.expect("feed starts");

        assert_eq!(
            stream.next().await,
            Some(Ok(AgentEvent::Done(AgentOutcome::Completed)))
        );
        assert_eq!(stream.next().await, None);
        assert!(!loop_impl.guard.is_active());

        let _next = loop_impl
            .feed(input())
            .await
            .expect("feed can restart after EOF even if old stream is still in scope");
    }
}
