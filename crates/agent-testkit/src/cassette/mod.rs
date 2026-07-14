//! Cassette schema, request fingerprint, and redactor for provider-neutral
//! effect req/resp.
//!
//! A *cassette* records the request/result of real agent effects
//! ([`RequirementResult`](agent_lib::agent::RequirementResult) families) so a CI
//! run can replay them offline. It is a provider-neutral fixture: it stores the
//! effect-boundary [`ChatRequest`]/[`Response`], [`ToolCall`]/[`ToolResponse`],
//! [`Interaction`]/[`InteractionResponse`], and reconfig payloads the machine
//! exchanges with its handlers — never HTTP headers, auth, endpoints, or a
//! provider's raw wire body.
//!
//! This module defines the on-disk **schema**, the request **fingerprint**, and
//! the **redactor** (milestone M3-1), plus the offline **replay handlers**
//! (milestone M3-2; see the [Replay](#replay) section). The record/verify/update
//! wrappers land in M3-3.
//!
//! # Schema shape
//!
//! - [`Cassette`] carries a [`schema version`](CASSETTE_SCHEMA_VERSION),
//!   [`CassetteMetadata`], an ordered list of [`CassetteEntry`] values, and
//!   optional [`CassetteObservations`].
//! - Each [`CassetteEntry`] is one fulfilled effect: a family tag, the call's
//!   dispatch index, the normalized request, its [`request fingerprint`](request_fingerprint),
//!   the normalized result, and an optional review summary.
//! - Loading through [`Cassette::from_json_str`] classifies an unknown
//!   [`schema version`](CASSETTE_SCHEMA_VERSION) as a
//!   [`CassetteError::UnsupportedSchemaVersion`] instead of a vague parse error.
//!
//! # Replay
//!
//! The replay handlers ([`CassetteLlmHandler`], [`CassetteToolHandler`],
//! [`CassetteInteractionHandler`], [`CassetteReconfigHandler`], built cohesively
//! through a [`CassettePlayer`]) turn a recorded [`Cassette`] back into the four
//! effect handler traits. Each returns recorded results in family + dispatch
//! order, matching every request by its [`request fingerprint`](request_fingerprint)
//! and surfacing a clear [`ReplayMismatch`] when a live request diverges.

mod replay;

pub use replay::{
    CassetteInteractionHandler, CassetteLlmHandler, CassettePlayer, CassetteReconfigHandler,
    CassetteToolHandler, ReplayMismatch, ReplayMismatchKind,
};

use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use agent_lib::agent::{
    Interaction, InteractionResponse, LlmStepMode, ToolRuntimeError, ToolSetId, ToolSetRef,
};
use agent_lib::client::{ChatRequest, ClientError, Response};
use agent_lib::model::tool::{ToolCall, ToolResponse};

/// Current cassette schema version.
///
/// A cassette loaded with a different version is rejected by
/// [`Cassette::from_json_str`] as a [`CassetteError::UnsupportedSchemaVersion`]
/// so a stale fixture fails loudly rather than deserializing into a subtly wrong
/// shape.
pub const CASSETTE_SCHEMA_VERSION: u32 = 1;

/// Canonical token substituted for a volatile id while computing a
/// [`request fingerprint`](request_fingerprint).
pub const VOLATILE_ID_PLACEHOLDER: &str = "<volatile-id>";

/// Value written by [`DefaultRedactor`] in place of an un-allowlisted field.
pub const REDACTED_PLACEHOLDER: &str = "<redacted>";

/// Object keys whose string value is a volatile, per-run identity and is
/// normalized out of a [`request fingerprint`](request_fingerprint).
///
/// These cover the host/provider-assigned ids the effect boundary carries —
/// requirement ids, trace node ids, message/step ids, and provider tool-call
/// ids — none of which should make two otherwise-identical logical requests
/// fingerprint differently.
const VOLATILE_ID_KEYS: &[&str] = &[
    "id",
    "tool_call_id",
    "tool_use_id",
    "call_id",
    "step_id",
    "message_id",
    "requirement_id",
    "trace_node_id",
];

/// Object keys whose value is opaque model/tool payload: its keys are still
/// canonically ordered, but volatile-id normalization does **not** descend into
/// it, so a semantically meaningful `"id"` inside tool input is preserved.
const OPAQUE_SUBTREE_KEYS: &[&str] = &["input", "input_schema"];

/// A recorded sequence of provider-neutral effect req/resp for offline replay.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Cassette {
    /// Schema version this cassette was written with.
    pub schema_version: u32,
    /// Descriptive, non-secret metadata about the recording.
    pub metadata: CassetteMetadata,
    /// Ordered effect entries, one per fulfilled requirement.
    #[serde(default)]
    pub entries: Vec<CassetteEntry>,
    /// Optional terminal observations captured alongside the entries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observations: Option<CassetteObservations>,
}

impl Cassette {
    /// Creates an empty cassette stamped with the current schema version.
    #[must_use]
    pub fn new(metadata: CassetteMetadata) -> Self {
        Self {
            schema_version: CASSETTE_SCHEMA_VERSION,
            metadata,
            entries: Vec::new(),
            observations: None,
        }
    }

    /// Appends one effect entry and returns `self` for chaining.
    #[must_use]
    pub fn with_entry(mut self, entry: impl Into<CassetteEntry>) -> Self {
        self.entries.push(entry.into());
        self
    }

    /// Appends one effect entry in place.
    pub fn push(&mut self, entry: impl Into<CassetteEntry>) {
        self.entries.push(entry.into());
    }

    /// Serializes this cassette to a compact JSON string.
    ///
    /// # Errors
    ///
    /// Returns [`CassetteError::Serialize`] when serialization fails.
    pub fn to_json_string(&self) -> Result<String, CassetteError> {
        serde_json::to_string(self).map_err(CassetteError::Serialize)
    }

    /// Serializes this cassette to a pretty-printed JSON string.
    ///
    /// # Errors
    ///
    /// Returns [`CassetteError::Serialize`] when serialization fails.
    pub fn to_json_string_pretty(&self) -> Result<String, CassetteError> {
        serde_json::to_string_pretty(self).map_err(CassetteError::Serialize)
    }

    /// Parses a cassette from JSON, classifying an unknown schema version.
    ///
    /// The `schema_version` field is read and validated *before* the rest of the
    /// document, so a version mismatch surfaces as a
    /// [`CassetteError::UnsupportedSchemaVersion`] rather than a downstream
    /// shape error.
    ///
    /// # Errors
    ///
    /// Returns [`CassetteError::Deserialize`] when the text is not valid JSON,
    /// [`CassetteError::MissingSchemaVersion`] when the `schema_version` field
    /// is absent or not a number, or
    /// [`CassetteError::UnsupportedSchemaVersion`] when it names a version this
    /// build does not support.
    pub fn from_json_str(json: &str) -> Result<Self, CassetteError> {
        let value: Value = serde_json::from_str(json).map_err(CassetteError::Deserialize)?;
        match value.get("schema_version").and_then(Value::as_u64) {
            Some(version) if version == u64::from(CASSETTE_SCHEMA_VERSION) => {}
            Some(version) => {
                return Err(CassetteError::UnsupportedSchemaVersion {
                    found: Some(version),
                    supported: CASSETTE_SCHEMA_VERSION,
                });
            }
            None => return Err(CassetteError::MissingSchemaVersion),
        }
        serde_json::from_value(value).map_err(CassetteError::Deserialize)
    }
}

/// Descriptive, non-secret metadata about a recording.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CassetteMetadata {
    /// Name of the test that owns this cassette.
    pub test_name: String,
    /// Optional free-form description of the recorded scenario.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Optional `agent-lib` version the cassette was recorded against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crate_version: Option<String>,
}

impl CassetteMetadata {
    /// Creates metadata naming the owning test.
    #[must_use]
    pub fn new(test_name: impl Into<String>) -> Self {
        Self {
            test_name: test_name.into(),
            description: None,
            crate_version: None,
        }
    }

    /// Sets the scenario description and returns `self` for chaining.
    #[must_use]
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Sets the recorded `agent-lib` version and returns `self` for chaining.
    #[must_use]
    pub fn with_crate_version(mut self, crate_version: impl Into<String>) -> Self {
        self.crate_version = Some(crate_version.into());
        self
    }
}

/// Optional terminal observations captured alongside a cassette's entries.
///
/// These are review aids: short, human-readable summaries of the turn's final
/// disposition. They are intentionally string-shaped so the schema stays stable
/// while richer structured observations are added later.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CassetteObservations {
    /// Summary of the machine's final cursor after the replayed turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_cursor: Option<String>,
    /// Summary of notifications emitted during the replayed turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notifications_summary: Option<String>,
    /// Summary of the committed conversation after the replayed turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation_summary: Option<String>,
    /// Trace requirement dispositions observed during the replayed turn.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trace_dispositions: Vec<String>,
}

/// One fulfilled effect recorded in a cassette.
///
/// The `family` tag selects the variant, and each variant carries the normalized
/// request, its fingerprint, and the normalized result for that effect family.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "family", rename_all = "snake_case")]
pub enum CassetteEntry {
    /// One recorded LLM generation.
    Llm(LlmEntry),
    /// One recorded tool execution.
    Tool(ToolEntry),
    /// One recorded interaction with the "user".
    Interaction(InteractionEntry),
    /// One recorded tool-set reconfiguration.
    Reconfig(ReconfigEntry),
}

impl CassetteEntry {
    /// Returns the zero-based dispatch index this entry was recorded at.
    #[must_use]
    pub const fn index(&self) -> usize {
        match self {
            Self::Llm(entry) => entry.index,
            Self::Tool(entry) => entry.index,
            Self::Interaction(entry) => entry.index,
            Self::Reconfig(entry) => entry.index,
        }
    }

    /// Returns the recorded request fingerprint for this entry.
    #[must_use]
    pub fn fingerprint(&self) -> &str {
        match self {
            Self::Llm(entry) => &entry.fingerprint,
            Self::Tool(entry) => &entry.fingerprint,
            Self::Interaction(entry) => &entry.fingerprint,
            Self::Reconfig(entry) => &entry.fingerprint,
        }
    }
}

impl From<LlmEntry> for CassetteEntry {
    fn from(entry: LlmEntry) -> Self {
        Self::Llm(entry)
    }
}

impl From<ToolEntry> for CassetteEntry {
    fn from(entry: ToolEntry) -> Self {
        Self::Tool(entry)
    }
}

impl From<InteractionEntry> for CassetteEntry {
    fn from(entry: InteractionEntry) -> Self {
        Self::Interaction(entry)
    }
}

impl From<ReconfigEntry> for CassetteEntry {
    fn from(entry: ReconfigEntry) -> Self {
        Self::Reconfig(entry)
    }
}

/// A recorded LLM generation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LlmEntry {
    /// Zero-based dispatch index of the generation.
    pub index: usize,
    /// Normalized request sent to the model.
    pub request: ChatRequest,
    /// Transport mode the generation was rendered for.
    pub mode: LlmStepMode,
    /// Fingerprint of `request`, ignoring volatile ids.
    pub fingerprint: String,
    /// Recorded result of the generation.
    pub result: LlmOutcome,
    /// Optional human-readable review summary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

impl LlmEntry {
    /// Records a generation, computing the request fingerprint.
    #[must_use]
    pub fn new(index: usize, request: ChatRequest, mode: LlmStepMode, result: LlmOutcome) -> Self {
        let fingerprint = request_fingerprint(&request);
        Self {
            index,
            request,
            mode,
            fingerprint,
            result,
            summary: None,
        }
    }
}

/// A recorded tool execution.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolEntry {
    /// Zero-based dispatch index of the tool call.
    pub index: usize,
    /// Normalized tool call selected by the model.
    pub request: ToolCall,
    /// Fingerprint of `request`, ignoring volatile ids.
    pub fingerprint: String,
    /// Recorded result of the tool call.
    pub result: ToolOutcome,
    /// Optional human-readable review summary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

impl ToolEntry {
    /// Records a tool execution, computing the request fingerprint.
    #[must_use]
    pub fn new(index: usize, request: ToolCall, result: ToolOutcome) -> Self {
        let fingerprint = request_fingerprint(&request);
        Self {
            index,
            request,
            fingerprint,
            result,
            summary: None,
        }
    }
}

/// A recorded interaction with the "user".
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InteractionEntry {
    /// Zero-based dispatch index of the interaction.
    pub index: usize,
    /// Normalized interaction presented externally.
    pub request: Interaction,
    /// Fingerprint of `request`, ignoring volatile ids.
    pub fingerprint: String,
    /// Recorded resolution of the interaction.
    pub result: InteractionResponse,
    /// Optional human-readable review summary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

impl InteractionEntry {
    /// Records an interaction, computing the request fingerprint.
    #[must_use]
    pub fn new(index: usize, request: Interaction, result: InteractionResponse) -> Self {
        let fingerprint = request_fingerprint(&request);
        Self {
            index,
            request,
            fingerprint,
            result,
            summary: None,
        }
    }
}

/// A recorded tool-set reconfiguration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReconfigEntry {
    /// Zero-based dispatch index of the reconfiguration.
    pub index: usize,
    /// Normalized tool set whose live registry was resolved.
    pub request: ToolSetRef,
    /// Fingerprint of `request`, ignoring volatile ids.
    pub fingerprint: String,
    /// Recorded result of the reconfiguration.
    pub result: ReconfigOutcome,
    /// Optional human-readable review summary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

impl ReconfigEntry {
    /// Records a reconfiguration, computing the request fingerprint.
    #[must_use]
    pub fn new(index: usize, request: ToolSetRef, result: ReconfigOutcome) -> Self {
        let fingerprint = request_fingerprint(&request);
        Self {
            index,
            request,
            fingerprint,
            result,
            summary: None,
        }
    }
}

/// Recorded outcome of an LLM generation.
///
/// Mirrors the [`RequirementResult::Llm`](agent_lib::agent::RequirementResult::Llm)
/// family with serializable halves.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmOutcome {
    /// A successful generation.
    Ok(Response),
    /// A client-layer generation failure.
    Err(ClientError),
}

/// Recorded outcome of a tool execution.
///
/// Mirrors the [`RequirementResult::Tool`](agent_lib::agent::RequirementResult::Tool)
/// family; the error half uses [`CassetteToolError`] because
/// [`ToolRuntimeError`] is not itself serializable.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolOutcome {
    /// A completed tool call (including model-visible tool errors).
    Ok(ToolResponse),
    /// A runtime failure raised before a complete `ToolResponse`.
    Err(CassetteToolError),
}

/// Recorded outcome of a tool-set reconfiguration.
///
/// Mirrors the [`RequirementResult::Reconfig`](agent_lib::agent::RequirementResult::Reconfig)
/// family.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReconfigOutcome {
    /// The driver swapped in a validated registry for the requested tool set.
    Ok,
    /// A resolution or declaration-mismatch failure.
    Err(CassetteToolError),
}

/// Serializable mirror of [`ToolRuntimeError`].
///
/// [`ToolRuntimeError`] is intentionally a non-persistable runtime error, so the
/// cassette records this structurally identical, serializable twin instead.
/// Lossless [`From`] conversions bridge the two so replay handlers (M3-2) can
/// reconstruct the live error.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CassetteToolError {
    /// The registry has no executable tool with this name.
    UnknownTool {
        /// Tool name selected by the model.
        name: String,
    },
    /// The runtime has no registry for a requested tool-set identity.
    UnknownToolSet {
        /// Tool-set identity selected by a reconfiguration request.
        id: ToolSetId,
    },
    /// The host did not provide a stable identity required by the loop.
    IdUnavailable {
        /// Stable description of the missing identity.
        purpose: String,
    },
    /// The executor failed before returning a complete `ToolResponse`.
    ExecutionFailed {
        /// Tool name selected by the model.
        tool_name: String,
        /// Stable diagnostic text.
        message: String,
    },
    /// The registry itself rejected construction or lookup data.
    InvalidRegistry {
        /// Stable diagnostic text.
        message: String,
    },
}

impl From<&ToolRuntimeError> for CassetteToolError {
    fn from(error: &ToolRuntimeError) -> Self {
        match error {
            ToolRuntimeError::UnknownTool { name } => Self::UnknownTool { name: name.clone() },
            ToolRuntimeError::UnknownToolSet { id } => Self::UnknownToolSet { id: *id },
            ToolRuntimeError::IdUnavailable { purpose } => Self::IdUnavailable {
                purpose: purpose.clone(),
            },
            ToolRuntimeError::ExecutionFailed { tool_name, message } => Self::ExecutionFailed {
                tool_name: tool_name.clone(),
                message: message.clone(),
            },
            ToolRuntimeError::InvalidRegistry { message } => Self::InvalidRegistry {
                message: message.clone(),
            },
        }
    }
}

impl From<ToolRuntimeError> for CassetteToolError {
    fn from(error: ToolRuntimeError) -> Self {
        Self::from(&error)
    }
}

impl From<CassetteToolError> for ToolRuntimeError {
    fn from(error: CassetteToolError) -> Self {
        match error {
            CassetteToolError::UnknownTool { name } => Self::UnknownTool { name },
            CassetteToolError::UnknownToolSet { id } => Self::UnknownToolSet { id },
            CassetteToolError::IdUnavailable { purpose } => Self::IdUnavailable { purpose },
            CassetteToolError::ExecutionFailed { tool_name, message } => {
                Self::ExecutionFailed { tool_name, message }
            }
            CassetteToolError::InvalidRegistry { message } => Self::InvalidRegistry { message },
        }
    }
}

impl fmt::Display for CassetteToolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&ToolRuntimeError::from(self.clone()), formatter)
    }
}

impl std::error::Error for CassetteToolError {}

/// A classified failure raised while (de)serializing a [`Cassette`].
#[derive(Debug)]
pub enum CassetteError {
    /// Serialization to JSON failed.
    Serialize(serde_json::Error),
    /// Deserialization from JSON failed.
    Deserialize(serde_json::Error),
    /// The document had no numeric `schema_version` field.
    MissingSchemaVersion,
    /// The document named a schema version this build does not support.
    UnsupportedSchemaVersion {
        /// The version found in the document, if it was numeric.
        found: Option<u64>,
        /// The version this build supports.
        supported: u32,
    },
}

impl fmt::Display for CassetteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Serialize(error) => write!(formatter, "failed to serialize cassette: {error}"),
            Self::Deserialize(error) => {
                write!(formatter, "failed to deserialize cassette: {error}")
            }
            Self::MissingSchemaVersion => {
                formatter.write_str("cassette is missing a numeric `schema_version` field")
            }
            Self::UnsupportedSchemaVersion { found, supported } => match found {
                Some(found) => write!(
                    formatter,
                    "unsupported cassette schema version {found} (this build supports {supported})"
                ),
                None => write!(
                    formatter,
                    "unsupported cassette schema version (this build supports {supported})"
                ),
            },
        }
    }
}

impl std::error::Error for CassetteError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Serialize(error) | Self::Deserialize(error) => Some(error),
            Self::MissingSchemaVersion | Self::UnsupportedSchemaVersion { .. } => None,
        }
    }
}

/// Computes a stable, order-independent fingerprint for an effect request.
///
/// The request is serialized to JSON, canonicalized (object keys sorted, and
/// volatile per-run ids replaced with [`VOLATILE_ID_PLACEHOLDER`]), and returned
/// as a compact JSON string. Two requests that differ only in volatile ids —
/// requirement/trace/message/step ids or provider tool-call ids — therefore
/// share a fingerprint, while a difference in logical content (tool input,
/// prompt text, model, tools) changes it.
///
/// This is the v1 strategy the plan calls for: the canonical string *is* the
/// fingerprint. A later revision may hash it without changing the matching
/// semantics.
#[must_use]
pub fn request_fingerprint<T: Serialize>(request: &T) -> String {
    let value = serde_json::to_value(request).unwrap_or(Value::Null);
    let canonical = canonicalize(&value, true);
    serde_json::to_string(&canonical).unwrap_or_default()
}

/// Recursively canonicalizes a JSON value: object keys are sorted, and when
/// `strip_ids` is set, string values under a [`VOLATILE_ID_KEYS`] key are
/// replaced with [`VOLATILE_ID_PLACEHOLDER`]. Descending into an
/// [`OPAQUE_SUBTREE_KEYS`] value clears `strip_ids` so meaningful ids inside
/// tool input or a JSON schema survive.
fn canonicalize(value: &Value, strip_ids: bool) -> Value {
    match value {
        Value::Object(map) => {
            let mut entries: Vec<(String, Value)> = map
                .iter()
                .map(|(key, child)| {
                    let normalized = if strip_ids
                        && child.is_string()
                        && VOLATILE_ID_KEYS.contains(&key.as_str())
                    {
                        Value::String(VOLATILE_ID_PLACEHOLDER.to_owned())
                    } else {
                        let descend = strip_ids && !OPAQUE_SUBTREE_KEYS.contains(&key.as_str());
                        canonicalize(child, descend)
                    };
                    (key.clone(), normalized)
                })
                .collect();
            entries.sort_by(|left, right| left.0.cmp(&right.0));
            Value::Object(entries.into_iter().collect())
        }
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| canonicalize(item, strip_ids))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Scrubs sensitive or volatile fields out of effect payloads before they are
/// written to a cassette.
///
/// The cassette boundary records provider-neutral request/result data, but a
/// payload can still carry provider-specific escape-hatch fields
/// ([`provider_extras`](ChatRequest::provider_extras),
/// [`Response::extra`]) whose contents are unmodeled and potentially sensitive.
/// A [`Redactor`] normalizes those before persistence. Every method defaults to
/// a no-op so an implementor overrides only the families it cares about.
pub trait Redactor: Send + Sync {
    /// Redacts an LLM request in place.
    fn redact_chat_request(&self, _request: &mut ChatRequest) {}

    /// Redacts an LLM response in place.
    fn redact_response(&self, _response: &mut Response) {}

    /// Redacts a tool call in place.
    fn redact_tool_call(&self, _call: &mut ToolCall) {}

    /// Redacts a tool response in place.
    fn redact_tool_response(&self, _response: &mut ToolResponse) {}

    /// Redacts an interaction request in place.
    fn redact_interaction(&self, _interaction: &mut Interaction) {}

    /// Redacts an interaction response in place.
    fn redact_interaction_response(&self, _response: &mut InteractionResponse) {}

    /// Redacts a tool-set reference in place.
    fn redact_tool_set(&self, _tool_set: &mut ToolSetRef) {}
}

/// The default redactor: keeps modeled content (message text, tool input,
/// results) but scrubs the *values* of un-allowlisted provider-extras fields on
/// requests and un-allowlisted unmodeled fields on responses.
///
/// Field *keys* are preserved so a cassette still records the shape of the
/// provider payload; only their values become [`REDACTED_PLACEHOLDER`]. The
/// allowlist starts empty, so by default every such field value is redacted.
#[derive(Clone, Debug, Default)]
pub struct DefaultRedactor {
    allowed_fields: BTreeSet<String>,
}

impl DefaultRedactor {
    /// Creates a redactor with an empty allowlist (redacts every extras value).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a provider field name to preserve, returning `self` for chaining.
    #[must_use]
    pub fn allow_field(mut self, name: impl Into<String>) -> Self {
        self.allowed_fields.insert(name.into());
        self
    }

    /// Redacts the values of un-allowlisted keys in an unmodeled field map.
    fn redact_map(&self, map: &mut Map<String, Value>) {
        for (key, value) in map.iter_mut() {
            if !self.allowed_fields.contains(key.as_str()) {
                *value = Value::String(REDACTED_PLACEHOLDER.to_owned());
            }
        }
    }
}

impl Redactor for DefaultRedactor {
    fn redact_chat_request(&self, request: &mut ChatRequest) {
        if let Some(extras) = request.provider_extras.as_mut() {
            self.redact_map(&mut extras.fields);
        }
    }

    fn redact_response(&self, response: &mut Response) {
        self.redact_map(&mut response.extra);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CASSETTE_SCHEMA_VERSION, Cassette, CassetteEntry, CassetteError, CassetteMetadata,
        CassetteObservations, CassetteToolError, DefaultRedactor, InteractionEntry, LlmEntry,
        LlmOutcome, REDACTED_PLACEHOLDER, ReconfigEntry, ReconfigOutcome, Redactor, ToolEntry,
        ToolOutcome, VOLATILE_ID_PLACEHOLDER, request_fingerprint,
    };
    use crate::fixtures::{assistant_text, tool_call, tool_ok, usage, user_message, weather_tool};
    use agent_lib::agent::{
        Interaction, InteractionResponse, LlmStepMode, StepId, ToolRuntimeError, ToolSetId,
        ToolSetRef,
    };
    use agent_lib::client::ChatRequest;
    use agent_lib::model::extras::{ProviderExtras, ProviderId};
    use agent_lib::model::message::Message;
    use agent_lib::model::tool::ToolCall;
    use serde_json::{Map, Value, json};

    fn step_id() -> StepId {
        StepId::parse_str("018f0d9c-7b6a-7c12-8f31-1234567890a3").expect("step id")
    }

    fn tool_set_id() -> ToolSetId {
        ToolSetId::parse_str("018f0d9c-7b6a-7c12-8f31-1234567890a4").expect("tool set id")
    }

    fn chat_request(messages: Vec<Message>, extras: Option<ProviderExtras>) -> ChatRequest {
        ChatRequest {
            model: "test-model".to_owned(),
            messages,
            tools: vec![weather_tool()],
            system: Some("system".to_owned()),
            max_tokens: 512,
            temperature: Some(0.2),
            stream: false,
            provider_extras: extras,
        }
    }

    fn sample_cassette() -> Cassette {
        let mut cassette = Cassette::new(
            CassetteMetadata::new("sample_test")
                .with_description("weather tool round trip")
                .with_crate_version("0.1.0"),
        );
        cassette.push(LlmEntry::new(
            0,
            chat_request(vec![user_message("weather?")], None),
            LlmStepMode::NonStreaming,
            LlmOutcome::Ok(assistant_text("sunny", usage(3, 1))),
        ));
        cassette.push(ToolEntry::new(
            1,
            tool_call("call-1", "get_weather", json!({ "city": "SH" })),
            ToolOutcome::Ok(tool_ok("call-1", "sunny")),
        ));
        cassette.push(ToolEntry::new(
            2,
            tool_call("call-2", "get_weather", json!({ "city": "BJ" })),
            ToolOutcome::Err(CassetteToolError::ExecutionFailed {
                tool_name: "get_weather".to_owned(),
                message: "backend down".to_owned(),
            }),
        ));
        cassette.push(InteractionEntry::new(
            3,
            Interaction::question(step_id(), "approve?".to_owned()),
            InteractionResponse::Answer("yes".to_owned()),
        ));
        cassette.push(ReconfigEntry::new(
            4,
            ToolSetRef::new(tool_set_id(), vec![weather_tool()]),
            ReconfigOutcome::Ok,
        ));
        cassette.observations = Some(CassetteObservations {
            final_cursor: Some("done".to_owned()),
            conversation_summary: Some("1 turn".to_owned()),
            ..CassetteObservations::default()
        });
        cassette
    }

    #[test]
    fn cassette_round_trips_through_json() {
        let cassette = sample_cassette();

        let json = cassette.to_json_string().expect("serialize");
        let decoded = Cassette::from_json_str(&json).expect("deserialize");

        assert_eq!(decoded, cassette);
        assert_eq!(decoded.schema_version, CASSETTE_SCHEMA_VERSION);
        assert_eq!(decoded.entries.len(), 5);
    }

    #[test]
    fn pretty_json_round_trips_and_stays_stable() {
        let cassette = sample_cassette();

        let first = cassette.to_json_string_pretty().expect("serialize");
        let reparsed = Cassette::from_json_str(&first).expect("deserialize");
        let second = reparsed.to_json_string_pretty().expect("reserialize");

        assert_eq!(first, second);
    }

    #[test]
    fn fingerprint_ignores_volatile_ids_but_not_logical_content() {
        // Two chat requests identical except for the provider tool-call ids
        // carried in their message history.
        let mut left_call = tool_call("call-aaaa", "get_weather", json!({ "city": "SH" }));
        let mut right_call = tool_call("call-zzzz", "get_weather", json!({ "city": "SH" }));
        let left = chat_request(
            vec![
                user_message("weather?"),
                assistant_tool_use_message(&left_call),
            ],
            None,
        );
        let right = chat_request(
            vec![
                user_message("weather?"),
                assistant_tool_use_message(&right_call),
            ],
            None,
        );
        assert_eq!(request_fingerprint(&left), request_fingerprint(&right));

        // A single tool call fingerprints the same across ids.
        assert_eq!(
            request_fingerprint(&left_call),
            request_fingerprint(&right_call)
        );

        // But changing the logical tool input changes the fingerprint, proving
        // the opaque `input` subtree is not stripped.
        left_call.input = json!({ "city": "BJ" });
        assert_ne!(
            request_fingerprint(&left_call),
            request_fingerprint(&right_call)
        );

        // And a meaningful `id` inside tool input survives canonicalization.
        right_call.input = json!({ "id": "keep-me", "city": "SH" });
        let mut other = right_call.clone();
        other.input = json!({ "id": "different", "city": "SH" });
        assert_ne!(
            request_fingerprint(&right_call),
            request_fingerprint(&other)
        );
    }

    fn assistant_tool_use_message(call: &ToolCall) -> Message {
        use agent_lib::model::content::ContentBlock;
        use agent_lib::model::message::Role;
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: call.id.clone(),
                name: call.name.clone(),
                input: call.input.clone(),
                extra: Map::new(),
            }],
        }
    }

    #[test]
    fn fingerprint_is_stable_under_key_reordering() {
        let ordered: ToolCall = serde_json::from_value(json!({
            "id": "call-1",
            "name": "get_weather",
            "input": { "a": 1, "b": 2 }
        }))
        .expect("tool call");
        let reordered: ToolCall = serde_json::from_value(json!({
            "name": "get_weather",
            "input": { "b": 2, "a": 1 },
            "id": "call-1"
        }))
        .expect("tool call");

        assert_eq!(
            request_fingerprint(&ordered),
            request_fingerprint(&reordered)
        );
    }

    #[test]
    fn fingerprint_normalizes_volatile_ids_to_placeholder() {
        let call = tool_call("call-xyz", "get_weather", json!({ "city": "SH" }));
        let fingerprint = request_fingerprint(&call);

        assert!(fingerprint.contains(VOLATILE_ID_PLACEHOLDER));
        assert!(!fingerprint.contains("call-xyz"));
    }

    #[test]
    fn default_redactor_scrubs_unknown_provider_extras_and_keeps_text() {
        let mut fields = Map::new();
        fields.insert("api_key".to_owned(), Value::String("secret".to_owned()));
        fields.insert("model_hint".to_owned(), Value::String("fast".to_owned()));
        let extras = ProviderExtras {
            provider: ProviderId::Anthropic,
            fields,
        };
        let mut request = chat_request(vec![user_message("hello world")], Some(extras));

        let redactor = DefaultRedactor::new().allow_field("model_hint");
        redactor.redact_chat_request(&mut request);

        let redacted = request.provider_extras.expect("extras retained");
        assert_eq!(
            redacted.fields.get("api_key"),
            Some(&Value::String(REDACTED_PLACEHOLDER.to_owned()))
        );
        assert_eq!(
            redacted.fields.get("model_hint"),
            Some(&Value::String("fast".to_owned()))
        );

        // Message text is preserved verbatim.
        let serialized = serde_json::to_string(&request.messages).expect("serialize messages");
        assert!(serialized.contains("hello world"));
    }

    #[test]
    fn default_redactor_scrubs_unknown_response_fields() {
        let mut response = assistant_text("hi", usage(1, 1));
        response
            .extra
            .insert("trace_id".to_owned(), Value::String("t-1".to_owned()));

        DefaultRedactor::new().redact_response(&mut response);

        assert_eq!(
            response.extra.get("trace_id"),
            Some(&Value::String(REDACTED_PLACEHOLDER.to_owned()))
        );
    }

    #[test]
    fn unknown_schema_version_is_classified() {
        let mut value: Value =
            serde_json::from_str(&sample_cassette().to_json_string().expect("serialize"))
                .expect("value");
        value["schema_version"] = json!(999);
        let json = serde_json::to_string(&value).expect("reserialize");

        let error = Cassette::from_json_str(&json).expect_err("must reject");
        assert!(matches!(
            error,
            CassetteError::UnsupportedSchemaVersion {
                found: Some(999),
                supported: CASSETTE_SCHEMA_VERSION,
            }
        ));
    }

    #[test]
    fn missing_schema_version_is_classified() {
        let json = json!({
            "metadata": { "test_name": "x" },
            "entries": []
        })
        .to_string();

        let error = Cassette::from_json_str(&json).expect_err("must reject");
        assert!(matches!(error, CassetteError::MissingSchemaVersion));
    }

    #[test]
    fn entry_tag_selects_family_and_exposes_index() {
        let cassette = sample_cassette();
        let json = cassette.to_json_string().expect("serialize");

        assert!(json.contains("\"family\":\"llm\""));
        assert!(json.contains("\"family\":\"tool\""));
        assert!(json.contains("\"family\":\"interaction\""));
        assert!(json.contains("\"family\":\"reconfig\""));

        let indices: Vec<usize> = cassette.entries.iter().map(CassetteEntry::index).collect();
        assert_eq!(indices, vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn cassette_tool_error_round_trips_through_runtime_error() {
        let runtime = ToolRuntimeError::UnknownToolSet { id: tool_set_id() };
        let recorded = CassetteToolError::from(&runtime);
        let restored = ToolRuntimeError::from(recorded.clone());

        assert_eq!(restored, runtime);
        assert_eq!(recorded.to_string(), runtime.to_string());
    }
}
