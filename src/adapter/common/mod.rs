//! Shared implementation helpers for the built-in LLM wire adapters.
//!
//! This module is crate-private on purpose: the public adapter API remains the
//! provider-specific `anthropic` and `openai_resp` modules, while common HTTP,
//! SSE, request-building, and JSON bookkeeping code lives in one place.

mod http;
mod json;
mod request;
mod sse;

pub(crate) use http::{
    default_http_client, execute_json_response, execute_sse_response, map_transport_error,
};
pub(crate) use json::insert_preserving_collision;
pub(crate) use request::{endpoint_headers, endpoint_url};
pub(crate) use sse::{SseNormalizer, normalize_sse};
