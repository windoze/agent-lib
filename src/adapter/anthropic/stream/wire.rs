//! Minimal Anthropic SSE wire types used by the streaming normalizer.

use crate::model::message::Role;
use serde::Deserialize;
use serde_json::{Map, Value};

/// One JSON payload carried by an Anthropic server-sent event.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum WireEvent {
    /// Begins a streamed message and reports its initial usage snapshot.
    MessageStart {
        message: MessageStart,
        #[serde(default, flatten)]
        extra: Map<String, Value>,
    },
    /// Begins one indexed content block.
    ContentBlockStart {
        index: u64,
        content_block: ContentBlockStart,
        #[serde(default, flatten)]
        _extra: Map<String, Value>,
    },
    /// Appends one indexed content-block delta.
    ContentBlockDelta {
        index: u64,
        delta: ContentBlockDelta,
        #[serde(default, flatten)]
        _extra: Map<String, Value>,
    },
    /// Closes one indexed content block.
    ContentBlockStop {
        index: u64,
        #[serde(default, flatten)]
        _extra: Map<String, Value>,
    },
    /// Reports final message metadata and a cumulative usage snapshot.
    MessageDelta {
        delta: MessageDelta,
        usage: StreamUsage,
        #[serde(default, flatten)]
        extra: Map<String, Value>,
    },
    /// Closes the streamed message.
    MessageStop {
        #[serde(default, flatten)]
        extra: Map<String, Value>,
    },
    /// Keeps an otherwise idle stream connection alive.
    Ping {
        #[serde(default, flatten)]
        _extra: Map<String, Value>,
    },
    /// Reports a provider failure after HTTP headers were sent.
    Error {
        error: ErrorPayload,
        #[serde(default, flatten)]
        _extra: Map<String, Value>,
    },
}

impl WireEvent {
    /// Returns the `type` value expected in the SSE `event` field.
    pub(super) fn wire_type(&self) -> &'static str {
        match self {
            Self::MessageStart { .. } => "message_start",
            Self::ContentBlockStart { .. } => "content_block_start",
            Self::ContentBlockDelta { .. } => "content_block_delta",
            Self::ContentBlockStop { .. } => "content_block_stop",
            Self::MessageDelta { .. } => "message_delta",
            Self::MessageStop { .. } => "message_stop",
            Self::Ping { .. } => "ping",
            Self::Error { .. } => "error",
        }
    }
}

/// Message fields needed at the start of an Anthropic stream.
#[derive(Debug, Deserialize)]
pub(super) struct MessageStart {
    /// Provider-reported response role.
    pub(super) role: Role,
    /// Initial cumulative usage snapshot.
    pub(super) usage: StreamUsage,
    /// Response identifiers, model names, and other provider metadata.
    #[serde(default, flatten)]
    pub(super) extra: Map<String, Value>,
}

/// Content-block metadata supplied by `content_block_start`.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum ContentBlockStart {
    /// Assistant-visible text, normally empty at block start.
    Text {
        text: String,
        #[serde(default, flatten)]
        _extra: Map<String, Value>,
    },
    /// Extended thinking, normally empty at block start.
    Thinking {
        thinking: String,
        #[serde(default)]
        signature: Option<String>,
        #[serde(default, flatten)]
        _extra: Map<String, Value>,
    },
    /// Tool identity plus the provider's initial input placeholder.
    ToolUse {
        id: String,
        name: String,
        input: Value,
        #[serde(default, flatten)]
        _extra: Map<String, Value>,
    },
}

/// Incremental content carried by `content_block_delta`.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum ContentBlockDelta {
    /// Assistant-visible text fragment.
    #[serde(rename = "text_delta")]
    Text { text: String },
    /// Raw, potentially incomplete tool-input JSON fragment.
    #[serde(rename = "input_json_delta")]
    InputJson { partial_json: String },
    /// Extended-thinking text fragment.
    #[serde(rename = "thinking_delta")]
    Thinking { thinking: String },
    /// Opaque replay-signature fragment for extended thinking.
    #[serde(rename = "signature_delta")]
    Signature { signature: String },
}

/// Final message metadata supplied before `message_stop`.
#[derive(Debug, Deserialize)]
pub(super) struct MessageDelta {
    /// Provider stop reason; Anthropic normally supplies it once here.
    #[serde(default)]
    pub(super) stop_reason: Option<String>,
    /// Stop sequence and future message-level delta fields.
    #[serde(default, flatten)]
    pub(super) extra: Map<String, Value>,
}

/// Anthropic usage counters are cumulative snapshots, not additive chunks.
#[derive(Debug, Deserialize)]
pub(super) struct StreamUsage {
    #[serde(default)]
    pub(super) input_tokens: Option<u32>,
    #[serde(default)]
    pub(super) output_tokens: Option<u32>,
    #[serde(default)]
    pub(super) cache_creation_input_tokens: Option<u32>,
    #[serde(default)]
    pub(super) cache_read_input_tokens: Option<u32>,
    /// Cache-duration detail and future provider usage fields.
    #[serde(default, flatten)]
    pub(super) extra: Map<String, Value>,
}

/// Provider error details embedded in a successful HTTP SSE response.
#[derive(Debug, Deserialize)]
pub(super) struct ErrorPayload {
    /// Anthropic error discriminator such as `overloaded_error`.
    #[serde(rename = "type")]
    pub(super) kind: String,
    /// Provider message and future error fields are retained in the raw event
    /// passed to classification.
    #[serde(default, flatten)]
    _extra: Map<String, Value>,
}
