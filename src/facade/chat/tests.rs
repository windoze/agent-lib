//! Unit tests for the one-shot [`Chat`] facade.
//!
//! Every test is fully offline: a scripted [`FakeClient`] returns a fixed
//! [`Response`] and records the requests it received, so no network, credential,
//! or CLI is involved and each test finishes well under a second.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::stream::BoxStream;
use serde_json::{Map, json};

use super::{Chat, ChatBuilder};
use crate::client::{Capability, ChatRequest, ClientError, LlmClient, Response};
use crate::facade::error::FacadeError;
use crate::model::content::ContentBlock;
use crate::model::message::{Message, Role};
use crate::model::normalized::StopReason;
use crate::model::usage::Usage;
use crate::stream::StreamEvent;

/// A scripted client that returns a fixed response and records each request.
#[derive(Debug)]
struct FakeClient {
    response: Response,
    requests: Mutex<Vec<ChatRequest>>,
}

impl FakeClient {
    fn new(response: Response) -> Self {
        Self {
            response,
            requests: Mutex::new(Vec::new()),
        }
    }

    /// Returns the number of messages sent on each recorded request, in order.
    fn request_message_counts(&self) -> Vec<usize> {
        self.requests
            .lock()
            .expect("requests mutex")
            .iter()
            .map(|request| request.messages.len())
            .collect()
    }
}

#[async_trait]
impl LlmClient for FakeClient {
    fn capability(&self) -> &Capability {
        &crate::client::ANTHROPIC_DEFAULT_CAPABILITY
    }

    async fn chat(&self, request: ChatRequest) -> Result<Response, ClientError> {
        self.requests.lock().expect("requests mutex").push(request);
        Ok(self.response.clone())
    }

    async fn chat_stream(
        &self,
        _request: ChatRequest,
    ) -> Result<BoxStream<'static, Result<StreamEvent, ClientError>>, ClientError> {
        Err(ClientError::Other(
            "streaming not used in fixture".to_owned(),
        ))
    }
}

/// Builds an assistant response carrying only the given text.
fn text_response(text: &str) -> Response {
    Response {
        message: Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Text {
                text: text.to_owned(),
                extra: Map::new(),
            }],
        },
        usage: Usage {
            input: 11,
            output: 7,
            ..Usage::default()
        },
        stop_reason: StopReason::normalize("end_turn"),
        extra: Map::new(),
    }
}

/// Builds an assistant response that asks to call a tool.
fn tool_use_response() -> Response {
    Response {
        message: Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "call-1".to_owned(),
                name: "get_weather".to_owned(),
                input: json!({ "city": "Shanghai" }),
                extra: Map::new(),
            }],
        },
        usage: Usage::default(),
        stop_reason: StopReason::normalize("tool_use"),
        extra: Map::new(),
    }
}

/// Builds a [`Chat`] driven by the supplied client.
fn chat_with(client: Arc<dyn LlmClient>) -> Chat {
    Chat::builder()
        .client(client)
        .model("test-model")
        .system("Answer concisely.")
        .build()
        .expect("build chat")
}

#[tokio::test]
async fn ask_returns_aggregated_text() {
    let client = Arc::new(FakeClient::new(text_response("hello world")));
    let chat = chat_with(client.clone());

    let reply = chat.ask("hi").await.expect("ask succeeds");

    assert_eq!(reply.text(), "hello world");
    assert_eq!(reply.stop_reason(), Some(&StopReason::EndTurn));
    // The one request carries only the current user message.
    assert_eq!(client.request_message_counts(), vec![1]);
}

#[tokio::test]
async fn ask_full_reports_response_and_supervisor_usage() {
    let client = Arc::new(FakeClient::new(text_response("done")));
    let chat = chat_with(client.clone());

    let output = chat.ask_full("hi").await.expect("ask_full succeeds");

    assert_eq!(output.reply.text(), "done");
    let response = output.response.as_ref().expect("response retained");
    assert_eq!(response.message.content.len(), 1);
    assert_eq!(output.usage.supervisor.input, 11);
    assert_eq!(output.usage.supervisor.output, 7);
    assert_eq!(output.usage.total().output, 7);
    assert!(output.tool_calls.is_empty());
    assert!(output.events.is_empty());

    // The request carried the system prompt and only the current user message.
    let requests = client.requests.lock().expect("requests mutex");
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].system.as_deref(), Some("Answer concisely."));
    assert_eq!(requests[0].model, "test-model");
    assert!(requests[0].tools.is_empty());
    assert!(!requests[0].stream);
}

#[tokio::test]
async fn tool_use_response_is_rejected() {
    let client = Arc::new(FakeClient::new(tool_use_response()));
    let chat = chat_with(client);

    let error = chat.ask("hi").await.expect_err("tool use is rejected");

    assert!(matches!(error, FacadeError::UnexpectedToolUse));
}

#[tokio::test]
async fn consecutive_asks_do_not_retain_history() {
    let client = Arc::new(FakeClient::new(text_response("ok")));
    let chat = chat_with(client.clone());

    chat.ask("first").await.expect("first ask");
    chat.ask("second").await.expect("second ask");

    // Each one-shot uses a throwaway conversation, so neither request replays
    // the other's messages: both carry exactly one (the current user) message.
    assert_eq!(client.request_message_counts(), vec![1, 1]);
}

#[test]
fn builder_requires_a_model() {
    let error = ChatBuilder::default()
        .client(Arc::new(FakeClient::new(text_response("x"))))
        .build()
        .expect_err("missing model is rejected");

    assert!(matches!(error, FacadeError::Config(_)));
}

#[test]
fn builder_requires_a_client_or_provider() {
    let error = ChatBuilder::default()
        .model("test-model")
        .build()
        .expect_err("missing client and provider is rejected");

    assert!(matches!(error, FacadeError::Config(_)));
}
