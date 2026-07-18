//! Runtime **parser cassette**: a redacted, on-disk fixture that freezes the
//! mapping from a runtime's raw CLI output to the parsed
//! [`ExternalObservedEvent`] stream and [`RuntimeDecisionPoint`] a real adapter
//! must produce (design §12).
//!
//! The milestone-3 [`Cassette`](crate::cassette::Cassette) records
//! *provider-neutral effect* req/resp (`ChatRequest`/`ToolCall`/…). This module
//! is a **different** cassette, one layer lower: it targets the concrete runtime
//! adapters (Claude Code / Codex / OpenCode, milestones 6–8) whose parsers turn
//! CLI JSON/JSONL into the neutral external-session types. Those parsers are the
//! part most exposed to upstream protocol drift, so each cassette pins:
//!
//! - the [`runtime`](CassetteRuntimeInfo) it was recorded against (kind, version,
//!   probe fingerprint);
//! - the raw [`input_frames`](CassetteTurn::input_frames) a parser consumes
//!   (opaque stdout/stderr lines — usually JSONL), preserved verbatim so a future
//!   parser test can decode them;
//! - the [`expected_events`](CassetteTurn::expected_events) — the sequenced
//!   [`ExternalObservedEvent`] stream the parser must emit — and the
//!   [`decision`](CassetteTurn::decision) it must settle on;
//! - the [`redaction`](RedactionMetadata) applied when recording.
//!
//! # Loader
//!
//! [`ExternalRuntimeCassette::from_json_str`] / [`load`](ExternalRuntimeCassette::load)
//! read a cassette, validating [`schema_version`](EXTERNAL_CASSETTE_SCHEMA_VERSION)
//! *before* the body so a stale fixture fails loudly. Unknown fields are handled
//! **conservatively**: every unrecognised object key is preserved raw in an
//! `extra` map (never silently dropped, never a hard parse error), so a cassette
//! written by a newer build still round-trips.
//!
//! # Redaction
//!
//! [`scan_secrets`] and [`ExternalRuntimeCassette::assert_no_secrets`] guard that
//! a committed fixture carries no credential-shaped text (`API_KEY`,
//! `AUTH_TOKEN`, `sk-…`, private-key headers, bearer tokens).
//!
//! # Replay
//!
//! [`CassetteRuntimeExternalSessionHandler`] turns a loaded cassette into a
//! production-shaped [`ExternalSessionHandler`]: it drives a
//! [`CassetteExternalRuntimeSession`] through the real
//! [`ExternalSessionRegistry`], replaying each turn's recorded observations
//! (**preserving their recorded `seq`**, unlike the reassigning scripted
//! session) and decision point, so the whole managed loop runs offline from
//! frozen data — the `CassetteExternalSessionHandler` design §12 calls for.

use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::path::Path;
use std::sync::{Arc, Mutex};

use agent_lib::agent::external::{
    ExternalAgentError, ExternalAgentOutput, ExternalEventSink, ExternalObservedEvent,
    ExternalRuntimeAdapter, ExternalRuntimeCapabilities, ExternalRuntimeKind,
    ExternalRuntimeSession, ExternalSessionInput, ExternalSessionRef, ExternalSessionRegistry,
    ExternalSessionRequest, ExternalSessionShutdown, ExternalSubagentRequest, ExternalToolBatchId,
    ExternalToolCall, RuntimeDecisionPoint,
};
use agent_lib::agent::{ExternalSessionHandler, Interaction, RequirementResult, RunContext};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::external::ExternalAgentCallLog;
use crate::external::runtime::{ScriptedRuntimeStartLog, ScriptedSinkLog};
use crate::script::CallLog;

/// Current runtime-cassette schema version.
///
/// A cassette naming a different version is rejected by
/// [`ExternalRuntimeCassette::from_json_str`] as an
/// [`ExternalCassetteError::UnsupportedSchemaVersion`], so a stale fixture fails
/// loudly rather than deserializing into a subtly wrong shape.
pub const EXTERNAL_CASSETTE_SCHEMA_VERSION: u32 = 1;

/// Credential-shaped substrings a committed cassette must never contain.
///
/// Each entry is scanned by [`scan_secrets`]; `case_sensitive` distinguishes the
/// literal key-prefix patterns (`sk-`, `AKIA`) from the case-insensitive
/// field-name patterns (`API_KEY`, `AUTH_TOKEN`).
const SECRET_PATTERNS: &[SecretPattern] = &[
    SecretPattern::insensitive("api_key"),
    SecretPattern::insensitive("auth_token"),
    SecretPattern::insensitive("secret_key"),
    SecretPattern::insensitive("-----begin"),
    SecretPattern::sensitive("sk-"),
    SecretPattern::sensitive("AKIA"),
    SecretPattern::sensitive("Bearer "),
];

/// A recorded runtime session frozen for offline parser-regression replay.
///
/// Load one with [`from_json_str`](Self::from_json_str) or
/// [`load`](Self::load); replay it through a
/// [`CassetteRuntimeExternalSessionHandler`]. Unknown top-level fields are
/// preserved in [`extra`](Self::extra).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExternalRuntimeCassette {
    /// Schema version this cassette was written with.
    pub schema_version: u32,
    /// The runtime the cassette was recorded against.
    pub runtime: CassetteRuntimeInfo,
    /// Redaction applied to the recorded frames and observations.
    #[serde(default)]
    pub redaction: RedactionMetadata,
    /// Ordered turns, one per advance the session was driven through.
    #[serde(default)]
    pub turns: Vec<CassetteTurn>,
    /// Unrecognised top-level fields, preserved raw for forward compatibility.
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

impl ExternalRuntimeCassette {
    /// Creates an empty cassette stamped with the current schema version.
    #[must_use]
    pub fn new(runtime: CassetteRuntimeInfo) -> Self {
        Self {
            schema_version: EXTERNAL_CASSETTE_SCHEMA_VERSION,
            runtime,
            redaction: RedactionMetadata::default(),
            turns: Vec::new(),
            extra: BTreeMap::new(),
        }
    }

    /// Appends one turn and returns `self` for chaining.
    #[must_use]
    pub fn with_turn(mut self, turn: CassetteTurn) -> Self {
        self.turns.push(turn);
        self
    }

    /// Sets the recording's redaction metadata and returns `self` for chaining.
    #[must_use]
    pub fn with_redaction(mut self, redaction: RedactionMetadata) -> Self {
        self.redaction = redaction;
        self
    }

    /// Serializes this cassette to a pretty-printed JSON string.
    ///
    /// # Errors
    ///
    /// Returns [`ExternalCassetteError::Serialize`] when serialization fails.
    pub fn to_json_string_pretty(&self) -> Result<String, ExternalCassetteError> {
        serde_json::to_string_pretty(self).map_err(ExternalCassetteError::Serialize)
    }

    /// Serializes this cassette to a compact JSON string.
    ///
    /// # Errors
    ///
    /// Returns [`ExternalCassetteError::Serialize`] when serialization fails.
    pub fn to_json_string(&self) -> Result<String, ExternalCassetteError> {
        serde_json::to_string(self).map_err(ExternalCassetteError::Serialize)
    }

    /// Parses a cassette from JSON, classifying an unknown schema version.
    ///
    /// The `schema_version` field is read and validated *before* the rest of the
    /// document, so a version mismatch surfaces as an
    /// [`ExternalCassetteError::UnsupportedSchemaVersion`] rather than a
    /// downstream shape error. Unrecognised fields are preserved (never
    /// rejected).
    ///
    /// # Errors
    ///
    /// Returns [`ExternalCassetteError::Deserialize`] when the text is not valid
    /// JSON, [`ExternalCassetteError::MissingSchemaVersion`] when the
    /// `schema_version` field is absent or non-numeric, or
    /// [`ExternalCassetteError::UnsupportedSchemaVersion`] when it names a version
    /// this build does not support.
    pub fn from_json_str(json: &str) -> Result<Self, ExternalCassetteError> {
        let value: Value =
            serde_json::from_str(json).map_err(ExternalCassetteError::Deserialize)?;
        match value.get("schema_version").and_then(Value::as_u64) {
            Some(version) if version == u64::from(EXTERNAL_CASSETTE_SCHEMA_VERSION) => {}
            Some(version) => {
                return Err(ExternalCassetteError::UnsupportedSchemaVersion {
                    found: Some(version),
                    supported: EXTERNAL_CASSETTE_SCHEMA_VERSION,
                });
            }
            None => return Err(ExternalCassetteError::MissingSchemaVersion),
        }
        serde_json::from_value(value).map_err(ExternalCassetteError::Deserialize)
    }

    /// Loads a cassette from a JSON file on disk.
    ///
    /// # Errors
    ///
    /// Returns [`ExternalCassetteError::Io`] when the file cannot be read, or any
    /// error [`from_json_str`](Self::from_json_str) reports for its contents.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ExternalCassetteError> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path).map_err(|error| ExternalCassetteError::Io {
            path: path.display().to_string(),
            detail: error.to_string(),
        })?;
        Self::from_json_str(&text)
    }

    /// Asserts the cassette carries no credential-shaped text.
    ///
    /// Serializes the whole cassette and scans it with [`scan_secrets`], panicking
    /// with the offending pattern(s) if any match. Fixtures call this so a
    /// committed cassette cannot smuggle a secret into the repository.
    ///
    /// # Panics
    ///
    /// Panics if the serialized cassette matches any credential-shaped pattern
    /// [`scan_secrets`] recognises, or if serialization fails.
    pub fn assert_no_secrets(&self) {
        let json = self
            .to_json_string_pretty()
            .expect("a cassette serializes for redaction scanning");
        let hits = scan_secrets(&json);
        assert!(
            hits.is_empty(),
            "cassette contains redaction violations: {}",
            describe_hits(&hits),
        );
    }
}

/// The runtime a cassette was recorded against.
///
/// [`kind`](Self::kind) routes replay to the right adapter shape;
/// [`version`](Self::version) and [`probe`](Self::probe) document the exact CLI
/// build the frames came from so a drift can be attributed. Unknown fields are
/// preserved in [`extra`](Self::extra).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CassetteRuntimeInfo {
    /// Runtime the cassette targets.
    pub kind: ExternalRuntimeKind,
    /// CLI/SDK version string the frames were recorded from, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Free-form probe fingerprint (e.g. `--version` output), when captured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probe: Option<String>,
    /// Runtime-assigned session id the replayed session reports, when fixed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Unrecognised runtime fields, preserved raw.
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

impl CassetteRuntimeInfo {
    /// Builds runtime info for `kind` with no version, probe, or session id.
    #[must_use]
    pub fn new(kind: ExternalRuntimeKind) -> Self {
        Self {
            kind,
            version: None,
            probe: None,
            session_id: None,
            extra: BTreeMap::new(),
        }
    }

    /// Sets the recorded CLI version and returns `self` for chaining.
    #[must_use]
    pub fn with_version(mut self, version: impl Into<String>) -> Self {
        self.version = Some(version.into());
        self
    }

    /// Sets the captured probe fingerprint and returns `self` for chaining.
    #[must_use]
    pub fn with_probe(mut self, probe: impl Into<String>) -> Self {
        self.probe = Some(probe.into());
        self
    }

    /// Sets the runtime-assigned session id and returns `self` for chaining.
    #[must_use]
    pub fn with_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }
}

/// Records the redaction applied to a cassette when it was captured.
///
/// This is descriptive metadata: it documents that a recorder scrubbed
/// credentials and prompt bodies before committing the fixture. The
/// [`assert_no_secrets`](ExternalRuntimeCassette::assert_no_secrets) scan is the
/// enforced guard; this only records the intent and the placeholder token used.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RedactionMetadata {
    /// Whether redaction was applied to the recorded frames/observations.
    #[serde(default)]
    pub applied: bool,
    /// Token substituted in place of redacted content, when one was used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    /// Free-form note describing what was scrubbed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

impl RedactionMetadata {
    /// Metadata declaring redaction was applied with `placeholder`.
    #[must_use]
    pub fn applied(placeholder: impl Into<String>) -> Self {
        Self {
            applied: true,
            placeholder: Some(placeholder.into()),
            notes: None,
        }
    }

    /// Sets the descriptive note and returns `self` for chaining.
    #[must_use]
    pub fn with_notes(mut self, notes: impl Into<String>) -> Self {
        self.notes = Some(notes.into());
        self
    }
}

/// One recorded advance of a cassette session.
///
/// A turn freezes what a parser must produce for one
/// [`advance`](ExternalRuntimeSession::advance): the input it is driven with
/// (optionally asserted via [`expect_input`](Self::expect_input)), the raw
/// [`input_frames`](Self::input_frames) it consumes, the sequenced
/// [`expected_events`](Self::expected_events) it emits, and the
/// [`decision`](Self::decision) it settles on. Unknown fields are preserved in
/// [`extra`](Self::extra).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CassetteTurn {
    /// The input kind this turn expects to be advanced with, when asserted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect_input: Option<CassetteInputKind>,
    /// Raw runtime output frames a parser decodes this turn (opaque).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input_frames: Vec<CassetteFrame>,
    /// Sequenced observations the parser must produce from the frames.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub expected_events: Vec<ExternalObservedEvent>,
    /// The decision point the turn settles on.
    pub decision: CassetteDecision,
    /// Unrecognised turn fields, preserved raw.
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

impl CassetteTurn {
    /// Builds a turn resolving to `decision`, with no frames or events.
    #[must_use]
    pub fn new(decision: CassetteDecision) -> Self {
        Self {
            expect_input: None,
            input_frames: Vec::new(),
            expected_events: Vec::new(),
            decision,
            extra: BTreeMap::new(),
        }
    }

    /// Asserts the turn is advanced with an input of `kind`.
    #[must_use]
    pub fn expecting(mut self, kind: CassetteInputKind) -> Self {
        self.expect_input = Some(kind);
        self
    }

    /// Attaches the raw frames the parser consumes this turn.
    #[must_use]
    pub fn with_frames(mut self, frames: impl IntoIterator<Item = CassetteFrame>) -> Self {
        self.input_frames = frames.into_iter().collect();
        self
    }

    /// Attaches the sequenced observations the parser must emit this turn.
    #[must_use]
    pub fn emitting(mut self, events: impl IntoIterator<Item = ExternalObservedEvent>) -> Self {
        self.expected_events = events.into_iter().collect();
        self
    }
}

/// One raw runtime output line a cassette preserves for a future parser.
///
/// [`payload`](Self::payload) is the verbatim line the runtime wrote (usually a
/// JSONL object); it is opaque to this crate and only stored so a real adapter's
/// parser can be regression-tested against it. Unknown fields are preserved in
/// [`extra`](Self::extra).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CassetteFrame {
    /// Stream the line was written to.
    #[serde(default)]
    pub stream: CassetteStream,
    /// Verbatim output line (opaque; usually a JSONL object).
    pub payload: String,
    /// Unrecognised frame fields, preserved raw.
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

impl CassetteFrame {
    /// A frame on [`Stdout`](CassetteStream::Stdout) carrying `payload`.
    #[must_use]
    pub fn stdout(payload: impl Into<String>) -> Self {
        Self {
            stream: CassetteStream::Stdout,
            payload: payload.into(),
            extra: BTreeMap::new(),
        }
    }

    /// A frame on [`Stderr`](CassetteStream::Stderr) carrying `payload`.
    #[must_use]
    pub fn stderr(payload: impl Into<String>) -> Self {
        Self {
            stream: CassetteStream::Stderr,
            payload: payload.into(),
            extra: BTreeMap::new(),
        }
    }

    /// A [`Stdout`](CassetteStream::Stdout) frame whose payload is `value`
    /// serialized with **every object's keys sorted recursively**.
    ///
    /// Building a payload string from a [`serde_json::Value`] with
    /// `value.to_string()` preserves the underlying map's iteration order, which
    /// is not stable across builds: a default build backs
    /// `serde_json::Value` objects with a sorted `BTreeMap`, but a build that
    /// unifies `serde_json/preserve_order` (for example once `agent-lib`'s
    /// `external-acp` adapter is enabled, whose schema crate pulls that feature
    /// in) backs them with an insertion-order `IndexMap`. A committed cassette
    /// fixture that froze `to_string()` output would then drift purely on
    /// feature unification, even with identical logical content. Canonicalizing
    /// keys makes the payload byte-identical under either build, so a frozen
    /// fixture stays stable — build the JSON-carrying frames of a committed
    /// cassette with this constructor rather than a raw `to_string()`.
    #[must_use]
    pub fn stdout_json(value: &Value) -> Self {
        Self::stdout(canonical_json_string(value))
    }

    /// A [`Stderr`](CassetteStream::Stderr) frame whose payload is `value`
    /// serialized with object keys sorted recursively; the stderr counterpart of
    /// [`stdout_json`](Self::stdout_json), carrying the same
    /// `serde_json/preserve_order` determinism guarantee.
    #[must_use]
    pub fn stderr_json(value: &Value) -> Self {
        Self::stderr(canonical_json_string(value))
    }
}

/// Serializes `value` to a compact JSON string with **every object's keys sorted
/// recursively**, so the result is identical whether or not the build unifies
/// `serde_json/preserve_order`.
///
/// Under a default build `serde_json::Value` objects are already sorted
/// (`BTreeMap`), so this is a no-op; under `preserve_order` (insertion-order
/// `IndexMap`) it re-sorts them. Either way the string matches a fixture frozen
/// from a sorted build.
#[must_use]
pub fn canonical_json_string(value: &Value) -> String {
    sort_json_keys(value).to_string()
}

/// Recursively rebuilds `value` with every object's entries reinserted in
/// key-sorted order.
///
/// Reinserting in sorted order fixes the serialized key order under
/// `preserve_order` (insertion-order `IndexMap`) and is a harmless no-op under a
/// sorted `BTreeMap` build.
fn sort_json_keys(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut entries: Vec<(String, Value)> = map
                .iter()
                .map(|(key, child)| (key.clone(), sort_json_keys(child)))
                .collect();
            entries.sort_by(|left, right| left.0.cmp(&right.0));
            Value::Object(entries.into_iter().collect())
        }
        Value::Array(items) => Value::Array(items.iter().map(sort_json_keys).collect()),
        other => other.clone(),
    }
}

/// Which output stream a [`CassetteFrame`] was captured from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CassetteStream {
    /// Standard output (the primary structured channel for most runtimes).
    #[default]
    Stdout,
    /// Standard error.
    Stderr,
}

/// The input kind a [`CassetteTurn`] expects to be advanced with.
///
/// Mirrors the [`ExternalSessionInput`] discriminants so a turn can pin the exact
/// resume input the machine relays back into the runtime, decoupled from the
/// input's (unserializable-in-full) payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CassetteInputKind {
    /// A fresh [`Start`](ExternalSessionInput::Start).
    Start,
    /// A [`Continue`](ExternalSessionInput::Continue) follow-up.
    Continue,
    /// A [`RespondInteraction`](ExternalSessionInput::RespondInteraction).
    RespondInteraction,
    /// A [`RespondToolResults`](ExternalSessionInput::RespondToolResults).
    RespondToolResults,
    /// A [`RespondSubagent`](ExternalSessionInput::RespondSubagent).
    RespondSubagent,
    /// A [`Shutdown`](ExternalSessionInput::Shutdown).
    Shutdown,
}

impl CassetteInputKind {
    /// Classifies the [`ExternalSessionInput`] an advance was handed.
    #[must_use]
    pub fn classify(input: &ExternalSessionInput) -> Self {
        match input {
            ExternalSessionInput::Start { .. } => Self::Start,
            ExternalSessionInput::Continue { .. } => Self::Continue,
            ExternalSessionInput::RespondInteraction { .. } => Self::RespondInteraction,
            ExternalSessionInput::RespondToolResults { .. } => Self::RespondToolResults,
            ExternalSessionInput::RespondSubagent { .. } => Self::RespondSubagent,
            ExternalSessionInput::Shutdown => Self::Shutdown,
        }
    }
}

/// The control-flow transfer a [`CassetteTurn`] resolves to.
///
/// These mirror the non-session/observation payload of each
/// [`RuntimeDecisionPoint`] variant plus a [`Failed`](Self::Failed) arm carrying
/// the classified error an advance returns as `Err`. The `session` facts and
/// `observations` are supplied by the replaying session (from
/// [`expected_events`](CassetteTurn::expected_events)), so they are not stored
/// twice here.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CassetteDecision {
    /// The session produced terminal output.
    Completed {
        /// Terminal output of the session step.
        output: ExternalAgentOutput,
    },
    /// The session paused awaiting an interaction under `action_id`.
    PausedForInteraction {
        /// Runtime handle echoed back on resume.
        action_id: String,
        /// The interaction the host must resolve.
        request: Interaction,
    },
    /// The session paused awaiting host execution of a tool-call batch.
    PausedForToolCalls {
        /// Identifier the matching results echo back.
        batch_id: ExternalToolBatchId,
        /// Tool calls the host must execute this step.
        calls: Vec<ExternalToolCall>,
    },
    /// The session paused awaiting a host-driven subagent.
    PausedForSubagent {
        /// The subagent spawn the host must drive this step.
        request: ExternalSubagentRequest,
    },
    /// The session failed with a classified error.
    Failed {
        /// Classified failure reason returned as `Err`.
        error: ExternalAgentError,
    },
}

/// One credential-shaped substring found by [`scan_secrets`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SecretHit {
    /// The matched pattern (a credential-shaped substring [`scan_secrets`] flags).
    pub pattern: &'static str,
    /// Byte offset of the match within the scanned text.
    pub offset: usize,
}

/// A credential-shaped pattern scanned for by [`scan_secrets`].
#[derive(Clone, Copy)]
struct SecretPattern {
    needle: &'static str,
    case_sensitive: bool,
}

impl SecretPattern {
    const fn insensitive(needle: &'static str) -> Self {
        Self {
            needle,
            case_sensitive: false,
        }
    }

    const fn sensitive(needle: &'static str) -> Self {
        Self {
            needle,
            case_sensitive: true,
        }
    }
}

/// Scans `text` for any credential-shaped substring (`API_KEY`, `AUTH_TOKEN`,
/// `secret_key`, private-key headers, `sk-…`, `AKIA…`, or bearer tokens).
///
/// Returns every match (pattern + byte offset). Case-insensitive patterns match
/// regardless of case; literal patterns (`sk-`, `AKIA`, `Bearer `) match exactly.
#[must_use]
pub fn scan_secrets(text: &str) -> Vec<SecretHit> {
    let lower = text.to_ascii_lowercase();
    let mut hits = Vec::new();
    for pattern in SECRET_PATTERNS {
        let (haystack, needle_owned);
        let needle: &str = if pattern.case_sensitive {
            haystack = text;
            pattern.needle
        } else {
            haystack = lower.as_str();
            needle_owned = pattern.needle.to_ascii_lowercase();
            needle_owned.as_str()
        };
        let mut from = 0;
        while let Some(found) = haystack[from..].find(needle) {
            let offset = from + found;
            hits.push(SecretHit {
                pattern: pattern.needle,
                offset,
            });
            from = offset + needle.len();
        }
    }
    hits.sort_by_key(|hit| hit.offset);
    hits
}

/// Renders [`scan_secrets`] hits for a panic message.
fn describe_hits(hits: &[SecretHit]) -> String {
    hits.iter()
        .map(|hit| format!("`{}`@{}", hit.pattern, hit.offset))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Error returned while (de)serializing or loading a runtime cassette.
#[derive(Debug)]
pub enum ExternalCassetteError {
    /// Serialization to JSON failed.
    Serialize(serde_json::Error),
    /// Deserialization from JSON failed.
    Deserialize(serde_json::Error),
    /// Reading the cassette file failed.
    Io {
        /// Path the read was attempted against.
        path: String,
        /// Stable diagnostic text.
        detail: String,
    },
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

impl fmt::Display for ExternalCassetteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Serialize(error) => {
                write!(formatter, "failed to serialize runtime cassette: {error}")
            }
            Self::Deserialize(error) => {
                write!(formatter, "failed to deserialize runtime cassette: {error}")
            }
            Self::Io { path, detail } => {
                write!(
                    formatter,
                    "failed to read runtime cassette {path}: {detail}"
                )
            }
            Self::MissingSchemaVersion => {
                formatter.write_str("runtime cassette is missing a numeric `schema_version` field")
            }
            Self::UnsupportedSchemaVersion { found, supported } => match found {
                Some(found) => write!(
                    formatter,
                    "unsupported runtime cassette schema version {found} (this build supports {supported})"
                ),
                None => write!(
                    formatter,
                    "unsupported runtime cassette schema version (this build supports {supported})"
                ),
            },
        }
    }
}

impl std::error::Error for ExternalCassetteError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Serialize(error) | Self::Deserialize(error) => Some(error),
            Self::Io { .. }
            | Self::MissingSchemaVersion
            | Self::UnsupportedSchemaVersion { .. } => None,
        }
    }
}

/// A live session replaying a cassette's recorded turns.
///
/// Unlike the scripted session (which *reassigns* a monotonic `seq` to each
/// event), this session emits every recorded [`ExternalObservedEvent`] **with its
/// frozen `seq`**, so a parser regression that changes the sequence line is
/// caught. Each [`advance`](ExternalRuntimeSession::advance) pops one recorded
/// turn, optionally asserts the input kind, mirrors the turn's observations to
/// the live sink, and returns the recorded decision point.
pub struct CassetteExternalRuntimeSession {
    runtime: ExternalRuntimeKind,
    session_id: String,
    last_event_seq: Option<u64>,
    turns: VecDeque<CassetteTurn>,
    sink: Option<Arc<dyn ExternalEventSink>>,
}

impl CassetteExternalRuntimeSession {
    fn new(
        runtime: ExternalRuntimeKind,
        session_id: String,
        turns: VecDeque<CassetteTurn>,
        sink: Option<Arc<dyn ExternalEventSink>>,
    ) -> Self {
        Self {
            runtime,
            session_id,
            last_event_seq: None,
            turns,
            sink,
        }
    }

    /// Mirrors each recorded observation to the live sink (preserving its `seq`),
    /// advances the high-water mark, and returns the buffered observations.
    fn observe(&mut self, events: Vec<ExternalObservedEvent>) -> Vec<ExternalObservedEvent> {
        for observed in &events {
            if let Some(sink) = &self.sink {
                sink.emit(observed);
            }
            self.last_event_seq = Some(observed.seq);
        }
        events
    }
}

#[async_trait]
impl ExternalRuntimeSession for CassetteExternalRuntimeSession {
    fn session_ref(&self) -> ExternalSessionRef {
        ExternalSessionRef {
            runtime: self.runtime.clone(),
            session_id: Some(self.session_id.clone()),
            transcript_ref: None,
            resume_token: None,
            last_event_seq: self.last_event_seq,
        }
    }

    async fn advance(
        &mut self,
        input: &ExternalSessionInput,
        _ctx: &RunContext,
    ) -> Result<RuntimeDecisionPoint, ExternalAgentError> {
        let Some(turn) = self.turns.pop_front() else {
            return Err(ExternalAgentError::Runtime {
                code: None,
                message: "cassette external runtime session advanced past its recorded turns"
                    .to_owned(),
                runtime_output: None,
            });
        };

        if let Some(expected) = turn.expect_input {
            let actual = CassetteInputKind::classify(input);
            assert_eq!(
                actual, expected,
                "cassette turn expected a {expected:?} input but was driven with {actual:?}",
            );
        }

        let observations = self.observe(turn.expected_events);
        let session = self.session_ref();

        match turn.decision {
            CassetteDecision::Completed { output } => Ok(RuntimeDecisionPoint::Completed {
                session,
                output,
                observations,
            }),
            CassetteDecision::PausedForInteraction { action_id, request } => {
                Ok(RuntimeDecisionPoint::PausedForInteraction {
                    session,
                    action_id,
                    request,
                    observations,
                })
            }
            CassetteDecision::PausedForToolCalls { batch_id, calls } => {
                Ok(RuntimeDecisionPoint::PausedForToolCalls {
                    session,
                    batch_id,
                    calls,
                    observations,
                })
            }
            CassetteDecision::PausedForSubagent { request } => {
                Ok(RuntimeDecisionPoint::PausedForSubagent {
                    session,
                    request,
                    observations,
                })
            }
            CassetteDecision::Failed { error } => Err(error),
        }
    }

    async fn shutdown(&mut self) -> ExternalSessionShutdown {
        ExternalSessionShutdown::Graceful
    }
}

/// A per-runtime factory that hands a cassette's turns to each fresh
/// [`start`](ExternalRuntimeAdapter::start).
///
/// It carries one recorded turn queue handed out on the first start; a second
/// start with the queue already taken fails with
/// [`ExternalAgentError::Launch`]. Every start request is recorded in a shared
/// [`ScriptedRuntimeStartLog`].
pub struct CassetteExternalRuntimeAdapter {
    runtime: ExternalRuntimeKind,
    session_id: String,
    capabilities: ExternalRuntimeCapabilities,
    turns: Mutex<Option<VecDeque<CassetteTurn>>>,
    start_log: ScriptedRuntimeStartLog,
}

impl CassetteExternalRuntimeAdapter {
    /// Returns the shared log of every start request this adapter serviced.
    #[must_use]
    pub fn start_log(&self) -> &ScriptedRuntimeStartLog {
        &self.start_log
    }
}

#[async_trait]
impl ExternalRuntimeAdapter for CassetteExternalRuntimeAdapter {
    fn kind(&self) -> ExternalRuntimeKind {
        self.runtime.clone()
    }

    fn capabilities(&self) -> ExternalRuntimeCapabilities {
        self.capabilities.clone()
    }

    async fn start(
        &self,
        request: &ExternalSessionRequest,
        _ctx: &RunContext,
        sink: Option<Arc<dyn ExternalEventSink>>,
    ) -> Result<Box<dyn ExternalRuntimeSession>, ExternalAgentError> {
        self.start_log.record(request.clone());
        let turns = self
            .turns
            .lock()
            .expect("cassette adapter turns mutex poisoned")
            .take()
            .ok_or_else(|| ExternalAgentError::Launch {
                runtime: self.runtime.clone(),
                detail: "cassette external runtime adapter has no recorded turns left to start"
                    .to_owned(),
            })?;
        Ok(Box::new(CassetteExternalRuntimeSession::new(
            self.runtime.clone(),
            self.session_id.clone(),
            turns,
            sink,
        )))
    }
}

/// An [`ExternalSessionHandler`] replaying a cassette through a real
/// [`ExternalSessionRegistry`].
///
/// This is the design §12 `CassetteExternalSessionHandler`: it holds no machine
/// state. Every [`fulfill`](ExternalSessionHandler::fulfill) resolves the live
/// handle through the registry
/// ([`get_or_start`](ExternalSessionRegistry::get_or_start) starts on the first
/// [`Start`](ExternalSessionInput::Start), reattaches on every follow-up turn),
/// advances it one recorded [`RuntimeDecisionPoint`], and folds the outcome into
/// a family-aligned [`RequirementResult::ExternalSession`]. Build one from a
/// loaded cassette with [`from_cassette`](Self::from_cassette).
pub struct CassetteRuntimeExternalSessionHandler {
    registry: Arc<ExternalSessionRegistry>,
    sink: Arc<ScriptedSinkLog>,
    log: Arc<ExternalAgentCallLog>,
    start_log: ScriptedRuntimeStartLog,
}

impl CassetteRuntimeExternalSessionHandler {
    /// Builds a registry-backed replay handler from a loaded cassette.
    ///
    /// The runtime kind and (optional) session id are read from the cassette's
    /// [`runtime`](ExternalRuntimeCassette::runtime); a session id defaults to
    /// `"cassette-sess-1"` when the cassette does not fix one. Capabilities are
    /// permissive with `resume` off, so a follow-up turn reattaches through the
    /// live handle rather than an adapter resume path.
    #[must_use]
    pub fn from_cassette(cassette: &ExternalRuntimeCassette) -> Self {
        let runtime = cassette.runtime.kind.clone();
        let session_id = cassette
            .runtime
            .session_id
            .clone()
            .unwrap_or_else(|| "cassette-sess-1".to_owned());
        let start_log = ScriptedRuntimeStartLog::default();
        let adapter = CassetteExternalRuntimeAdapter {
            runtime: runtime.clone(),
            session_id,
            capabilities: permissive_capabilities(runtime),
            turns: Mutex::new(Some(cassette.turns.iter().cloned().collect())),
            start_log: start_log.clone(),
        };
        let registry = Arc::new(ExternalSessionRegistry::with_worktree_manager(
            Arc::new(adapter) as Arc<dyn ExternalRuntimeAdapter>,
            Arc::new(crate::external::PassThroughWorktreeManager),
        ));
        Self {
            registry,
            sink: Arc::new(ScriptedSinkLog::default()),
            log: Arc::new(CallLog::new()),
            start_log,
        }
    }

    /// Returns the registry that owns the handler's live session.
    #[must_use]
    pub fn registry(&self) -> &Arc<ExternalSessionRegistry> {
        &self.registry
    }

    /// Returns the collecting sink recording every replayed observation.
    #[must_use]
    pub fn sink(&self) -> &Arc<ScriptedSinkLog> {
        &self.sink
    }

    /// Returns the call log recording every fulfilled `NeedExternalSession`.
    #[must_use]
    pub fn log(&self) -> &Arc<ExternalAgentCallLog> {
        &self.log
    }

    /// Returns the log of every fresh session the adapter started.
    #[must_use]
    pub fn start_log(&self) -> &ScriptedRuntimeStartLog {
        &self.start_log
    }

    /// Resolves the live handle and advances it one recorded decision point,
    /// folding both a `get_or_start` failure and an `advance` failure into a
    /// family-aligned [`ExternalSessionResult`](agent_lib::agent::external::ExternalSessionResult).
    async fn advance(
        &self,
        request: &ExternalSessionRequest,
        ctx: &RunContext,
    ) -> agent_lib::agent::external::ExternalSessionResult {
        let sink: Arc<dyn ExternalEventSink> = Arc::clone(&self.sink) as Arc<dyn ExternalEventSink>;
        let handle = match self.registry.get_or_start(request, ctx, Some(sink)).await {
            Ok(handle) => handle,
            Err(error) => return Err::<RuntimeDecisionPoint, _>(error).into(),
        };
        let mut session = handle.lock().await;
        let point = session.advance(&request.input, ctx).await;
        point.into()
    }
}

#[async_trait]
impl ExternalSessionHandler for CassetteRuntimeExternalSessionHandler {
    async fn fulfill(
        &self,
        request: &ExternalSessionRequest,
        ctx: &RunContext,
    ) -> RequirementResult {
        let ticket = self.log.begin(request.clone());
        let result = RequirementResult::ExternalSession(Box::new(self.advance(request, ctx).await));
        self.log.complete(ticket, result.clone());
        result
    }
}

/// A permissive capability set: every managed feature on except `resume`.
fn permissive_capabilities(runtime: ExternalRuntimeKind) -> ExternalRuntimeCapabilities {
    ExternalRuntimeCapabilities {
        runtime,
        streaming: true,
        resume: false,
        permission_bridge: true,
        host_tools: true,
        host_subagents: true,
        artifacts: true,
        usage: true,
        graceful_shutdown: true,
        reconfigure: true,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CassetteDecision, CassetteFrame, CassetteInputKind, CassetteRuntimeInfo, CassetteStream,
        CassetteTurn, EXTERNAL_CASSETTE_SCHEMA_VERSION, ExternalCassetteError,
        ExternalRuntimeCassette, RedactionMetadata, scan_secrets,
    };
    use agent_lib::agent::external::{
        ExternalAgentEvent, ExternalAgentOutput, ExternalObservedEvent, ExternalRuntimeKind,
    };
    use serde_json::{Map, Value, json};

    fn sample_cassette() -> ExternalRuntimeCassette {
        ExternalRuntimeCassette::new(
            CassetteRuntimeInfo::new(ExternalRuntimeKind::ClaudeCode)
                .with_version("1.2.3")
                .with_session_id("cassette-sess-1"),
        )
        .with_redaction(RedactionMetadata::applied("<redacted>"))
        .with_turn(
            CassetteTurn::new(CassetteDecision::Completed {
                output: ExternalAgentOutput {
                    summary: "done".to_owned(),
                    artifacts: Vec::new(),
                    usage: None,
                    cost_micros: None,
                },
            })
            .expecting(CassetteInputKind::Start)
            .with_frames([CassetteFrame::stdout("{\"type\":\"text\",\"text\":\"hi\"}")])
            .emitting([
                ExternalObservedEvent::new(
                    0,
                    ExternalAgentEvent::TextDelta {
                        text: "hi".to_owned(),
                    },
                ),
                ExternalObservedEvent::new(1, ExternalAgentEvent::SessionCompleted),
            ]),
        )
    }

    #[test]
    fn cassette_round_trips_through_json() {
        let cassette = sample_cassette();
        let json = cassette.to_json_string_pretty().expect("serialize");
        let loaded = ExternalRuntimeCassette::from_json_str(&json).expect("deserialize");
        assert_eq!(loaded, cassette);
        assert_eq!(loaded.schema_version, EXTERNAL_CASSETTE_SCHEMA_VERSION);
    }

    #[test]
    fn cassette_preserves_unknown_fields_raw() {
        let json = r#"{
            "schema_version": 1,
            "runtime": { "kind": "codex", "future_runtime_field": 7 },
            "turns": [],
            "future_top_level": { "nested": true }
        }"#;
        let cassette = ExternalRuntimeCassette::from_json_str(json).expect("load");
        assert!(cassette.extra.contains_key("future_top_level"));
        assert!(cassette.runtime.extra.contains_key("future_runtime_field"));
        // Preserved fields survive a re-serialize.
        let round = cassette.to_json_string().expect("serialize");
        let reloaded = ExternalRuntimeCassette::from_json_str(&round).expect("reload");
        assert_eq!(reloaded, cassette);
    }

    #[test]
    fn cassette_rejects_unknown_schema_version() {
        let json = r#"{ "schema_version": 999, "runtime": { "kind": "codex" } }"#;
        match ExternalRuntimeCassette::from_json_str(json) {
            Err(ExternalCassetteError::UnsupportedSchemaVersion { found, supported }) => {
                assert_eq!(found, Some(999));
                assert_eq!(supported, EXTERNAL_CASSETTE_SCHEMA_VERSION);
            }
            other => panic!("expected UnsupportedSchemaVersion, got {other:?}"),
        }
    }

    #[test]
    fn cassette_missing_schema_version_is_classified() {
        let json = r#"{ "runtime": { "kind": "codex" } }"#;
        assert!(matches!(
            ExternalRuntimeCassette::from_json_str(json),
            Err(ExternalCassetteError::MissingSchemaVersion)
        ));
    }

    #[test]
    fn cassette_default_frame_stream_is_stdout() {
        let frame: CassetteFrame = serde_json::from_str(r#"{ "payload": "line" }"#).expect("frame");
        assert_eq!(frame.stream, CassetteStream::Stdout);
    }

    #[test]
    fn stdout_json_sorts_object_keys_recursively() {
        // Build an object whose keys are inserted in reverse-sorted order. Under
        // a `preserve_order` (all-features) build this insertion order survives
        // `to_string()`; `stdout_json` must still emit fully sorted keys so a
        // frozen fixture never drifts on feature unification.
        let mut nested = Map::new();
        nested.insert("z".to_owned(), json!(1));
        nested.insert("a".to_owned(), json!(2));
        let mut outer = Map::new();
        outer.insert("type".to_owned(), json!("system"));
        outer.insert("nested".to_owned(), Value::Object(nested));
        outer.insert("cwd".to_owned(), json!("/repo"));
        let value = Value::Object(outer);

        let frame = CassetteFrame::stdout_json(&value);
        assert_eq!(frame.stream, CassetteStream::Stdout);
        assert_eq!(
            frame.payload,
            r#"{"cwd":"/repo","nested":{"a":2,"z":1},"type":"system"}"#,
        );
        // The stderr counterpart canonicalizes identically.
        assert_eq!(
            CassetteFrame::stderr_json(&value).payload,
            frame.payload,
            "stderr_json must canonicalize the same way as stdout_json",
        );
    }

    #[test]
    fn scan_secrets_flags_credential_shapes() {
        assert!(scan_secrets("nothing to see here").is_empty());
        assert!(!scan_secrets("token=sk-abc123").is_empty());
        assert!(!scan_secrets("HEADER_API_KEY: x").is_empty());
        assert!(!scan_secrets("auth_token = y").is_empty());
        assert!(!scan_secrets("Authorization: Bearer zzz").is_empty());
    }

    #[test]
    fn assert_no_secrets_passes_for_clean_cassette() {
        sample_cassette().assert_no_secrets();
    }

    #[test]
    #[should_panic(expected = "redaction violations")]
    fn assert_no_secrets_panics_on_leaked_secret() {
        let mut cassette = sample_cassette();
        cassette.runtime.probe = Some("token sk-leaked".to_owned());
        cassette.assert_no_secrets();
    }
}
