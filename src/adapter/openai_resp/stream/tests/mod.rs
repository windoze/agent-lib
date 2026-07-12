//! Focused tests for OpenAI Responses SSE normalization and transport.

use super::*;
use crate::{
    client::{ChatRequest, ClientError, Response},
    model::{content::ContentBlock, message::Role, usage::Usage},
    stream::{
        BlockId, BlockKind, Delta, StreamEvent,
        accumulator::{Accumulator, AccumulatorError},
    },
};
use futures::{TryStreamExt, stream};
use serde_json::Value;
use std::convert::Infallible;

mod errors;
mod parsing;
mod transport;

/// Real Foundry text stream captured on 2026-07-13, with ids and obfuscation
/// values redacted.
const REAL_TEXT_STREAM: &str = include_str!("fixtures/text_stream.sse");

/// Real Foundry tool stream captured on 2026-07-13, with ids and obfuscation
/// values redacted.
const REAL_TOOL_STREAM: &str = include_str!("fixtures/tool_stream.sse");

/// Decodes an SSE fixture after splitting bytes across framing and UTF-8
/// boundaries.
async fn decode_fixture(fixture: &str) -> Result<Vec<StreamEvent>, ClientError> {
    let chunks = irregular_chunks(fixture.as_bytes());
    let source = stream::iter(chunks.into_iter().map(Ok::<_, Infallible>));

    normalize_sse(source, |never| match never {})
        .try_collect()
        .await
}

/// Uses a repeating uneven pattern so tests do not depend on HTTP chunk
/// boundaries.
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

/// Parses the complete response embedded in a fixture's terminal event.
fn parse_terminal_response(fixture: &str) -> Response {
    let response = fixture
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter_map(|data| serde_json::from_str::<Value>(data).ok())
        .find_map(|event| {
            matches!(
                event.get("type").and_then(Value::as_str),
                Some("response.completed" | "response.incomplete")
            )
            .then(|| event.get("response").cloned())
            .flatten()
        })
        .expect("fixture should contain a terminal response object");
    let body = serde_json::to_vec(&response).expect("serialize terminal response fixture");
    let mut response =
        OpenAiRespAdapter::parse_response(&body).expect("parse terminal complete response");
    clear_content_extras(&mut response.message.content);
    response
}

/// Removes complete-response-only item metadata before comparing normalized
/// content with the stream accumulator, whose event model carries top-level
/// response metadata but not per-block provider extras.
fn clear_content_extras(blocks: &mut [ContentBlock]) {
    for block in blocks {
        match block {
            ContentBlock::Text { extra, .. }
            | ContentBlock::Image { extra, .. }
            | ContentBlock::ToolUse { extra, .. }
            | ContentBlock::Thinking { extra, .. } => extra.clear(),
            ContentBlock::ToolResult { content, extra, .. } => {
                extra.clear();
                clear_content_extras(content);
            }
        }
    }
}
