//! Local HTTP coverage for the public Responses streaming entry point.

use super::*;
use crate::{
    client::{AuthScheme, EndpointConfig, LlmClient},
    model::{content::ContentBlock, message::Message},
};
use futures::TryStreamExt;
use serde_json::Map;
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    task::JoinHandle,
    time::timeout,
};

/// Starts a one-shot HTTP server that returns the supplied response body.
async fn serve_once(
    status: &str,
    content_type: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> (String, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind local streaming server");
    let address = listener.local_addr().expect("read local server address");
    let mut response = format!(
        "HTTP/1.1 {status}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n",
        body.len()
    );
    for (name, value) in headers {
        response.push_str(name);
        response.push_str(": ");
        response.push_str(value);
        response.push_str("\r\n");
    }
    response.push_str("\r\n");
    response.push_str(body);

    let task = tokio::spawn(async move {
        let (mut socket, _) = timeout(Duration::from_secs(5), listener.accept())
            .await
            .expect("stream client should connect within five seconds")
            .expect("accept streaming request");
        let mut request = [0_u8; 16_384];
        let bytes_read = timeout(Duration::from_secs(5), socket.read(&mut request))
            .await
            .expect("stream client should write within five seconds")
            .expect("read streaming request");
        let request = String::from_utf8_lossy(&request[..bytes_read]);
        assert!(request.contains("POST /responses"));
        assert!(request.contains("\"stream\":true"));
        socket
            .write_all(response.as_bytes())
            .await
            .expect("write local streaming response");
        socket.shutdown().await.expect("close local response");
    });

    (format!("http://{address}"), task)
}

/// Builds an unauthenticated local adapter.
fn local_adapter(base_url: String) -> OpenAiRespAdapter {
    OpenAiRespAdapter::new(EndpointConfig {
        base_url,
        auth: AuthScheme::None,
        query_params: Vec::new(),
        extra_headers: Vec::new(),
    })
}

/// Constructs a small request with an explicit streaming mode.
fn request(stream: bool) -> ChatRequest {
    ChatRequest {
        model: "gpt-5.5".to_owned(),
        messages: vec![Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "Say hi in exactly two words.".to_owned(),
                extra: Map::new(),
            }],
        }],
        tools: Vec::new(),
        system: None,
        max_tokens: 64,
        temperature: None,
        stream,
        provider_extras: None,
    }
}

/// Bounds both response-header acquisition and complete event consumption.
async fn collect_with_timeout(
    client: &dyn LlmClient,
    request: ChatRequest,
) -> Result<Vec<StreamEvent>, ClientError> {
    timeout(Duration::from_secs(5), async {
        client
            .chat_stream(request)
            .await?
            .try_collect::<Vec<_>>()
            .await
    })
    .await
    .expect("local streaming chat should finish within five seconds")
}

#[tokio::test]
async fn boxed_client_sends_sse_request_and_yields_foldable_events() {
    let (base_url, server) = serve_once(
        "200 OK",
        "text/event-stream; charset=utf-8",
        &[],
        REAL_TEXT_STREAM,
    )
    .await;
    let client: Box<dyn LlmClient> = Box::new(local_adapter(base_url));

    assert!(client.capability().streaming);
    let events = collect_with_timeout(client.as_ref(), request(true))
        .await
        .expect("streaming Responses chat should succeed");
    server.await.expect("local streaming server should finish");

    let response = fold_events(&events).expect("local transport events should fold");
    assert_eq!(response.message.role, Role::Assistant);
    assert_eq!(response.usage.input, 12);
    assert_eq!(response.usage.output, 19);
    assert!(response.extra.contains_key("content_filters"));
}

#[tokio::test]
async fn streaming_chat_classifies_http_errors_and_retry_after() {
    let body = r#"{"error":{"code":"rate_limit_exceeded","message":"slow down"}}"#;
    let (base_url, server) = serve_once(
        "429 Too Many Requests",
        "application/json",
        &[("retry-after", "4")],
        body,
    )
    .await;
    let adapter = local_adapter(base_url);

    let error = match adapter.chat_stream(request(true)).await {
        Err(error) => error,
        Ok(_) => panic!("rate-limited stream should fail before returning a body stream"),
    };
    server.await.expect("local error server should finish");

    assert_eq!(
        error,
        ClientError::RateLimited {
            retry_after: Some(Duration::from_secs(4)),
        }
    );
}

#[tokio::test]
async fn successful_stream_requires_event_stream_content_type() {
    let (base_url, server) = serve_once("200 OK", "application/json", &[], "{}").await;
    let adapter = local_adapter(base_url);

    let error = match adapter.chat_stream(request(true)).await {
        Err(error) => error,
        Ok(_) => panic!("JSON success body must not enter SSE parsing"),
    };
    server
        .await
        .expect("local content-type server should finish");

    assert!(matches!(error, ClientError::Protocol(_)));
    assert!(error.to_string().contains("application/json"));
}

#[tokio::test]
async fn truncated_body_surfaces_as_a_stream_item_error() {
    let truncated = concat!(
        "event: response.created\n",
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_truncated\",\"object\":\"response\",\"status\":\"in_progress\",\"output\":[],\"usage\":null},\"sequence_number\":0}\n\n"
    );
    let (base_url, server) = serve_once("200 OK", "text/event-stream", &[], truncated).await;
    let adapter = local_adapter(base_url);

    let error = collect_with_timeout(&adapter, request(true))
        .await
        .expect_err("truncated SSE body must fail during consumption");
    server.await.expect("local truncated server should finish");

    assert!(matches!(error, ClientError::Protocol(_)));
    assert!(error.to_string().contains("terminal response event"));
}

#[tokio::test]
async fn streaming_chat_rejects_non_stream_request_before_transport() {
    let adapter = local_adapter("http://127.0.0.1:1".to_owned());

    let error = match adapter.chat_stream(request(false)).await {
        Err(error) => error,
        Ok(_) => panic!("non-stream request must be rejected"),
    };

    assert!(matches!(error, ClientError::Protocol(_)));
    assert!(error.to_string().contains("stream to be true"));
}
