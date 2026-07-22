//! Minimal typed views over OpenAI Chat/Completions streaming chunks.
//!
//! A chat/completions stream is a sequence of chunk objects whose
//! `choices[0].delta` carries incremental message fields. Only the fields the
//! normalizer needs are modeled; everything else (`id`, `object`, `created`,
//! `model`, `system_fingerprint`, the choice's `index`, the tool-call `type`
//! tag, …) is ignored by serde's default unknown-field handling, because there
//! is no `extra` escape hatch for mid-stream chunks (design doc §4.4).
//!
//! The terminal `data: [DONE]` sentinel is not a chunk: the normalizer
//! recognizes it before JSON decoding so it never reaches [`decode`].

use serde::Deserialize;
use serde_json::Value;

/// One decoded chat/completions streaming chunk.
///
/// `choices` is empty on the standalone usage chunk emitted after
/// `include_usage`; otherwise it carries exactly one choice at index 0.
#[derive(Debug, Deserialize)]
pub(super) struct DecodedChunk {
    /// Incremental choices; empty on the terminal usage-only chunk.
    #[serde(default)]
    pub(super) choices: Vec<Choice>,
    /// Terminal token usage, present only on the usage-only chunk emitted after
    /// `include_usage` was requested.
    #[serde(default)]
    pub(super) usage: Option<Value>,
}

/// One streaming choice, carrying a delta and an optional terminal reason.
#[derive(Debug, Deserialize)]
pub(super) struct Choice {
    /// Incremental message fields for this choice.
    pub(super) delta: Delta,
    /// Authoritative stop reason, set only on the final non-empty chunk.
    #[serde(default)]
    pub(super) finish_reason: Option<String>,
}

/// Incremental assistant message fields carried by a chunk delta.
///
/// Every field is optional: the first chunk typically carries `role`, content
/// chunks carry `content` or `reasoning_content`, and tool-call chunks carry
/// `tool_calls` keyed by `index`.
#[derive(Default, Debug, Deserialize)]
pub(super) struct Delta {
    /// Role tag, present on the first chunk of a choice (always `assistant`).
    #[serde(default)]
    pub(super) role: Option<String>,
    /// Assistant-visible text fragment.
    #[serde(default)]
    pub(super) content: Option<String>,
    /// Model reasoning text fragment (DeepSeek / vLLM `reasoning_content`).
    #[serde(default)]
    pub(super) reasoning_content: Option<String>,
    /// Tool-call deltas keyed by `index` (design doc §4.4.2).
    #[serde(default)]
    pub(super) tool_calls: Option<Vec<ToolCallDelta>>,
}

/// One tool-call delta fragment, keyed by `index` within the chunk.
///
/// `id` and `function.name` appear only on the first chunk for an index;
/// subsequent chunks carry further `function.arguments` string fragments.
#[derive(Debug, Deserialize)]
pub(super) struct ToolCallDelta {
    /// Positional key identifying which tool call this fragment belongs to.
    pub(super) index: u64,
    /// Provider tool-call id, present only on the first fragment for an index.
    #[serde(default)]
    pub(super) id: Option<String>,
    /// Function name and incremental argument fragments.
    #[serde(default)]
    pub(super) function: Option<FunctionDelta>,
}

/// Incremental function-call fields within a tool-call delta.
#[derive(Default, Debug, Deserialize)]
pub(super) struct FunctionDelta {
    /// Tool name, present only on the first fragment for an index.
    #[serde(default)]
    pub(super) name: Option<String>,
    /// Raw argument-JSON string fragment; never parsed mid-stream.
    #[serde(default)]
    pub(super) arguments: Option<String>,
}

/// Decodes one chat/completions chunk JSON into a typed view.
///
/// Returns a human-readable error so the caller can wrap it with adapter stream
/// context. The `data: [DONE]` sentinel is handled earlier by the normalizer
/// and never reaches this function.
pub(super) fn decode(data: &str) -> Result<DecodedChunk, String> {
    serde_json::from_str(data).map_err(|error| {
        format!(
            "failed to deserialize chunk JSON at line {}, column {}: {error}",
            error.line(),
            error.column()
        )
    })
}
