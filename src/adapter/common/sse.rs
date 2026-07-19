//! Generic SSE framing and terminal-on-error stream production.

use crate::{client::ClientError, stream::StreamEvent};
use eventsource_stream::{Event, EventStreamError, Eventsource};
use futures::{
    Stream, StreamExt,
    stream::{self, BoxStream},
};
use std::collections::VecDeque;

/// Adapter-specific state machine required by the shared SSE decoder.
pub(crate) trait SseNormalizer: Default + Send + 'static {
    /// Translates one fully framed SSE event into zero or more normalized events.
    fn translate(&mut self, event: Event) -> Result<Vec<StreamEvent>, ClientError>;

    /// Reports whether a normal terminal provider event has already ended the stream.
    fn is_terminal(&self) -> bool;

    /// Builds the protocol error emitted when the byte stream ends too early.
    fn incomplete_error(&self) -> ClientError;

    /// Adds adapter-specific context to invalid SSE framing diagnostics.
    fn invalid_sse(message: String) -> ClientError;
}

/// Turns a byte stream into a terminal-on-error stream of normalized events.
pub(crate) fn normalize_sse<N, S, B, E, F>(
    source: S,
    map_transport: F,
) -> BoxStream<'static, Result<StreamEvent, ClientError>>
where
    N: SseNormalizer,
    S: Stream<Item = Result<B, E>> + Send + 'static,
    B: AsRef<[u8]> + Send + 'static,
    E: Send + 'static,
    F: Fn(E) -> ClientError + Send + 'static,
{
    let state = DecoderState::<N, E> {
        source: source.eventsource().boxed(),
        normalizer: N::default(),
        pending: VecDeque::new(),
        map_transport: Box::new(map_transport),
        terminated: false,
    };

    stream::unfold(state, |mut state| async move {
        loop {
            if let Some(event) = state.pending.pop_front() {
                return Some((Ok(event), state));
            }
            if state.terminated || state.normalizer.is_terminal() {
                return None;
            }

            match state.source.next().await {
                Some(Ok(event)) => match state.normalizer.translate(event) {
                    Ok(events) => state.pending.extend(events),
                    Err(error) => {
                        state.terminated = true;
                        return Some((Err(error), state));
                    }
                },
                Some(Err(error)) => {
                    state.terminated = true;
                    let error = map_event_stream_error::<N, E>(error, &state.map_transport);
                    return Some((Err(error), state));
                }
                None => {
                    state.terminated = true;
                    let error = state.normalizer.incomplete_error();
                    return Some((Err(error), state));
                }
            }
        }
    })
    .boxed()
}

/// Owned state retained between polls of the normalized output stream.
struct DecoderState<N, E> {
    source: BoxStream<'static, Result<Event, EventStreamError<E>>>,
    normalizer: N,
    pending: VecDeque<StreamEvent>,
    map_transport: Box<dyn Fn(E) -> ClientError + Send>,
    terminated: bool,
}

/// Separates HTTP body failures from invalid UTF-8 or SSE syntax.
fn map_event_stream_error<N, E>(
    error: EventStreamError<E>,
    map_transport: &dyn Fn(E) -> ClientError,
) -> ClientError
where
    N: SseNormalizer,
{
    match error {
        EventStreamError::Transport(error) => map_transport(error),
        EventStreamError::Utf8(error) => {
            N::invalid_sse(format!("SSE body was not valid UTF-8: {error}"))
        }
        EventStreamError::Parser(error) => {
            N::invalid_sse(format!("failed to parse SSE framing: {error}"))
        }
    }
}
