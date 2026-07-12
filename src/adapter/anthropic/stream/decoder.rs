//! Lazy SSE framing and terminal-on-error normalized stream production.

use super::{invalid_stream, normalizer::StreamNormalizer};
use crate::{client::ClientError, stream::StreamEvent};
use eventsource_stream::{Event, EventStreamError, Eventsource};
use futures::{
    Stream, StreamExt,
    stream::{self, BoxStream},
};
use std::collections::VecDeque;

/// Turns a byte stream into a terminal-on-error stream of normalized events.
pub(super) fn normalize_sse<S, B, E, F>(
    source: S,
    map_transport: F,
) -> BoxStream<'static, Result<StreamEvent, ClientError>>
where
    S: Stream<Item = Result<B, E>> + Send + 'static,
    B: AsRef<[u8]> + Send + 'static,
    E: Send + 'static,
    F: Fn(E) -> ClientError + Send + 'static,
{
    let state = DecoderState {
        source: source.eventsource().boxed(),
        normalizer: StreamNormalizer::default(),
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
                    let error = map_event_stream_error(error, &state.map_transport);
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
struct DecoderState<E> {
    source: BoxStream<'static, Result<Event, EventStreamError<E>>>,
    normalizer: StreamNormalizer,
    pending: VecDeque<StreamEvent>,
    map_transport: Box<dyn Fn(E) -> ClientError + Send>,
    terminated: bool,
}

/// Separates HTTP body failures from invalid UTF-8 or SSE syntax.
fn map_event_stream_error<E>(
    error: EventStreamError<E>,
    map_transport: &dyn Fn(E) -> ClientError,
) -> ClientError {
    match error {
        EventStreamError::Transport(error) => map_transport(error),
        EventStreamError::Utf8(error) => {
            invalid_stream(format!("SSE body was not valid UTF-8: {error}"))
        }
        EventStreamError::Parser(error) => {
            invalid_stream(format!("failed to parse SSE framing: {error}"))
        }
    }
}
