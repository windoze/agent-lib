//! Shared HTTP transport limits for the built-in LLM adapters.
//!
//! The two wire adapters (`anthropic`, `openai_resp`) historically created a
//! bare `reqwest::Client` with no timeouts and read error bodies without any
//! size or time bound, so a peer that kept a connection open without sending
//! data could hang a request forever. This module holds the default limits
//! that close that hole, plus the bounded error-body reader used by all four
//! non-2xx paths.
//!
//! Deliberate design choices:
//!
//! - `reqwest::Client::timeout()` is **never** used for the overall request,
//!   because it covers the entire body read and would kill healthy long-lived
//!   SSE streams. Instead the adapters wrap only the phases that must be
//!   bounded (connect, and "connect + response headers" for streaming) in
//!   `tokio::time::timeout` themselves.
//! - Error bodies are read chunk-by-chunk up to [`ERROR_BODY_MAX_BYTES`]; a
//!   truncated body is marked with a `[truncated]` suffix so callers can tell
//!   the evidence is incomplete.

use std::time::Duration;

use futures::StreamExt;
use reqwest::Response;

use crate::client::ClientError;

/// Connect timeout applied to the default client built by `Adapter::new`.
pub(crate) const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Total timeout for one non-streaming `chat()` request, and for the
/// "connect + response headers" phase of `chat_stream()`.
///
/// Streaming bodies are intentionally not covered: a long-lived SSE stream is
/// the normal case, not a hang.
pub(crate) const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(600);

/// Independent timeout for reading a non-2xx error body.
pub(crate) const ERROR_BODY_READ_TIMEOUT: Duration = Duration::from_secs(30);

/// Maximum bytes retained from a non-2xx error body before truncating.
pub(crate) const ERROR_BODY_MAX_BYTES: usize = 1024 * 1024;

/// Suffix appended to an error body that hit [`ERROR_BODY_MAX_BYTES`].
pub(crate) const TRUNCATED_SUFFIX: &str = "[truncated]";

/// Replacement for the entire query string of a URL embedded in a transport
/// error message. Query parameters are where deployments most often stash
/// credentials (`?key=...`), so the whole query is hidden rather than trying
/// to pick out individual sensitive values.
pub(crate) const REDACTED_QUERY: &str = "[REDACTED]";

/// Builds the default reusable HTTP client for `Adapter::new`.
///
/// Only the connect timeout is set here; per-request phase timeouts are
/// applied by the adapter entry points so long SSE bodies stay unbounded.
pub(crate) fn default_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(DEFAULT_CONNECT_TIMEOUT)
        .build()
        .expect("default reqwest client (connect timeout only) must build")
}

/// Maps reqwest failures into retry-relevant client error classes.
///
/// Shared by every adapter transport path so the classification and the
/// redaction behavior stay identical across all four call sites. Timeouts map
/// to [`ClientError::Timeout`]; everything else becomes
/// [`ClientError::Network`] with the error's Display text.
///
/// reqwest embeds the full request URL in that Display text, and
/// [`crate::client::EndpointConfig::query_params`] may carry credentials such
/// as `?key=...`, so when the error knows its URL the embedded copy has its
/// entire query string replaced with [`REDACTED_QUERY`]. When the error
/// carries no URL the message is used verbatim.
pub(crate) fn map_transport_error(error: reqwest::Error) -> ClientError {
    if error.is_timeout() {
        return ClientError::Timeout;
    }

    let message = match error.url() {
        Some(url) => {
            let redacted = redact_url_query(url.as_str());
            if redacted == url.as_str() {
                error.to_string()
            } else {
                // The query string can only leak through the embedded URL
                // text; if the Display text does not contain the URL verbatim
                // the replacement is a harmless no-op.
                error.to_string().replace(url.as_str(), &redacted)
            }
        }
        None => error.to_string(),
    };
    ClientError::Network(message)
}

/// Returns `url` with its entire query string replaced by [`REDACTED_QUERY`],
/// keeping any trailing fragment. URLs without a query are returned unchanged.
fn redact_url_query(url: &str) -> String {
    let Some(query_start) = url.find('?') else {
        return url.to_owned();
    };
    // A fragment can only appear after the query, so look for it there.
    let fragment = url[query_start..]
        .find('#')
        .map(|offset| &url[query_start + offset..]);

    let mut redacted = String::with_capacity(query_start + 1 + REDACTED_QUERY.len());
    redacted.push_str(&url[..=query_start]);
    redacted.push_str(REDACTED_QUERY);
    if let Some(fragment) = fragment {
        redacted.push_str(fragment);
    }
    redacted
}

/// Reads a non-2xx error body with the default size cap and read timeout.
///
/// Returns the body as lossy UTF-8 text, truncated to
/// [`ERROR_BODY_MAX_BYTES`] with a [`TRUNCATED_SUFFIX`] marker when the peer
/// sent more. A peer that stalls the body read produces
/// [`ClientError::Timeout`] after [`ERROR_BODY_READ_TIMEOUT`].
pub(crate) async fn read_error_body(response: Response) -> Result<String, ClientError> {
    read_error_body_bounded(
        response.bytes_stream(),
        ERROR_BODY_READ_TIMEOUT,
        ERROR_BODY_MAX_BYTES,
    )
    .await
}

/// Reads an error body stream with an explicit timeout and byte cap.
///
/// Kept separate from [`read_error_body`] so tests can exercise the bounding
/// logic with in-memory streams and short timeouts. Reading stops at the cap
/// (the rest of the body is dropped, not drained); transport errors map to
/// [`ClientError::Network`] and the deadline to [`ClientError::Timeout`].
async fn read_error_body_bounded<S, B>(
    body: S,
    timeout: Duration,
    max_bytes: usize,
) -> Result<String, ClientError>
where
    S: futures::Stream<Item = Result<B, reqwest::Error>>,
    B: AsRef<[u8]>,
{
    match tokio::time::timeout(timeout, collect_bounded(body, max_bytes)).await {
        Ok(Ok(text)) => Ok(text),
        Ok(Err(error)) => Err(map_transport_error(error)),
        Err(_elapsed) => Err(ClientError::Timeout),
    }
}

/// Accumulates body chunks up to `max_bytes`, marking truncation.
async fn collect_bounded<S, B>(body: S, max_bytes: usize) -> Result<String, reqwest::Error>
where
    S: futures::Stream<Item = Result<B, reqwest::Error>>,
    B: AsRef<[u8]>,
{
    tokio::pin!(body);
    let mut buffer = Vec::new();
    let mut truncated = false;
    while let Some(chunk) = body.next().await {
        let chunk = chunk?;
        let chunk = chunk.as_ref();
        let remaining = max_bytes.saturating_sub(buffer.len());
        if chunk.len() > remaining {
            buffer.extend_from_slice(&chunk[..remaining]);
            truncated = true;
            break;
        }
        buffer.extend_from_slice(chunk);
    }

    let mut text = String::from_utf8_lossy(&buffer).into_owned();
    if truncated {
        text.push_str(TRUNCATED_SUFFIX);
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream;

    /// A stream that delivers more bytes than the cap is truncated and marked.
    #[tokio::test]
    async fn oversized_error_body_is_truncated_and_marked() {
        let chunks: Vec<Result<Vec<u8>, reqwest::Error>> = (0..4)
            .map(|index| Ok(vec![b'a' + index as u8; 1024]))
            .collect();
        let body = stream::iter(chunks);

        let text = read_error_body_bounded(body, Duration::from_secs(5), 1500)
            .await
            .expect("in-memory stream must not fail");

        assert_eq!(text.len(), 1500 + TRUNCATED_SUFFIX.len());
        assert!(text.ends_with(TRUNCATED_SUFFIX));
        assert!(text.starts_with("aaa"));
    }

    /// A body exactly at the cap is kept verbatim without the marker.
    #[tokio::test]
    async fn body_at_exact_cap_is_not_marked() {
        let chunks: Vec<Result<&[u8], reqwest::Error>> = vec![Ok(b"hello")];
        let text = read_error_body_bounded(stream::iter(chunks), Duration::from_secs(5), 5)
            .await
            .expect("in-memory stream must not fail");
        assert_eq!(text, "hello");
    }

    /// A stalled body read surfaces as a timeout instead of hanging.
    #[tokio::test]
    async fn stalled_error_body_times_out() {
        let body = stream::pending::<Result<Vec<u8>, reqwest::Error>>();
        let started = std::time::Instant::now();
        let error = read_error_body_bounded(body, Duration::from_millis(10), 1024)
            .await
            .expect_err("stalled stream must time out");
        assert!(matches!(error, ClientError::Timeout));
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "timeout path must return promptly"
        );
    }

    /// The default client builder wires the documented connect timeout.
    #[test]
    fn default_client_builds() {
        let _client = default_http_client();
    }

    /// A URL with a query string keeps path and fragment but loses the query.
    #[test]
    fn redact_url_query_replaces_entire_query() {
        assert_eq!(
            redact_url_query("https://example.test/v1/chat?api-key=secret&api-version=1"),
            "https://example.test/v1/chat?[REDACTED]"
        );
        assert_eq!(
            redact_url_query("https://example.test/v1/chat?key=secret#frag"),
            "https://example.test/v1/chat?[REDACTED]#frag"
        );
    }

    /// URLs without a query string pass through unchanged.
    #[test]
    fn redact_url_query_leaves_queryless_urls_untouched() {
        assert_eq!(
            redact_url_query("https://example.test/v1/chat"),
            "https://example.test/v1/chat"
        );
        assert_eq!(
            redact_url_query("https://example.test/v1/chat#frag"),
            "https://example.test/v1/chat#frag"
        );
    }

    /// A real transport failure against an unreachable loopback address keeps
    /// the host visible but never leaks the query string into the message.
    #[tokio::test]
    async fn transport_error_message_redacts_url_query() {
        let error = default_http_client()
            .get("http://127.0.0.1:1/v1/chat?api-key=secret")
            .send()
            .await
            .expect_err("loopback port 1 must refuse the connection");

        let mapped = map_transport_error(error);
        let ClientError::Network(message) = mapped else {
            panic!("connect failure must map to Network, got {mapped:?}");
        };
        assert!(!message.contains("secret"), "query leaked: {message}");
        assert!(!message.contains("api-key"), "query leaked: {message}");
        assert!(
            message.contains("127.0.0.1:1"),
            "URL context should survive redaction: {message}"
        );
        assert!(message.contains("[REDACTED]"), "missing marker: {message}");
    }

    /// Queryless transport errors keep their original message verbatim.
    #[tokio::test]
    async fn transport_error_without_query_keeps_message() {
        let error = default_http_client()
            .get("http://127.0.0.1:1/v1/chat")
            .send()
            .await
            .expect_err("loopback port 1 must refuse the connection");

        let mapped = map_transport_error(error);
        let ClientError::Network(message) = mapped else {
            panic!("connect failure must map to Network, got {mapped:?}");
        };
        assert!(
            message.contains("http://127.0.0.1:1/v1/chat"),
            "queryless URL must stay visible: {message}"
        );
        assert!(!message.contains("[REDACTED]"));
    }
}
