//! Classification and serialization tests for client errors.

use super::ClientError;
use serde_json::json;
use std::time::{Duration, SystemTime};

/// Asserts that an error retains its category and payload through serde.
fn assert_round_trip(error: ClientError) {
    let encoded = serde_json::to_value(&error).expect("serialize client error");
    let decoded: ClientError = serde_json::from_value(encoded).expect("deserialize client error");

    assert_eq!(decoded, error);
}

#[test]
fn every_error_variant_round_trips_through_serde() {
    for error in [
        ClientError::RateLimited {
            retry_after: Some(Duration::from_secs(12)),
        },
        ClientError::Timeout,
        ClientError::ContextLengthExceeded,
        ClientError::ContentFiltered,
        ClientError::Network("connection reset".to_owned()),
        ClientError::Protocol("invalid SSE frame".to_owned()),
        ClientError::Auth,
        ClientError::Api {
            status: 404,
            body: "not found".to_owned(),
        },
        ClientError::Other("worker stopped".to_owned()),
    ] {
        assert_round_trip(error);
    }
}

#[test]
fn rate_limit_parses_delay_seconds() {
    let error = ClientError::from_http_response(429, "too many requests", Some(" 17 "));

    assert_eq!(
        error,
        ClientError::RateLimited {
            retry_after: Some(Duration::from_secs(17)),
        }
    );
}

#[test]
fn rate_limit_parses_http_date_relative_to_response_time() {
    let retry_at = httpdate::parse_http_date("Sun, 06 Nov 1994 08:49:37 GMT").unwrap();
    let now = retry_at.checked_sub(Duration::from_secs(37)).unwrap();
    let error = ClientError::from_http_response_at(
        429,
        "too many requests".to_owned(),
        Some("Sun, 06 Nov 1994 08:49:37 GMT"),
        now,
    );

    assert_eq!(
        error,
        ClientError::RateLimited {
            retry_after: Some(Duration::from_secs(37)),
        }
    );
}

#[test]
fn expired_retry_date_becomes_an_immediate_retry_delay() {
    let error = ClientError::from_http_response_at(
        429,
        String::new(),
        Some("Sun, 06 Nov 1994 08:49:37 GMT"),
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000),
    );

    assert_eq!(
        error,
        ClientError::RateLimited {
            retry_after: Some(Duration::ZERO),
        }
    );
}

#[test]
fn missing_or_invalid_retry_after_is_retained_as_unknown() {
    for retry_after in [None, Some("later"), Some("18446744073709551616")] {
        assert_eq!(
            ClientError::from_http_response(429, String::new(), retry_after),
            ClientError::RateLimited { retry_after: None }
        );
    }
}

#[test]
fn timeout_statuses_are_classified() {
    for status in [408, 504] {
        assert_eq!(
            ClientError::from_http_response(status, "upstream timeout", None),
            ClientError::Timeout
        );
    }
}

#[test]
fn context_length_status_and_provider_bodies_are_classified() {
    let openai_body = json!({
        "error": {
            "message": "This model's maximum context length is 128000 tokens.",
            "type": "invalid_request_error",
            "code": "context_length_exceeded"
        }
    })
    .to_string();

    assert_eq!(
        ClientError::from_http_response(400, openai_body, None),
        ClientError::ContextLengthExceeded
    );
    assert_eq!(
        ClientError::from_http_response(413, "request entity too large", None),
        ClientError::ContextLengthExceeded
    );
    assert_eq!(
        ClientError::from_http_response(422, "Prompt is too long for this model", None),
        ClientError::ContextLengthExceeded
    );
}

#[test]
fn content_filter_bodies_are_classified_before_generic_forbidden_status() {
    let foundry_body = json!({
        "error": {
            "code": "content_filter",
            "message": "The response was filtered due to the prompt triggering a policy.",
            "inner_error": {
                "code": "ResponsibleAIPolicyViolation"
            }
        }
    })
    .to_string();

    assert_eq!(
        ClientError::from_http_response(403, foundry_body, None),
        ClientError::ContentFiltered
    );
    assert_eq!(
        ClientError::from_http_response(400, "Rejected by CONTENT POLICY", None),
        ClientError::ContentFiltered
    );
}

#[test]
fn authentication_statuses_are_classified_when_no_policy_error_is_present() {
    for status in [401, 403] {
        assert_eq!(
            ClientError::from_http_response(status, "invalid API credential", None),
            ClientError::Auth
        );
    }
}

#[test]
fn unknown_http_errors_preserve_status_and_raw_body() {
    for (status, body) in [
        (404, r#"{"error":"deployment not found"}"#),
        (500, "upstream unavailable"),
    ] {
        assert_eq!(
            ClientError::from_http_response(status, body, None),
            ClientError::Api {
                status,
                body: body.to_owned(),
            }
        );
    }
}
