//! Local HTTP tests for the non-streaming adapter entry point.

use super::*;
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    task::JoinHandle,
    time::timeout,
};

/// Starts a one-shot local HTTP server and returns its endpoint plus task.
async fn serve_once(
    status: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> (String, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind local response server");
    let address = listener.local_addr().expect("read local server address");
    let mut response = format!(
        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n",
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
            .expect("client should connect within five seconds")
            .expect("accept local request");
        let mut request = [0_u8; 8_192];
        let bytes_read = timeout(Duration::from_secs(5), socket.read(&mut request))
            .await
            .expect("client should write within five seconds")
            .expect("read local request");
        assert!(bytes_read > 0, "client should send an HTTP request");
        socket
            .write_all(response.as_bytes())
            .await
            .expect("write local response");
        socket.shutdown().await.expect("close local response");
    });

    (format!("http://{address}"), task)
}

/// Bounds a local adapter call so a transport regression cannot hang a test.
async fn chat_with_timeout(
    adapter: &AnthropicAdapter,
    request: ChatRequest,
) -> Result<Response, ClientError> {
    timeout(Duration::from_secs(5), adapter.chat(request))
        .await
        .expect("local chat should finish within five seconds")
}

/// Verifies the public transport path decodes a successful complete response.
#[tokio::test]
async fn chat_sends_and_parses_non_streaming_response() {
    let (base_url, server) = serve_once("200 OK", &[], REAL_TEXT_RESPONSE).await;
    let adapter = AnthropicAdapter::new(local_endpoint(base_url));

    let response = chat_with_timeout(&adapter, minimal_request())
        .await
        .expect("complete Anthropic chat should succeed");
    server.await.expect("local server should finish");

    assert_eq!(response.message.role, Role::Assistant);
    assert_eq!(response.usage.input, 14);
    assert_eq!(response.usage.output, 7);
}

/// Verifies unsuccessful responses use the shared retry-aware classification.
#[tokio::test]
async fn chat_classifies_http_errors_and_retry_after() {
    let body = r#"{"type":"error","error":{"type":"rate_limit_error","message":"slow down"}}"#;
    let (base_url, server) =
        serve_once("429 Too Many Requests", &[("retry-after", "3")], body).await;
    let adapter = AnthropicAdapter::new(local_endpoint(base_url));

    let error = chat_with_timeout(&adapter, minimal_request())
        .await
        .expect_err("rate-limited request should fail");
    server.await.expect("local server should finish");

    assert_eq!(
        error,
        ClientError::RateLimited {
            retry_after: Some(Duration::from_secs(3)),
        }
    );
}

/// Verifies a successful status never masks an invalid provider response body.
#[tokio::test]
async fn chat_rejects_invalid_success_body() {
    let (base_url, server) = serve_once("200 OK", &[], r#"{"unexpected":true}"#).await;
    let adapter = AnthropicAdapter::new(local_endpoint(base_url));

    let error = chat_with_timeout(&adapter, minimal_request())
        .await
        .expect_err("invalid success body should fail");
    server.await.expect("local server should finish");

    assert!(matches!(error, ClientError::Protocol(_)));
    assert!(error.to_string().contains("Anthropic Messages response"));
}

/// Verifies complete-response code cannot accidentally consume an SSE request.
#[tokio::test]
async fn chat_rejects_streaming_request_before_transport() {
    let adapter = AnthropicAdapter::new(local_endpoint("http://127.0.0.1:1".to_owned()));
    let mut request = minimal_request();
    request.stream = true;

    let error = chat_with_timeout(&adapter, request)
        .await
        .expect_err("streaming request should be rejected");

    assert!(matches!(error, ClientError::Protocol(_)));
    assert!(error.to_string().contains("stream to be false"));
}
