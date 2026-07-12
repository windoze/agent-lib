//! Minimal typed views over OpenAI Responses streaming payloads.

use serde::Deserialize;
use serde_json::Value;

/// One decoded Responses event plus the original JSON for diagnostics and
/// escape-hatch retention.
#[derive(Debug)]
pub(super) struct DecodedEvent {
    /// Event kind copied from the payload's `type` discriminator.
    pub(super) kind: String,
    /// Typed fields needed by the normalized state machine.
    pub(super) event: WireEvent,
    /// Unmodified event JSON.
    pub(super) raw: Value,
}

/// Responses event variants that affect normalized message state.
#[derive(Debug)]
pub(super) enum WireEvent {
    /// Creates the streamed response and its assistant message.
    ResponseCreated(ResponseEvent),
    /// Confirms that response generation is in progress.
    ResponseInProgress(ResponseEvent),
    /// Completes a response normally.
    ResponseCompleted(ResponseEvent),
    /// Completes a response without satisfying the requested output limit.
    ResponseIncomplete(ResponseEvent),
    /// Terminates generation with a provider failure object.
    ResponseFailed(ResponseEvent),
    /// Adds one top-level output item.
    OutputItemAdded(OutputItemEvent),
    /// Completes one top-level output item.
    OutputItemDone(OutputItemEvent),
    /// Adds one content part to an output-message item.
    ContentPartAdded(ContentPartEvent),
    /// Completes one content part of an output-message item.
    ContentPartDone(ContentPartEvent),
    /// Appends assistant-visible output text.
    OutputTextDelta(DeltaEvent),
    /// Publishes the authoritative complete assistant-visible text.
    OutputTextDone(TextDoneEvent),
    /// Appends a refusal string.
    RefusalDelta(DeltaEvent),
    /// Publishes the authoritative complete refusal string.
    RefusalDone(RefusalDoneEvent),
    /// Appends raw function-call argument JSON.
    FunctionArgumentsDelta(DeltaEvent),
    /// Publishes authoritative complete function-call argument JSON.
    FunctionArgumentsDone(ArgumentsDoneEvent),
    /// Appends raw reasoning content.
    ReasoningTextDelta(DeltaEvent),
    /// Publishes authoritative complete raw reasoning content.
    ReasoningTextDone(TextDoneEvent),
    /// Appends a provider-visible reasoning summary.
    ReasoningSummaryTextDelta(DeltaEvent),
    /// Publishes authoritative complete reasoning-summary text.
    ReasoningSummaryTextDone(TextDoneEvent),
    /// Reports a provider error outside the response object lifecycle.
    Error(SequencedEvent),
    /// Retains a future or unsupported Responses event without guessing at a
    /// normalized meaning.
    Unknown(SequencedEvent),
}

impl WireEvent {
    /// Returns the provider sequence number shared by every Responses event.
    pub(super) fn sequence_number(&self) -> u64 {
        match self {
            Self::ResponseCreated(event)
            | Self::ResponseInProgress(event)
            | Self::ResponseCompleted(event)
            | Self::ResponseIncomplete(event)
            | Self::ResponseFailed(event) => event.sequence_number,
            Self::OutputItemAdded(event) | Self::OutputItemDone(event) => event.sequence_number,
            Self::ContentPartAdded(event) | Self::ContentPartDone(event) => event.sequence_number,
            Self::OutputTextDelta(event)
            | Self::RefusalDelta(event)
            | Self::FunctionArgumentsDelta(event)
            | Self::ReasoningTextDelta(event)
            | Self::ReasoningSummaryTextDelta(event) => event.sequence_number,
            Self::OutputTextDone(event)
            | Self::ReasoningTextDone(event)
            | Self::ReasoningSummaryTextDone(event) => event.sequence_number,
            Self::RefusalDone(event) => event.sequence_number,
            Self::FunctionArgumentsDone(event) => event.sequence_number,
            Self::Error(event) | Self::Unknown(event) => event.sequence_number,
        }
    }
}

/// Event carrying a complete response snapshot.
#[derive(Debug, Deserialize)]
pub(super) struct ResponseEvent {
    /// Complete or in-progress Responses object.
    pub(super) response: Value,
    /// Monotonic event position within this stream.
    pub(super) sequence_number: u64,
}

/// Event carrying one complete or placeholder output item.
#[derive(Debug, Deserialize)]
pub(super) struct OutputItemEvent {
    /// Position of the item in the final `response.output` array.
    pub(super) output_index: u64,
    /// Provider output item object.
    pub(super) item: Value,
    /// Monotonic event position within this stream.
    pub(super) sequence_number: u64,
}

/// Event carrying one complete or placeholder message content part.
#[derive(Debug, Deserialize)]
pub(super) struct ContentPartEvent {
    /// Provider id of the parent output-message item.
    pub(super) item_id: String,
    /// Position of the parent item in `response.output`.
    pub(super) output_index: u64,
    /// Position of the part in the parent message's `content` array.
    pub(super) content_index: u64,
    /// Provider content-part object.
    pub(super) part: Value,
    /// Monotonic event position within this stream.
    pub(super) sequence_number: u64,
}

/// Shared shape of text, refusal, reasoning, and argument delta events.
#[derive(Debug, Deserialize)]
pub(super) struct DeltaEvent {
    /// Provider id of the output item receiving this delta.
    pub(super) item_id: String,
    /// Position of the output item in `response.output`.
    pub(super) output_index: u64,
    /// Message or raw-reasoning content index, when applicable.
    #[serde(default)]
    pub(super) content_index: Option<u64>,
    /// Reasoning-summary part index, when applicable.
    #[serde(default)]
    pub(super) summary_index: Option<u64>,
    /// Incremental string supplied by the provider.
    pub(super) delta: String,
    /// Monotonic event position within this stream.
    pub(super) sequence_number: u64,
}

/// Completion event carrying one authoritative text value.
#[derive(Debug, Deserialize)]
pub(super) struct TextDoneEvent {
    /// Provider id of the output item receiving this text.
    pub(super) item_id: String,
    /// Position of the output item in `response.output`.
    pub(super) output_index: u64,
    /// Message or raw-reasoning content index, when applicable.
    #[serde(default)]
    pub(super) content_index: Option<u64>,
    /// Reasoning-summary part index, when applicable.
    #[serde(default)]
    pub(super) summary_index: Option<u64>,
    /// Complete text value supplied by the provider.
    pub(super) text: String,
    /// Monotonic event position within this stream.
    pub(super) sequence_number: u64,
}

/// Completion event carrying one authoritative refusal value.
#[derive(Debug, Deserialize)]
pub(super) struct RefusalDoneEvent {
    /// Provider id of the output-message item.
    pub(super) item_id: String,
    /// Position of the output item in `response.output`.
    pub(super) output_index: u64,
    /// Position of the refusal part in the message.
    pub(super) content_index: u64,
    /// Complete refusal string supplied by the provider.
    pub(super) refusal: String,
    /// Monotonic event position within this stream.
    pub(super) sequence_number: u64,
}

/// Completion event carrying authoritative function-call arguments.
#[derive(Debug, Deserialize)]
pub(super) struct ArgumentsDoneEvent {
    /// Provider id of the function-call output item.
    pub(super) item_id: String,
    /// Position of the output item in `response.output`.
    pub(super) output_index: u64,
    /// Complete JSON argument text supplied by the provider.
    pub(super) arguments: String,
    /// Monotonic event position within this stream.
    pub(super) sequence_number: u64,
}

/// Minimal shape shared by error and future event kinds.
#[derive(Debug, Deserialize)]
pub(super) struct SequencedEvent {
    /// Monotonic event position within this stream.
    pub(super) sequence_number: u64,
}

/// Deserializes a JSON payload according to its `type` discriminator.
pub(super) fn decode(data: &str) -> Result<DecodedEvent, String> {
    let raw: Value = serde_json::from_str(data).map_err(|error| {
        format!(
            "failed to deserialize event JSON at line {}, column {}: {error}",
            error.line(),
            error.column()
        )
    })?;
    let kind = raw
        .as_object()
        .ok_or_else(|| "event JSON must be an object".to_owned())?
        .get("type")
        .ok_or_else(|| "event JSON field `type` is required".to_owned())?
        .as_str()
        .ok_or_else(|| "event JSON field `type` must be a string".to_owned())?
        .to_owned();

    let event = match kind.as_str() {
        "response.created" => WireEvent::ResponseCreated(from_raw(&raw, &kind)?),
        "response.in_progress" => WireEvent::ResponseInProgress(from_raw(&raw, &kind)?),
        "response.completed" => WireEvent::ResponseCompleted(from_raw(&raw, &kind)?),
        "response.incomplete" => WireEvent::ResponseIncomplete(from_raw(&raw, &kind)?),
        "response.failed" => WireEvent::ResponseFailed(from_raw(&raw, &kind)?),
        "response.output_item.added" => WireEvent::OutputItemAdded(from_raw(&raw, &kind)?),
        "response.output_item.done" => WireEvent::OutputItemDone(from_raw(&raw, &kind)?),
        "response.content_part.added" => WireEvent::ContentPartAdded(from_raw(&raw, &kind)?),
        "response.content_part.done" => WireEvent::ContentPartDone(from_raw(&raw, &kind)?),
        "response.output_text.delta" => WireEvent::OutputTextDelta(from_raw(&raw, &kind)?),
        "response.output_text.done" => WireEvent::OutputTextDone(from_raw(&raw, &kind)?),
        "response.refusal.delta" => WireEvent::RefusalDelta(from_raw(&raw, &kind)?),
        "response.refusal.done" => WireEvent::RefusalDone(from_raw(&raw, &kind)?),
        "response.function_call_arguments.delta" => {
            WireEvent::FunctionArgumentsDelta(from_raw(&raw, &kind)?)
        }
        "response.function_call_arguments.done" => {
            WireEvent::FunctionArgumentsDone(from_raw(&raw, &kind)?)
        }
        "response.reasoning_text.delta" => WireEvent::ReasoningTextDelta(from_raw(&raw, &kind)?),
        "response.reasoning_text.done" => WireEvent::ReasoningTextDone(from_raw(&raw, &kind)?),
        "response.reasoning_summary_text.delta" => {
            WireEvent::ReasoningSummaryTextDelta(from_raw(&raw, &kind)?)
        }
        "response.reasoning_summary_text.done" => {
            WireEvent::ReasoningSummaryTextDone(from_raw(&raw, &kind)?)
        }
        "error" => WireEvent::Error(from_raw(&raw, &kind)?),
        _ => WireEvent::Unknown(from_raw(&raw, &kind)?),
    };

    Ok(DecodedEvent { kind, event, raw })
}

/// Deserializes one typed view while retaining the original payload outside
/// that view.
fn from_raw<T>(raw: &Value, kind: &str) -> Result<T, String>
where
    T: for<'de> Deserialize<'de>,
{
    serde_json::from_value(raw.clone())
        .map_err(|error| format!("invalid `{kind}` event payload: {error}"))
}
