//! Responses-specific adapter for the shared SSE decoder.

use super::{invalid_stream, normalizer::StreamNormalizer};
use crate::{adapter::common, client::ClientError, stream::StreamEvent};
use eventsource_stream::Event;
use futures::{Stream, stream::BoxStream};

/// Turns a byte stream into a terminal-on-error stream of normalized Responses
/// events.
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
    common::normalize_sse::<StreamNormalizer, _, _, _, _>(source, map_transport)
}

impl common::SseNormalizer for StreamNormalizer {
    fn translate(&mut self, event: Event) -> Result<Vec<StreamEvent>, ClientError> {
        StreamNormalizer::translate(self, event)
    }

    fn is_terminal(&self) -> bool {
        StreamNormalizer::is_terminal(self)
    }

    fn incomplete_error(&self) -> ClientError {
        StreamNormalizer::incomplete_error(self)
    }

    fn invalid_sse(message: String) -> ClientError {
        invalid_stream(message)
    }
}
