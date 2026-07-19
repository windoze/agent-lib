//! Unit tests for the one-shot [`Chat`] facade.
//!
//! Every test is fully offline: a scripted [`FakeClient`] returns a fixed
//! [`Response`] and records the requests it received, so no network, credential,
//! or CLI is involved and each test finishes well under a second.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use serde_json::{Map, json};

use super::{Chat, ChatBuilder, ChatSession};
use crate::client::{
    AuthScheme, Capability, ChatRequest, ClientError, EndpointConfig, LlmClient, Response,
};
use crate::conversation::ConversationSnapshot;
use crate::facade::config::ProviderConfig;
use crate::facade::error::FacadeError;
use crate::facade::run::{RunEvent, RunOutput};
use crate::model::content::ContentBlock;
use crate::model::extras::{ProviderExtras, ProviderId};
use crate::model::message::{Message, Role};
use crate::model::normalized::{Normalized, StopReason};
use crate::model::usage::Usage;
use crate::stream::{BlockId, BlockKind, Delta, StreamEvent};

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

fn provider_extras(provider: ProviderId) -> ProviderExtras {
    ProviderExtras {
        provider,
        fields: Map::from_iter([("top_k".to_owned(), json!(25))]),
    }
}

fn provider_config(provider: ProviderId) -> ProviderConfig {
    ProviderConfig::custom(
        EndpointConfig {
            base_url: "https://example.invalid".to_owned(),
            auth: AuthScheme::None,
            query_params: Vec::new(),
            extra_headers: Vec::new(),
        },
        provider,
    )
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
async fn builder_provider_extras_reach_chat_request() {
    let client = Arc::new(FakeClient::new(text_response("done")));
    let extras = provider_extras(ProviderId::Anthropic);
    let chat = Chat::builder()
        .client(client.clone())
        .model("claude-test")
        .provider_extras(extras.clone())
        .build()
        .expect("build chat");

    chat.ask("hi").await.expect("ask succeeds");

    let requests = client.requests.lock().expect("requests mutex");
    assert_eq!(requests[0].provider_extras, Some(extras));
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
fn builder_rejects_blank_model() {
    let error = ChatBuilder::default()
        .client(Arc::new(FakeClient::new(text_response("x"))))
        .model("   ")
        .build()
        .expect_err("blank model is rejected");

    let FacadeError::Config(message) = error else {
        panic!("expected config error")
    };
    assert!(message.contains("model"));
}

#[test]
fn builder_rejects_non_finite_temperature() {
    let error = ChatBuilder::default()
        .client(Arc::new(FakeClient::new(text_response("x"))))
        .model("test-model")
        .temperature(f32::NAN)
        .build()
        .expect_err("non-finite temperature is rejected");

    let FacadeError::Config(message) = error else {
        panic!("expected config error")
    };
    assert!(message.contains("temperature"));
}

#[test]
fn builder_requires_a_client_or_provider() {
    let error = ChatBuilder::default()
        .model("test-model")
        .build()
        .expect_err("missing client and provider is rejected");

    assert!(matches!(error, FacadeError::Config(_)));
}

#[test]
fn builder_rejects_provider_extras_for_different_provider() {
    let error = ChatBuilder::default()
        .provider(provider_config(ProviderId::OpenAiResp))
        .model("gpt-test")
        .provider_extras(provider_extras(ProviderId::Anthropic))
        .build()
        .expect_err("provider mismatch is rejected");

    let FacadeError::Config(message) = error else {
        panic!("expected config error")
    };
    assert!(message.contains("provider_extras"));
}

#[tokio::test]
async fn session_accumulates_history_across_turns() {
    let client = Arc::new(FakeClient::new(text_response("ok")));
    let chat = chat_with(client.clone());
    let mut session = chat.session().build().expect("build session");

    session.send("first").await.expect("first send");
    session.send("second").await.expect("second send");

    // The first request carries only the current user message; the second replays
    // the committed [user, assistant] pair plus the new user message.
    assert_eq!(client.request_message_counts(), vec![1, 3]);

    // The effective view exposes the accumulated [user, assistant, user, assistant].
    let (_system, messages) = session.conversation().effective_view().into_parts();
    assert_eq!(messages.len(), 4);
}

#[tokio::test]
async fn session_build_inherits_chat_system_prompt() {
    let client = Arc::new(FakeClient::new(text_response("ok")));
    let chat = chat_with(client.clone());
    let mut session = chat.session().build().expect("build session");

    session.send("hello").await.expect("send");

    let requests = client.requests.lock().expect("requests mutex");
    assert_eq!(requests[0].system.as_deref(), Some("Answer concisely."));
}

#[tokio::test]
async fn session_system_override_replaces_inherited_prompt() {
    let client = Arc::new(FakeClient::new(text_response("ok")));
    let chat = chat_with(client.clone());
    let mut session = chat
        .session()
        .system("Only speak French.")
        .build()
        .expect("build session");

    session.send("hello").await.expect("send");

    let requests = client.requests.lock().expect("requests mutex");
    assert_eq!(requests[0].system.as_deref(), Some("Only speak French."));
}

#[tokio::test]
async fn session_clear_system_removes_inherited_prompt() {
    let client = Arc::new(FakeClient::new(text_response("ok")));
    let chat = chat_with(client.clone());
    let mut session = chat
        .session()
        .clear_system()
        .build()
        .expect("build session");

    session.send("hello").await.expect("send");

    let requests = client.requests.lock().expect("requests mutex");
    assert_eq!(requests[0].system, None);
}

#[tokio::test]
async fn session_rejects_tool_use() {
    let client = Arc::new(FakeClient::new(tool_use_response()));
    let chat = chat_with(client);
    let mut session = chat.session().build().expect("build session");

    let error = session.send("hi").await.expect_err("tool use is rejected");

    assert!(matches!(error, FacadeError::UnexpectedToolUse));
}

#[tokio::test]
async fn snapshot_is_data_only_and_round_trips() {
    let client = Arc::new(FakeClient::new(text_response("ok")));
    let chat = chat_with(client);
    let mut session = chat.session().build().expect("build session");
    session.send("hello").await.expect("send");

    let snapshot = session.snapshot().expect("snapshot at committed point");
    let json = serde_json::to_string(&snapshot).expect("serialize snapshot");

    // The snapshot is pure conversation data: it round-trips through serde and
    // carries neither a client handle nor any credential.
    let restored: ConversationSnapshot = serde_json::from_str(&json).expect("round-trip");
    assert_eq!(restored, snapshot);
    assert!(!json.contains("client"));
    assert!(!json.contains("api_key"));
    assert!(!json.contains("LlmClient"));
}

#[tokio::test]
async fn restore_continues_history_with_reinjected_client() {
    let client = Arc::new(FakeClient::new(text_response("ok")));
    let chat = chat_with(client);
    let mut session = chat.session().build().expect("build session");
    session.send("first").await.expect("first send");

    let snapshot = session.snapshot().expect("snapshot at committed point");

    // Restore into a fresh session backed by a different Chat: the snapshot
    // restores the committed history, while the client is re-injected from `chat`.
    let restore_client = Arc::new(FakeClient::new(text_response("second-ok")));
    let restore_chat = chat_with(restore_client.clone());
    let mut restored = ChatSession::restore(snapshot, restore_chat).expect("restore");

    let reply = restored.send("second").await.expect("send after restore");
    assert_eq!(reply.text(), "second-ok");

    // The restored session replays the prior [user, assistant] pair plus the new
    // user message, proving history survived the round-trip.
    assert_eq!(restore_client.request_message_counts(), vec![3]);
}

/// A scripted client whose `chat_stream` replays a fixed normalized event
/// sequence and records the request each turn sent.
#[derive(Debug)]
struct StreamingFakeClient {
    events: Vec<StreamEvent>,
    requests: Mutex<Vec<ChatRequest>>,
}

impl StreamingFakeClient {
    fn new(events: Vec<StreamEvent>) -> Self {
        Self {
            events,
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
impl LlmClient for StreamingFakeClient {
    fn capability(&self) -> &Capability {
        &crate::client::ANTHROPIC_DEFAULT_CAPABILITY
    }

    async fn chat(&self, _request: ChatRequest) -> Result<Response, ClientError> {
        Err(ClientError::Other(
            "chat not used in streaming fixture".to_owned(),
        ))
    }

    async fn chat_stream(
        &self,
        request: ChatRequest,
    ) -> Result<BoxStream<'static, Result<StreamEvent, ClientError>>, ClientError> {
        self.requests.lock().expect("requests mutex").push(request);
        let events = self.events.clone();
        Ok(futures::stream::iter(events.into_iter().map(Ok::<_, ClientError>)).boxed())
    }
}

/// A scripted client that serves both `chat` (a fixed [`Response`]) and
/// `chat_stream` (a fixed normalized event sequence), recording every request.
///
/// It lets a test open a stream, drop it early, and then keep driving the same
/// session with a non-streaming `send`, all offline.
#[derive(Debug)]
struct DualFakeClient {
    response: Response,
    events: Vec<StreamEvent>,
    requests: Mutex<Vec<ChatRequest>>,
}

impl DualFakeClient {
    fn new(response: Response, events: Vec<StreamEvent>) -> Self {
        Self {
            response,
            events,
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
impl LlmClient for DualFakeClient {
    fn capability(&self) -> &Capability {
        &crate::client::ANTHROPIC_DEFAULT_CAPABILITY
    }

    async fn chat(&self, request: ChatRequest) -> Result<Response, ClientError> {
        self.requests.lock().expect("requests mutex").push(request);
        Ok(self.response.clone())
    }

    async fn chat_stream(
        &self,
        request: ChatRequest,
    ) -> Result<BoxStream<'static, Result<StreamEvent, ClientError>>, ClientError> {
        self.requests.lock().expect("requests mutex").push(request);
        let events = self.events.clone();
        Ok(futures::stream::iter(events.into_iter().map(Ok::<_, ClientError>)).boxed())
    }
}

/// A stop reason shared by every text stream fixture and its expected response.
fn end_turn() -> Normalized<StopReason> {
    Normalized::from_mapped(StopReason::EndTurn, "end_turn")
}

/// Builds a text response stream: message start, one text block streamed in
/// `chunks`, a fixed usage report, and a normalized end-turn stop.
fn text_stream_events(chunks: &[&str], usage: Usage) -> Vec<StreamEvent> {
    let id = BlockId::new("text-1");
    let mut events = vec![
        StreamEvent::MessageStart {
            role: Role::Assistant,
        },
        StreamEvent::BlockStart {
            id: id.clone(),
            kind: BlockKind::Text,
        },
    ];
    for chunk in chunks {
        events.push(StreamEvent::BlockDelta {
            id: id.clone(),
            delta: Delta::Text((*chunk).to_owned()),
        });
    }
    events.push(StreamEvent::BlockStop { id: id.clone() });
    events.push(StreamEvent::Usage(usage));
    events.push(StreamEvent::MessageStop {
        stop_reason: end_turn(),
    });
    events
}

/// Builds a tool-use response stream that streams one tool-input block.
fn tool_stream_events() -> Vec<StreamEvent> {
    let id = BlockId::new("tool-1");
    vec![
        StreamEvent::MessageStart {
            role: Role::Assistant,
        },
        StreamEvent::BlockStart {
            id: id.clone(),
            kind: BlockKind::ToolInput {
                tool_name: "get_weather".to_owned(),
                tool_call_id: "call-1".to_owned(),
            },
        },
        StreamEvent::BlockDelta {
            id: id.clone(),
            delta: Delta::Json("{\"city\":\"Shanghai\"}".to_owned()),
        },
        StreamEvent::BlockStop { id: id.clone() },
        StreamEvent::MessageStop {
            stop_reason: Normalized::from_mapped(StopReason::ToolUse, "tool_use"),
        },
    ]
}

/// Drains a session stream to completion, returning every yielded item.
async fn drain_stream(
    session: &mut ChatSession,
    input: &str,
) -> Vec<Result<RunEvent, FacadeError>> {
    let mut stream = session.stream(input).await.expect("open stream");
    let mut collected = Vec::new();
    while let Some(item) = stream.next().await {
        collected.push(item);
    }
    collected
}

/// Extracts the ordered `TextDelta` payloads from a drained event list.
fn text_deltas(events: &[Result<RunEvent, FacadeError>]) -> Vec<String> {
    events
        .iter()
        .filter_map(|item| match item {
            Ok(RunEvent::TextDelta(text)) => Some(text.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn stream_forwards_text_deltas_then_commits() {
    let usage = Usage {
        input: 11,
        output: 7,
        ..Usage::default()
    };
    let client = Arc::new(StreamingFakeClient::new(text_stream_events(
        &["Hello ", "world"],
        usage,
    )));
    let chat = chat_with(client.clone());
    let mut session = chat.session().build().expect("build session");

    let events = drain_stream(&mut session, "hi").await;

    // The normalized text deltas arrive in order and concatenate to the full text.
    assert_eq!(text_deltas(&events), vec!["Hello ", "world"]);

    // The raw stream events are forwarded as an escape hatch.
    let raw_count = events
        .iter()
        .filter(|item| matches!(item, Ok(RunEvent::RawStream(_))))
        .count();
    assert!(raw_count > 0, "raw stream events should be forwarded");

    // Exactly one terminal Done carries the aggregated reply and usage.
    let done = match events.last().expect("at least one event") {
        Ok(RunEvent::Done(output)) => output,
        other => panic!("expected terminal Done, got {other:?}"),
    };
    assert_eq!(done.reply.text(), "Hello world");
    assert_eq!(done.usage.supervisor.input, 11);
    assert_eq!(done.usage.supervisor.output, 7);

    // After the stream ends the turn is committed: the effective view holds the
    // [user, assistant] pair.
    let (_system, messages) = session.conversation().effective_view().into_parts();
    assert_eq!(messages.len(), 2);
}

#[tokio::test]
async fn stream_done_matches_the_non_streaming_response() {
    let usage = Usage {
        input: 5,
        output: 9,
        ..Usage::default()
    };
    let client = Arc::new(StreamingFakeClient::new(text_stream_events(
        &["stream ", "done"],
        usage.clone(),
    )));
    let chat = chat_with(client);
    let mut session = chat.session().build().expect("build session");

    let events = drain_stream(&mut session, "hi").await;
    let done = match events.last().expect("at least one event") {
        Ok(RunEvent::Done(output)) => output.clone(),
        other => panic!("expected terminal Done, got {other:?}"),
    };

    // The folded response is identical to what the non-streaming drive builds
    // from the same complete response, so the whole RunOutput compares equal.
    let expected_response = Response {
        message: Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Text {
                text: "stream done".to_owned(),
                extra: Map::new(),
            }],
        },
        usage,
        stop_reason: end_turn(),
        extra: Map::new(),
    };
    assert_eq!(*done, RunOutput::from(expected_response));
}

#[tokio::test]
async fn stream_rejects_tool_use_and_rolls_back() {
    let client = Arc::new(StreamingFakeClient::new(tool_stream_events()));
    let chat = chat_with(client);
    let mut session = chat.session().build().expect("build session");

    let events = drain_stream(&mut session, "hi").await;

    // The terminal item is the tool-use rejection; no Done is produced.
    let error = match events.last().expect("at least one event") {
        Err(error) => error,
        other => panic!("expected terminal error, got {other:?}"),
    };
    assert!(matches!(error, FacadeError::UnexpectedToolUse));
    assert!(
        !events
            .iter()
            .any(|item| matches!(item, Ok(RunEvent::Done(_)))),
        "a rejected tool-use stream must not commit a Done",
    );

    // The in-flight turn was discarded, so no history was committed.
    assert!(session.conversation().turns().is_empty());
    let (_system, messages) = session.conversation().effective_view().into_parts();
    assert!(messages.is_empty());
}

#[tokio::test]
async fn consecutive_streams_accumulate_history() {
    let client = Arc::new(StreamingFakeClient::new(text_stream_events(
        &["ok"],
        Usage::default(),
    )));
    let chat = chat_with(client.clone());
    let mut session = chat.session().build().expect("build session");

    drain_stream(&mut session, "first").await;
    drain_stream(&mut session, "second").await;

    // The first request carries only the current user message; the second replays
    // the committed [user, assistant] pair plus the new user message.
    assert_eq!(client.request_message_counts(), vec![1, 3]);

    // The effective view exposes [user, assistant, user, assistant].
    let (_system, messages) = session.conversation().effective_view().into_parts();
    assert_eq!(messages.len(), 4);
}

#[tokio::test]
async fn stream_dropped_before_polling_leaves_session_usable() {
    let client = Arc::new(DualFakeClient::new(
        text_response("recovered"),
        text_stream_events(&["ignored"], Usage::default()),
    ));
    let chat = chat_with(client.clone());
    let mut session = chat.session().build().expect("build session");

    // Open a stream but drop it before polling a single event. The pending turn
    // opened by `stream` must be rolled back by the drop guard.
    {
        let _stream = session.stream("first").await.expect("open stream");
    }

    // The abandoned turn left no committed history and no stranded pending turn,
    // so a subsequent non-streaming `send` succeeds and starts from scratch.
    let reply = session.send("second").await.expect("send after early drop");
    assert_eq!(reply.text(), "recovered");

    // The dropped stream contributed nothing: only the committed [user, assistant]
    // pair from the successful `send` remains.
    let (_system, messages) = session.conversation().effective_view().into_parts();
    assert_eq!(messages.len(), 2);
}

#[tokio::test]
async fn stream_dropped_after_delta_does_not_commit_partial_turn() {
    let client = Arc::new(DualFakeClient::new(
        text_response("recovered"),
        text_stream_events(&["Hel", "lo"], Usage::default()),
    ));
    let chat = chat_with(client.clone());
    let mut session = chat.session().build().expect("build session");

    // Read at least one text delta, then drop the stream mid-flight.
    {
        let mut stream = session.stream("first").await.expect("open stream");
        let mut saw_delta = false;
        while let Some(item) = stream.next().await {
            if matches!(item.expect("stream item ok"), RunEvent::TextDelta(_)) {
                saw_delta = true;
                break;
            }
        }
        assert!(saw_delta, "stream should yield at least one text delta");
    }

    // `snapshot` requires a committed consistency point: it only succeeds if the
    // drop guard rolled the half-streamed pending turn back.
    let _snapshot = session.snapshot().expect("snapshot after mid-stream drop");
    let (_system, messages) = session.conversation().effective_view().into_parts();
    assert!(
        messages.is_empty(),
        "no half assistant turn should have been committed",
    );

    // The next `send` replays no uncommitted assistant turn: with no committed
    // history the recorded request carries exactly the single new user message.
    let reply = session
        .send("second")
        .await
        .expect("send after mid-stream drop");
    assert_eq!(reply.text(), "recovered");
    assert_eq!(
        client
            .request_message_counts()
            .last()
            .copied()
            .expect("a send was recorded"),
        1,
    );
}

#[tokio::test]
async fn stream_dropped_after_completion_keeps_committed_turn() {
    let client = Arc::new(DualFakeClient::new(
        text_response("recovered"),
        text_stream_events(&["all ", "done"], Usage::default()),
    ));
    let chat = chat_with(client.clone());
    let mut session = chat.session().build().expect("build session");

    // Drain the stream to its terminal `Done`, then drop it. The commit that the
    // terminal `Done` performed must survive the drop.
    {
        let mut stream = session.stream("first").await.expect("open stream");
        let mut saw_done = false;
        while let Some(item) = stream.next().await {
            if matches!(item.expect("stream item ok"), RunEvent::Done(_)) {
                saw_done = true;
            }
        }
        assert!(saw_done, "stream should reach a terminal Done");
    }

    // The committed [user, assistant] pair is still present right after the drop.
    let (_system, messages) = session.conversation().effective_view().into_parts();
    assert_eq!(messages.len(), 2);

    // A follow-up send replays the committed pair plus the new user message,
    // proving the completed turn was not rolled back on drop.
    session
        .send("second")
        .await
        .expect("send after completed drop");
    assert_eq!(client.request_message_counts(), vec![1, 3]);
}
