//! Async stream collection tests.

use crate::{
    model::{
        content::ContentBlock,
        message::Role,
        normalized::{Normalized, StopReason},
    },
    stream::{
        BlockId, BlockKind, Delta, StreamEvent,
        accumulator::{CollectError, collect},
    },
};
use futures::stream;
use serde_json::Map;
use std::convert::Infallible;

#[tokio::test]
async fn collect_consumes_a_complete_fallible_stream() {
    let id = BlockId::new("text-1");
    let events = vec![
        StreamEvent::MessageStart {
            role: Role::Assistant,
        },
        StreamEvent::BlockStart {
            id: id.clone(),
            kind: BlockKind::Text,
        },
        StreamEvent::BlockDelta {
            id: id.clone(),
            delta: Delta::Text("hello".to_owned()),
        },
        StreamEvent::BlockStop { id },
        StreamEvent::MessageStop {
            stop_reason: Normalized::from_mapped(StopReason::EndTurn, "end_turn"),
        },
    ];
    let stream = stream::iter(events.into_iter().map(Ok::<_, Infallible>));

    let response = collect(stream).await.expect("collect response");

    assert_eq!(
        response.message.content,
        vec![ContentBlock::Text {
            text: "hello".to_owned(),
            extra: Map::new(),
        }]
    );
}

#[tokio::test]
async fn collect_preserves_source_stream_errors() {
    let stream = stream::iter([Err::<StreamEvent, _>("network unavailable")]);

    let error = collect(stream).await.unwrap_err();

    assert!(matches!(error, CollectError::Stream("network unavailable")));
}
