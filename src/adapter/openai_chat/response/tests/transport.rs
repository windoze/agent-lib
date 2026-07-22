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
///
/// The server reads one request, asserts the chat/completions request line, and
/// replies with the supplied status line, headers, and body before closing. It
/// serves exactly one connection, so each test builds its own server.
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
        let request = String::from_utf8_lossy(&request[..bytes_read]);
        assert!(
            request.starts_with("POST /chat/completions HTTP/1.1"),
            "adapter should call the Chat/Completions path: {request}"
        );
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
    adapter: &OpenAiChatAdapter,
    request: ChatRequest,
) -> Result<Response, ClientError> {
    timeout(Duration::from_secs(5), adapter.chat(request))
        .await
        .expect("local chat should finish within five seconds")
}

#[tokio::test]
async fn chat_sends_and_parses_non_streaming_response() {
    let (base_url, server) = serve_once("200 OK", &[], REAL_TEXT_RESPONSE).await;
    let adapter = OpenAiChatAdapter::new(local_endpoint(base_url));

    let response = chat_with_timeout(&adapter, minimal_request())
        .await
        .expect("complete chat/completions chat should succeed");
    server.await.expect("local server should finish");

    assert_eq!(response.message.role, Role::Assistant);
    assert_eq!(response.usage.input, 13);
    assert_eq!(response.usage.output, 26);
}

#[tokio::test]
async fn chat_classifies_rate_limit_with_retry_after() {
    let body = r#"{"error":{"type":"rate_limit_error","message":"slow down"}}"#;
    let (base_url, server) =
        serve_once("429 Too Many Requests", &[("retry-after", "3")], body).await;
    let adapter = OpenAiChatAdapter::new(local_endpoint(base_url));

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

#[tokio::test]
async fn chat_classifies_unauthorized_as_auth_error() {
    let body = r#"{"error":{"type":"invalid_api_key","message":"Incorrect API key"}}"#;
    let (base_url, server) = serve_once("401 Unauthorized", &[], body).await;
    let adapter = OpenAiChatAdapter::new(local_endpoint(base_url));

    let error = chat_with_timeout(&adapter, minimal_request())
        .await
        .expect_err("unauthorized request should fail");
    server.await.expect("local server should finish");

    assert!(matches!(error, ClientError::Auth), "got {error:?}");
}

#[tokio::test]
async fn chat_classifies_context_length_error_body() {
    let body = r#"{"error":{"message":"This model's maximum context length is 8192 tokens. However, your messages resulted in 9001 tokens.","type":"invalid_request_error","code":"context_length_exceeded"}}"#;
    let (base_url, server) = serve_once("400 Bad Request", &[], body).await;
    let adapter = OpenAiChatAdapter::new(local_endpoint(base_url));

    let error = chat_with_timeout(&adapter, minimal_request())
        .await
        .expect_err("context-length body should classify");
    server.await.expect("local server should finish");

    assert!(
        matches!(error, ClientError::ContextLengthExceeded),
        "got {error:?}"
    );
}

#[tokio::test]
async fn chat_classifies_content_filter_error_body() {
    let body =
        r#"{"error":{"code":"content_filter","message":"Output blocked by the content filter."}}"#;
    let (base_url, server) = serve_once("400 Bad Request", &[], body).await;
    let adapter = OpenAiChatAdapter::new(local_endpoint(base_url));

    let error = chat_with_timeout(&adapter, minimal_request())
        .await
        .expect_err("content-filter body should classify");
    server.await.expect("local server should finish");

    assert!(
        matches!(error, ClientError::ContentFiltered),
        "got {error:?}"
    );
}

#[tokio::test]
async fn chat_classifies_other_http_error_as_api() {
    let body = r#"{"error":{"type":"server_error","message":"boom"}}"#;
    let (base_url, server) = serve_once("500 Internal Server Error", &[], body).await;
    let adapter = OpenAiChatAdapter::new(local_endpoint(base_url));

    let error = chat_with_timeout(&adapter, minimal_request())
        .await
        .expect_err("other HTTP error should fail");
    server.await.expect("local server should finish");

    match error {
        ClientError::Api { status, body } => {
            assert_eq!(status, 500);
            assert!(
                body.contains("server_error"),
                "error body should be retained: {body}"
            );
        }
        other => panic!("expected Api error, got {other:?}"),
    }
}
