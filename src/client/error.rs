//! Error types for client-layer operations.

use serde::{Deserialize, Serialize};
use std::time::{Duration, SystemTime};
use thiserror::Error;

/// A provider-neutral failure raised while sending or decoding an LLM request.
///
/// The variants preserve the distinctions needed by retry and fallback policy
/// without exposing a particular HTTP client or provider SDK in the public
/// model.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Error)]
#[serde(rename_all = "snake_case")]
pub enum ClientError {
    /// The provider rejected the request because its rate limit was reached.
    #[error("request was rate limited")]
    RateLimited {
        /// Provider-requested delay before retrying, when `Retry-After` was
        /// present and valid.
        retry_after: Option<Duration>,
    },
    /// The request or upstream provider timed out.
    #[error("request timed out")]
    Timeout,
    /// The request exceeded the model's context window.
    #[error("request exceeded the model context length")]
    ContextLengthExceeded,
    /// The provider's safety system rejected or filtered the content.
    #[error("request or response content was filtered")]
    ContentFiltered,
    /// A connection, DNS, or other network transport operation failed.
    #[error("network error: {0}")]
    Network(String),
    /// A response or stream violated the expected wire protocol.
    #[error("protocol error: {0}")]
    Protocol(String),
    /// The endpoint rejected the configured authentication credentials.
    #[error("authentication failed")]
    Auth,
    /// The endpoint returned an HTTP API error not covered by another class.
    #[error("API returned HTTP {status}: {body}")]
    Api {
        /// HTTP response status code.
        status: u16,
        /// Raw response body retained for diagnostics and provider-specific
        /// inspection.
        body: String,
    },
    /// A client-layer failure that does not fit a more actionable category.
    #[error("client error: {0}")]
    Other(String),
}

impl ClientError {
    /// Classifies an unsuccessful HTTP response without depending on a
    /// particular HTTP client implementation.
    ///
    /// `retry_after` is the raw `Retry-After` response-header value. Both the
    /// delay-seconds and HTTP-date forms from HTTP are supported. Invalid or
    /// absent values still produce [`ClientError::RateLimited`] with no delay.
    pub fn from_http_response(
        status: u16,
        body: impl Into<String>,
        retry_after: Option<&str>,
    ) -> Self {
        Self::from_http_response_at(status, body.into(), retry_after, SystemTime::now())
    }

    /// Performs HTTP error classification relative to a supplied clock value.
    ///
    /// Keeping the clock at this internal boundary makes HTTP-date handling
    /// deterministic in unit tests while the public constructor uses real
    /// wall-clock time.
    fn from_http_response_at(
        status: u16,
        body: String,
        retry_after: Option<&str>,
        now: SystemTime,
    ) -> Self {
        if status == 429 {
            return Self::RateLimited {
                retry_after: retry_after.and_then(|value| parse_retry_after(value, now)),
            };
        }

        if matches!(status, 408 | 504) {
            return Self::Timeout;
        }

        if matches!(status, 401 | 403) {
            return Self::Auth;
        }

        if (400..500).contains(&status) {
            if status == 413 || body_contains_any(&body, CONTEXT_LENGTH_MARKERS) {
                return Self::ContextLengthExceeded;
            }

            if body_contains_any(&body, CONTENT_FILTER_MARKERS) {
                return Self::ContentFiltered;
            }
        }

        Self::Api { status, body }
    }
}

/// Provider error codes and messages commonly used for context-window
/// violations across Anthropic-compatible and OpenAI-compatible endpoints.
const CONTEXT_LENGTH_MARKERS: &[&str] = &[
    "context_length_exceeded",
    "context length exceeded",
    "maximum context length",
    "max context length",
    "context window exceeded",
    "context window is too large",
    "prompt is too long",
    "input is too long",
    "too many tokens",
    "token limit exceeded",
];

/// Provider error codes and messages commonly used for content-policy
/// rejections, including Azure/Foundry-specific spellings.
const CONTENT_FILTER_MARKERS: &[&str] = &[
    "content_filter",
    "content filter",
    "content_filtered",
    "content policy",
    "content_policy_violation",
    "responsibleaipolicyviolation",
    "safety policy",
    "filtered due to",
];

/// Parses an HTTP `Retry-After` value as delay-seconds or an HTTP date.
fn parse_retry_after(value: &str, now: SystemTime) -> Option<Duration> {
    let value = value.trim();
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }

    let retry_at = httpdate::parse_http_date(value).ok()?;
    Some(retry_at.duration_since(now).unwrap_or(Duration::ZERO))
}

/// Performs case-insensitive marker matching while retaining the original body
/// unchanged for the generic API-error fallback.
fn body_contains_any(body: &str, markers: &[&str]) -> bool {
    let normalized = body.to_ascii_lowercase();
    markers.iter().any(|marker| normalized.contains(marker))
}

#[cfg(test)]
mod tests;
