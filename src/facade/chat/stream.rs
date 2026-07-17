//! Incremental [`RunStream`] backing [`ChatSession::stream`].
//!
//! [`ChatSession::stream`](super::ChatSession::stream) opens a pending turn, asks
//! the client for a normalized event stream, and hands back a [`RunStream`]. The
//! stream forwards each normalized [`RunEvent::TextDelta`] plus the underlying
//! [`RunEvent::RawStream`] escape hatch while simultaneously folding every event
//! into a [`stream::accumulator::Accumulator`](crate::stream::accumulator::Accumulator).
//! When the source stream ends it finalizes the accumulator into a complete
//! [`Response`], folds that response through the same Conversation transaction the
//! non-streaming drive uses (`start_assistant_response` → `finish_assistant` →
//! `commit_pending`), and yields one terminal [`RunEvent::Done`].
//!
//! The terminal [`RunOutput`] is therefore identical to what
//! [`ChatSession::send_full`](super::ChatSession::send_full) would return for the
//! same response. Tool-use is rejected exactly as in the non-streaming path with
//! [`FacadeError::UnexpectedToolUse`], and any streaming failure rolls the
//! pending turn back so the session stays at its last committed point.

use std::collections::VecDeque;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures::stream::BoxStream;
use futures::{Stream, StreamExt};

use crate::client::{ClientError, Response};
use crate::conversation::{AssistantFinish, CancelDisposition, Conversation, TurnMeta};
use crate::facade::error::FacadeError;
use crate::facade::ids::FacadeIds;
use crate::facade::run::{RunEvent, RunOutput};
use crate::stream::StreamEvent;
use crate::stream::accumulator::{Accumulator, AccumulatorError};

/// An incremental stream of [`RunEvent`]s produced by [`ChatSession::stream`].
///
/// `RunStream` implements [`futures::Stream`] with
/// `Item = Result<RunEvent, FacadeError>` and also offers an inherent
/// [`next`](RunStream::next) convenience so callers need not import
/// [`futures::StreamExt`]. Each source event yields the normalized
/// [`RunEvent::TextDelta`] (for text deltas) followed by the raw
/// [`RunEvent::RawStream`] escape hatch; the stream ends with exactly one
/// [`RunEvent::Done`] carrying the complete [`RunOutput`].
///
/// [`ChatSession::stream`]: super::ChatSession::stream
pub struct RunStream<'a> {
    /// The session Conversation whose pending turn this stream will commit.
    conversation: &'a mut Conversation,
    /// The underlying normalized event stream from the client.
    inner: BoxStream<'static, Result<StreamEvent, ClientError>>,
    /// Folds incremental events into one complete response; taken at finish.
    accumulator: Option<Accumulator>,
    /// Identity source used to mint the assistant message id at commit.
    ids: FacadeIds,
    /// Normalized/raw events ready to be handed out before more are pulled.
    buffered: VecDeque<RunEvent>,
    /// Lifecycle state of the fold-and-commit drive.
    state: State,
}

/// Lifecycle of a [`RunStream`]'s fold-and-commit drive.
#[derive(Debug, PartialEq, Eq)]
enum State {
    /// Still pulling incremental events from the source stream.
    Streaming,
    /// The source stream ended; the accumulator must be finalized and committed.
    Finishing,
    /// Terminal `Done` (or an error) was produced; nothing more is yielded.
    Done,
}

impl<'a> RunStream<'a> {
    /// Builds a stream over an already-opened pending turn and source stream.
    ///
    /// The caller (`ChatSession::stream`) is responsible for having called
    /// `begin_turn` before constructing this value; the terminal fold commits
    /// that pending turn.
    pub(super) fn new(
        conversation: &'a mut Conversation,
        inner: BoxStream<'static, Result<StreamEvent, ClientError>>,
        ids: FacadeIds,
    ) -> Self {
        Self {
            conversation,
            inner,
            accumulator: Some(Accumulator::new()),
            ids,
            buffered: VecDeque::new(),
            state: State::Streaming,
        }
    }

    /// Returns the next event, or `None` once the stream is exhausted.
    ///
    /// This is an inherent convenience equivalent to
    /// [`StreamExt::next`](futures::StreamExt::next); it lets callers write
    /// `stream.next().await` without importing [`futures::StreamExt`].
    pub async fn next(&mut self) -> Option<Result<RunEvent, FacadeError>> {
        StreamExt::next(self).await
    }

    /// Discards the in-flight pending turn so the session stays consistent.
    fn rollback(&mut self) {
        let _ = self
            .conversation
            .cancel_pending(CancelDisposition::DiscardTurn);
    }

    /// Folds one source event into the accumulator and buffers its `RunEvent`s.
    ///
    /// On any accumulator error the pending turn is rolled back and the mapped
    /// [`FacadeError`] is returned so the caller can surface it as a stream item.
    fn absorb(&mut self, event: StreamEvent) -> Result<(), FacadeError> {
        // Normalized text is the primary path; the raw event is the escape hatch.
        if let StreamEvent::BlockDelta {
            delta: crate::stream::Delta::Text(text),
            ..
        } = &event
        {
            self.buffered.push_back(RunEvent::TextDelta(text.clone()));
        }
        self.buffered.push_back(RunEvent::RawStream(event.clone()));

        let accumulator = self
            .accumulator
            .as_mut()
            .expect("accumulator present while streaming");
        if let Err(error) = accumulator.push(event) {
            self.rollback();
            return Err(map_accumulator_error(error));
        }
        Ok(())
    }

    /// Finalizes the accumulator, commits the turn, and returns the `Done` event.
    ///
    /// This mirrors the non-streaming `drive_pending` tail exactly so the
    /// resulting [`RunOutput`] matches turn for turn. Any failure rolls the
    /// pending turn back.
    fn finish(&mut self) -> Result<RunEvent, FacadeError> {
        match self.finish_inner() {
            Ok(done) => Ok(done),
            Err(error) => {
                self.rollback();
                Err(error)
            }
        }
    }

    /// The fallible core of [`finish`](Self::finish) without rollback handling.
    fn finish_inner(&mut self) -> Result<RunEvent, FacadeError> {
        let accumulator = self
            .accumulator
            .take()
            .expect("accumulator present until finish");
        let response: Response = accumulator.finish().map_err(map_accumulator_error)?;

        self.conversation
            .start_assistant_response(response.clone())?;
        match self.conversation.finish_assistant(self.ids.message_id())? {
            AssistantFinish::ReadyToCommit => {}
            AssistantFinish::RequiresToolCallMappings => {
                return Err(FacadeError::UnexpectedToolUse);
            }
        }
        self.conversation.commit_pending(TurnMeta::default())?;

        Ok(RunEvent::Done(Box::new(RunOutput::from(response))))
    }
}

impl Stream for RunStream<'_> {
    type Item = Result<RunEvent, FacadeError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        // Every field is `Unpin`, so structural pinning is unnecessary.
        let this = self.get_mut();

        loop {
            if let Some(event) = this.buffered.pop_front() {
                return Poll::Ready(Some(Ok(event)));
            }

            match this.state {
                State::Streaming => match this.inner.poll_next_unpin(cx) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(Some(Ok(event))) => {
                        if let Err(error) = this.absorb(event) {
                            this.state = State::Done;
                            return Poll::Ready(Some(Err(error)));
                        }
                    }
                    Poll::Ready(Some(Err(error))) => {
                        this.rollback();
                        this.state = State::Done;
                        return Poll::Ready(Some(Err(FacadeError::from(error))));
                    }
                    Poll::Ready(None) => this.state = State::Finishing,
                },
                State::Finishing => {
                    let result = this.finish();
                    this.state = State::Done;
                    return Poll::Ready(Some(result));
                }
                State::Done => return Poll::Ready(None),
            }
        }
    }
}

/// Maps an [`AccumulatorError`] into the reduced facade error surface.
///
/// A provider-emitted stream error carries a [`ClientError`] and is preserved as
/// [`FacadeError::Client`]; every other accumulator failure is a stream-protocol
/// violation reported as [`ClientError::Protocol`].
fn map_accumulator_error(error: AccumulatorError) -> FacadeError {
    match error {
        AccumulatorError::Stream(client) => FacadeError::Client(client),
        other => FacadeError::Client(ClientError::Protocol(other.to_string())),
    }
}
