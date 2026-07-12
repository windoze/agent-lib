use super::{Capability, ChatRequest, ClientError, LlmClient, Response};
use crate::{
    model::{
        content::ContentBlock,
        message::{Message, Role},
        normalized::{Normalized, StopReason},
        usage::Usage,
    },
    stream::{BlockId, BlockKind, Delta, StreamEvent, accumulator::collect},
};
use async_trait::async_trait;
use futures::{StreamExt, stream};
use serde_json::Map;

/// Minimal client used to prove that both async entry points remain callable
/// after erasing the concrete implementation type.
struct MockClient {
    capability: Capability,
}

impl MockClient {
    /// Creates a mock that advertises the streaming path exercised below.
    fn new() -> Self {
        Self {
            capability: Capability {
                streaming: true,
                ..Capability::default()
            },
        }
    }
}

#[async_trait]
impl LlmClient for MockClient {
    fn capability(&self) -> &Capability {
        &self.capability
    }

    async fn chat(&self, _request: ChatRequest) -> Result<Response, ClientError> {
        Ok(expected_response())
    }

    async fn chat_stream(
        &self,
        _request: ChatRequest,
    ) -> Result<futures::stream::BoxStream<'static, Result<StreamEvent, ClientError>>, ClientError>
    {
        let id = BlockId::new("mock-text-1");
        let events: Vec<Result<StreamEvent, ClientError>> = vec![
            Ok(StreamEvent::MessageStart {
                role: Role::Assistant,
            }),
            Ok(StreamEvent::BlockStart {
                id: id.clone(),
                kind: BlockKind::Text,
            }),
            Ok(StreamEvent::BlockDelta {
                id: id.clone(),
                delta: Delta::Text("hel".to_owned()),
            }),
            Ok(StreamEvent::BlockDelta {
                id: id.clone(),
                delta: Delta::Text("lo".to_owned()),
            }),
            Ok(StreamEvent::BlockStop { id }),
            Ok(StreamEvent::Usage(Usage {
                input: 2,
                output: 1,
                ..Usage::default()
            })),
            Ok(StreamEvent::MessageStop {
                stop_reason: Normalized::from_mapped(StopReason::EndTurn, "end_turn"),
            }),
        ];

        Ok(stream::iter(events).boxed())
    }
}

/// Builds a provider-neutral request accepted by both mock entry points.
fn request(stream: bool) -> ChatRequest {
    ChatRequest {
        model: "mock-model".to_owned(),
        messages: Vec::new(),
        tools: Vec::new(),
        system: None,
        max_tokens: 32,
        temperature: None,
        stream,
        provider_extras: None,
    }
}

/// Returns the complete response represented by the mock event sequence.
fn expected_response() -> Response {
    Response {
        message: Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Text {
                text: "hello".to_owned(),
                extra: Map::new(),
            }],
        },
        usage: Usage {
            input: 2,
            output: 1,
            ..Usage::default()
        },
        stop_reason: Normalized::from_mapped(StopReason::EndTurn, "end_turn"),
        extra: Map::new(),
    }
}

#[tokio::test]
async fn boxed_dyn_client_supports_complete_and_streaming_calls() {
    let client: Box<dyn LlmClient> = Box::new(MockClient::new());

    assert!(client.capability().streaming);

    let complete = client
        .chat(request(false))
        .await
        .expect("mock complete response");
    assert_eq!(complete, expected_response());

    let events = client
        .chat_stream(request(true))
        .await
        .expect("mock event stream");
    let folded = collect(events).await.expect("fold mock event stream");

    assert_eq!(folded, complete);
}
