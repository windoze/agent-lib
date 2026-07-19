//! Focused tests for Anthropic SSE normalization and transport.

use super::*;
use crate::{
    client::{ChatRequest, ClientError, Response},
    model::{message::Role, usage::Usage},
    stream::{
        BlockId, BlockKind, Delta, StreamEvent,
        accumulator::{Accumulator, AccumulatorError},
    },
};
use futures::{TryStreamExt, stream};
use std::convert::Infallible;

mod errors;
mod parsing;
mod transport;

/// Real Foundry text stream captured on 2026-07-13, with its id redacted.
const REAL_TEXT_STREAM: &str = include_str!("fixtures/text_stream.sse");

/// Real Foundry tool stream captured on 2026-07-13, with ids redacted.
const REAL_TOOL_STREAM: &str = include_str!("fixtures/tool_stream.sse");

/// Protocol fixture covering Anthropic's separate thinking signature deltas.
const THINKING_STREAM: &str = include_str!("fixtures/thinking_stream.sse");

/// Decodes an SSE fixture after splitting bytes across framing and UTF-8 boundaries.
async fn decode_fixture(fixture: &str) -> Result<Vec<StreamEvent>, ClientError> {
    let chunks = irregular_chunks(fixture.as_bytes());
    let source = stream::iter(chunks.into_iter().map(Ok::<_, Infallible>));

    normalize_sse(source, |never| match never {})
        .try_collect()
        .await
}

/// Uses a repeating uneven pattern so tests do not depend on HTTP chunk boundaries.
fn irregular_chunks(bytes: &[u8]) -> Vec<Vec<u8>> {
    const SIZES: &[usize] = &[1, 2, 7, 3, 19, 5, 11];
    let mut chunks = Vec::new();
    let mut offset = 0;
    let mut next_size = 0;
    while offset < bytes.len() {
        let end = (offset + SIZES[next_size % SIZES.len()]).min(bytes.len());
        chunks.push(bytes[offset..end].to_vec());
        offset = end;
        next_size += 1;
    }
    chunks
}

/// Folds a decoded event list through the single shared accumulator.
fn fold_events(events: &[StreamEvent]) -> Result<Response, AccumulatorError> {
    let mut accumulator = Accumulator::new();
    for event in events {
        accumulator.push(event.clone())?;
    }
    accumulator.finish()
}

/// Aggregates usage exactly as direct stream consumers are expected to.
fn aggregate_usage_events(events: &[StreamEvent]) -> Usage {
    let mut usage = Usage::default();
    for event in events {
        if let StreamEvent::Usage(segment) = event {
            usage.merge(segment.clone());
        }
    }
    usage
}
