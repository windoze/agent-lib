//! Record / verify / update wrappers that capture real effect traffic into a
//! [`Cassette`].
//!
//! A [`CassetteRecorder`] wraps the four real effect handler traits. Each
//! wrapper calls the *real* handler, then normalizes the request/result of that
//! one call — passing it through the configured [`Redactor`] — into a
//! [`CassetteEntry`] appended in global dispatch order. Because recording and
//! updating drive live handlers (network, credentials, real tool backends), the
//! two writing modes are gated behind explicit environment opt-ins so a normal
//! CI run can never silently overwrite a committed fixture.
//!
//! # Modes
//!
//! - [`Record`](RecorderMode::Record): drive the real handlers and write a fresh
//!   cassette. Gated by [`RECORD_ENV_VAR`].
//! - [`Verify`](RecorderMode::Verify): drive the real handlers and compare the
//!   live traffic against an on-disk cassette, reporting any drift. Never
//!   writes, so it needs no opt-in.
//! - [`Update`](RecorderMode::Update): drive the real handlers and overwrite an
//!   existing cassette. Gated by [`UPDATE_ENV_VAR`].
//!
//! # Flow
//!
//! A test gates on [`CassetteRecorder::is_enabled`] before wrapping (so it never
//! calls a live backend when the mode is not opted in), wraps its real handlers,
//! runs the turn, then calls [`CassetteRecorder::finish`]:
//!
//! - a writing mode serializes the accumulated entries and persists them with a
//!   temp-file-plus-atomic-rename so a crash mid-write never leaves a truncated
//!   cassette, or returns [`RecorderReport::Skipped`] when the opt-in is absent;
//! - [`Verify`](RecorderMode::Verify) loads the on-disk cassette and returns a
//!   [`RecorderError::Drift`] naming every diverging entry.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use agent_lib::agent::{
    Interaction, InteractionHandler, LlmHandler, LlmStepMode, ReconfigHandler, RequirementKindTag,
    RequirementResult, RunContext, ToolHandler, ToolSetRef,
};
use agent_lib::client::ChatRequest;
use agent_lib::conversation::ToolCallId;
use agent_lib::model::tool::ToolCall;
use async_trait::async_trait;
use std::fmt;

use super::{
    Cassette, CassetteEntry, CassetteError, CassetteMetadata, DefaultRedactor, InteractionEntry,
    LlmEntry, LlmOutcome, ReconfigEntry, ReconfigOutcome, Redactor, ToolEntry, ToolOutcome,
};

/// Environment variable that must equal `"1"` to enable
/// [`Record`](RecorderMode::Record) writes.
pub const RECORD_ENV_VAR: &str = "AGENT_TESTKIT_RECORD_CASSETTES";

/// Environment variable that must equal `"1"` to enable
/// [`Update`](RecorderMode::Update) overwrites.
pub const UPDATE_ENV_VAR: &str = "AGENT_TESTKIT_UPDATE_CASSETTES";

/// How a [`CassetteRecorder`] treats the traffic it captures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecorderMode {
    /// Drive the real handlers and write a fresh cassette (gated by
    /// [`RECORD_ENV_VAR`]).
    Record,
    /// Drive the real handlers and compare against an on-disk cassette; never
    /// writes.
    Verify,
    /// Drive the real handlers and overwrite an existing cassette (gated by
    /// [`UPDATE_ENV_VAR`]).
    Update,
}

impl RecorderMode {
    /// Returns the environment variable gating this mode's writes, or `None` for
    /// [`Verify`](RecorderMode::Verify), which never writes.
    #[must_use]
    pub const fn env_var(self) -> Option<&'static str> {
        match self {
            Self::Record => Some(RECORD_ENV_VAR),
            Self::Update => Some(UPDATE_ENV_VAR),
            Self::Verify => None,
        }
    }

    /// Returns whether this mode persists a cassette to disk.
    #[must_use]
    pub const fn writes(self) -> bool {
        matches!(self, Self::Record | Self::Update)
    }
}

impl fmt::Display for RecorderMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::Record => "record",
            Self::Verify => "verify",
            Self::Update => "update",
        };
        formatter.write_str(text)
    }
}

/// Shared, dispatch-ordered accumulator the wrappers append entries to.
///
/// One state is cloned (by `Arc`) into every wrapper the recorder builds, so
/// entries from all four families land in the single global order they were
/// dispatched in.
#[derive(Debug, Default)]
struct RecorderState {
    entries: Mutex<Vec<CassetteEntry>>,
}

impl RecorderState {
    /// Appends one entry, handing the closure the entry's global dispatch index.
    ///
    /// The lock spans index selection and the push so concurrent wrappers can
    /// never collide on an index or interleave a torn entry.
    fn record(&self, make: impl FnOnce(usize) -> CassetteEntry) {
        let mut entries = self.entries.lock().expect("recorder state mutex poisoned");
        let index = entries.len();
        entries.push(make(index));
    }

    /// Returns a snapshot of the accumulated entries in dispatch order.
    fn snapshot(&self) -> Vec<CassetteEntry> {
        self.entries
            .lock()
            .expect("recorder state mutex poisoned")
            .clone()
    }
}

/// Wraps real effect handlers to record, verify, or update a [`Cassette`].
///
/// Build one with [`record`](Self::record), [`verify`](Self::verify), or
/// [`update`](Self::update), optionally configure a [`Redactor`] and
/// [`CassetteMetadata`], wrap each real handler with the matching `wrap_*`
/// method, run the turn, then [`finish`](Self::finish). Every wrapper shares the
/// recorder's accumulator, so a turn that mixes families records them in one
/// global dispatch order.
#[derive(Clone)]
pub struct CassetteRecorder {
    path: PathBuf,
    mode: RecorderMode,
    metadata: CassetteMetadata,
    redactor: Arc<dyn Redactor>,
    enabled_override: Option<bool>,
    state: Arc<RecorderState>,
}

impl fmt::Debug for CassetteRecorder {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CassetteRecorder")
            .field("path", &self.path)
            .field("mode", &self.mode)
            .field("metadata", &self.metadata)
            .field("enabled_override", &self.enabled_override)
            .finish_non_exhaustive()
    }
}

impl CassetteRecorder {
    fn new(path: impl Into<PathBuf>, mode: RecorderMode) -> Self {
        Self {
            path: path.into(),
            mode,
            metadata: CassetteMetadata::default(),
            redactor: Arc::new(DefaultRedactor::new()),
            enabled_override: None,
            state: Arc::new(RecorderState::default()),
        }
    }

    /// Builds a recorder that writes a fresh cassette to `path`.
    ///
    /// Writing is gated by [`RECORD_ENV_VAR`]; see [`is_enabled`](Self::is_enabled).
    #[must_use]
    pub fn record(path: impl Into<PathBuf>) -> Self {
        Self::new(path, RecorderMode::Record)
    }

    /// Builds a recorder that compares live traffic against the cassette at
    /// `path` without writing.
    #[must_use]
    pub fn verify(path: impl Into<PathBuf>) -> Self {
        Self::new(path, RecorderMode::Verify)
    }

    /// Builds a recorder that overwrites the cassette at `path`.
    ///
    /// Writing is gated by [`UPDATE_ENV_VAR`]; see [`is_enabled`](Self::is_enabled).
    #[must_use]
    pub fn update(path: impl Into<PathBuf>) -> Self {
        Self::new(path, RecorderMode::Update)
    }

    /// Sets the [`Redactor`] every wrapper runs captured payloads through.
    #[must_use]
    pub fn with_redactor<R: Redactor + 'static>(mut self, redactor: R) -> Self {
        self.redactor = Arc::new(redactor);
        self
    }

    /// Sets the [`CassetteMetadata`] stamped onto a written cassette.
    #[must_use]
    pub fn with_metadata(mut self, metadata: CassetteMetadata) -> Self {
        self.metadata = metadata;
        self
    }

    /// Forces the env opt-in gate on or off, bypassing [`RECORD_ENV_VAR`] /
    /// [`UPDATE_ENV_VAR`].
    ///
    /// This is an explicit test hook: a harness that decides enablement itself
    /// (or a test that must exercise the writing path without mutating
    /// process-global environment) can force the decision. It has no effect in
    /// [`Verify`](RecorderMode::Verify) mode, which never writes.
    #[must_use]
    pub fn with_enabled_override(mut self, enabled: bool) -> Self {
        self.enabled_override = Some(enabled);
        self
    }

    /// Returns the cassette path this recorder reads or writes.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the recorder's mode.
    #[must_use]
    pub const fn mode(&self) -> RecorderMode {
        self.mode
    }

    /// Returns whether this recorder is allowed to proceed.
    ///
    /// [`Verify`](RecorderMode::Verify) is always enabled (it never writes). A
    /// writing mode is enabled only when [`with_enabled_override`](Self::with_enabled_override)
    /// forced it, or its [gating env var](RecorderMode::env_var) equals `"1"`.
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        match self.mode {
            RecorderMode::Verify => true,
            RecorderMode::Record | RecorderMode::Update => {
                self.enabled_override.unwrap_or_else(|| {
                    env_flag(self.mode.env_var().expect("writing mode has env var"))
                })
            }
        }
    }

    /// Returns a skip explanation when a writing mode is not opted in.
    ///
    /// A record/update test should call this first and return early (a
    /// skipped/ignored style exit) so it never drives a live backend when the
    /// opt-in is absent.
    #[must_use]
    pub fn skip_reason(&self) -> Option<String> {
        if self.mode.writes() && !self.is_enabled() {
            let env_var = self.mode.env_var().expect("writing mode has env var");
            Some(format!(
                "cassette {} for `{}` is disabled; set {}=1 to enable it",
                self.mode,
                self.path.display(),
                env_var
            ))
        } else {
            None
        }
    }

    /// Wraps a real [`LlmHandler`], recording every generation it fulfils.
    pub fn wrap_llm<H: LlmHandler + 'static>(&self, inner: H) -> RecordingLlmHandler {
        RecordingLlmHandler {
            inner: Arc::new(inner),
            state: Arc::clone(&self.state),
            redactor: Arc::clone(&self.redactor),
        }
    }

    /// Wraps a real [`ToolHandler`], recording every execution it fulfils.
    pub fn wrap_tool<H: ToolHandler + 'static>(&self, inner: H) -> RecordingToolHandler {
        RecordingToolHandler {
            inner: Arc::new(inner),
            state: Arc::clone(&self.state),
            redactor: Arc::clone(&self.redactor),
        }
    }

    /// Wraps a real [`InteractionHandler`], recording every interaction it
    /// resolves.
    pub fn wrap_interaction<H: InteractionHandler + 'static>(
        &self,
        inner: H,
    ) -> RecordingInteractionHandler {
        RecordingInteractionHandler {
            inner: Arc::new(inner),
            state: Arc::clone(&self.state),
            redactor: Arc::clone(&self.redactor),
        }
    }

    /// Wraps a real [`ReconfigHandler`], recording every reconfiguration it
    /// resolves.
    pub fn wrap_reconfig<H: ReconfigHandler + 'static>(
        &self,
        inner: H,
    ) -> RecordingReconfigHandler {
        RecordingReconfigHandler {
            inner: Arc::new(inner),
            state: Arc::clone(&self.state),
            redactor: Arc::clone(&self.redactor),
        }
    }

    /// Builds the cassette from the entries accumulated so far.
    #[must_use]
    pub fn build_cassette(&self) -> Cassette {
        let mut cassette = Cassette::new(self.metadata.clone());
        cassette.entries = self.state.snapshot();
        cassette
    }

    /// Finalizes the recording: writes, verifies, or skips.
    ///
    /// # Errors
    ///
    /// Returns [`RecorderError::Serialize`] or [`RecorderError::Io`] when a
    /// writing mode fails to persist the cassette, and, in
    /// [`Verify`](RecorderMode::Verify) mode, [`RecorderError::Io`] /
    /// [`RecorderError::Load`] when the on-disk cassette cannot be read and
    /// [`RecorderError::Drift`] when the live traffic diverges from it.
    pub fn finish(&self) -> Result<RecorderReport, RecorderError> {
        match self.mode {
            RecorderMode::Verify => self.finish_verify(),
            RecorderMode::Record | RecorderMode::Update => self.finish_write(),
        }
    }

    fn finish_write(&self) -> Result<RecorderReport, RecorderError> {
        if !self.is_enabled() {
            return Ok(RecorderReport::Skipped {
                mode: self.mode,
                env_var: self.mode.env_var().expect("writing mode has env var"),
            });
        }
        let cassette = self.build_cassette();
        let json = cassette
            .to_json_string_pretty()
            .map_err(RecorderError::Serialize)?;
        write_atomic(&self.path, json.as_bytes()).map_err(RecorderError::Io)?;
        Ok(RecorderReport::Wrote {
            path: self.path.clone(),
            entry_count: cassette.entries.len(),
        })
    }

    fn finish_verify(&self) -> Result<RecorderReport, RecorderError> {
        let text = std::fs::read_to_string(&self.path).map_err(RecorderError::Io)?;
        let recorded = Cassette::from_json_str(&text).map_err(RecorderError::Load)?;
        let live = self.state.snapshot();
        let drifts = diff_entries(&recorded.entries, &live);
        if drifts.is_empty() {
            Ok(RecorderReport::Verified {
                entry_count: live.len(),
            })
        } else {
            Err(RecorderError::Drift(drifts))
        }
    }
}

/// The successful outcome of [`CassetteRecorder::finish`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecorderReport {
    /// A writing mode persisted the cassette.
    Wrote {
        /// Path the cassette was written to.
        path: PathBuf,
        /// Number of entries written.
        entry_count: usize,
    },
    /// [`Verify`](RecorderMode::Verify) confirmed the live traffic matched the
    /// on-disk cassette.
    Verified {
        /// Number of live entries compared.
        entry_count: usize,
    },
    /// A writing mode was skipped because its env opt-in was absent.
    Skipped {
        /// The mode that was skipped.
        mode: RecorderMode,
        /// The env var that would enable it.
        env_var: &'static str,
    },
}

/// Why [`CassetteRecorder::finish`] failed.
#[derive(Debug)]
pub enum RecorderError {
    /// [`Verify`](RecorderMode::Verify) found the live traffic diverged from the
    /// on-disk cassette.
    Drift(Vec<EntryDrift>),
    /// The on-disk cassette could not be parsed while verifying.
    Load(CassetteError),
    /// The cassette could not be serialized while writing.
    Serialize(CassetteError),
    /// Reading or writing the cassette file failed.
    Io(std::io::Error),
}

impl fmt::Display for RecorderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Drift(drifts) => {
                write!(
                    formatter,
                    "cassette verify detected {} drift(s):",
                    drifts.len()
                )?;
                for drift in drifts {
                    write!(formatter, "\n  - {drift}")?;
                }
                Ok(())
            }
            Self::Load(error) => write!(formatter, "failed to load cassette to verify: {error}"),
            Self::Serialize(error) => write!(formatter, "failed to serialize cassette: {error}"),
            Self::Io(error) => write!(formatter, "cassette I/O error: {error}"),
        }
    }
}

impl std::error::Error for RecorderError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Load(error) | Self::Serialize(error) => Some(error),
            Self::Io(error) => Some(error),
            Self::Drift(_) => None,
        }
    }
}

/// One diverging entry found while verifying live traffic against a cassette.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EntryDrift {
    position: usize,
    family: RequirementKindTag,
    detail: String,
}

impl EntryDrift {
    /// Returns the global dispatch position that diverged.
    #[must_use]
    pub const fn position(&self) -> usize {
        self.position
    }

    /// Returns the effect family of the diverging entry.
    #[must_use]
    pub const fn family(&self) -> RequirementKindTag {
        self.family
    }

    /// Returns the human-readable description of the divergence.
    #[must_use]
    pub fn detail(&self) -> &str {
        &self.detail
    }
}

impl fmt::Display for EntryDrift {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "entry #{} ({} family): {}",
            self.position, self.family, self.detail
        )
    }
}

/// Reads an env var and returns whether it equals `"1"`.
fn env_flag(name: &str) -> bool {
    std::env::var(name)
        .map(|value| value == "1")
        .unwrap_or(false)
}

/// Returns the effect family a recorded entry belongs to.
fn family_of(entry: &CassetteEntry) -> RequirementKindTag {
    match entry {
        CassetteEntry::Llm(_) => RequirementKindTag::Llm,
        CassetteEntry::Tool(_) => RequirementKindTag::Tool,
        CassetteEntry::Interaction(_) => RequirementKindTag::Interaction,
        CassetteEntry::Reconfig(_) => RequirementKindTag::Reconfig,
    }
}

/// Returns whether two same-family entries carry a different recorded result.
fn results_differ(recorded: &CassetteEntry, live: &CassetteEntry) -> bool {
    match (recorded, live) {
        (CassetteEntry::Llm(a), CassetteEntry::Llm(b)) => a.result != b.result,
        (CassetteEntry::Tool(a), CassetteEntry::Tool(b)) => a.result != b.result,
        (CassetteEntry::Interaction(a), CassetteEntry::Interaction(b)) => a.result != b.result,
        (CassetteEntry::Reconfig(a), CassetteEntry::Reconfig(b)) => a.result != b.result,
        _ => true,
    }
}

/// Compares recorded entries against live entries position by position.
fn diff_entries(recorded: &[CassetteEntry], live: &[CassetteEntry]) -> Vec<EntryDrift> {
    let mut drifts = Vec::new();
    let count = recorded.len().max(live.len());
    for position in 0..count {
        match (recorded.get(position), live.get(position)) {
            (Some(recorded_entry), Some(live_entry)) => {
                if let Some(drift) = diff_entry(position, recorded_entry, live_entry) {
                    drifts.push(drift);
                }
            }
            (Some(recorded_entry), None) => drifts.push(EntryDrift {
                position,
                family: family_of(recorded_entry),
                detail: format!(
                    "missing live call: cassette recorded a {} entry with no matching live call",
                    family_of(recorded_entry)
                ),
            }),
            (None, Some(live_entry)) => drifts.push(EntryDrift {
                position,
                family: family_of(live_entry),
                detail: format!(
                    "unexpected live call: live produced a {} entry not present in the cassette",
                    family_of(live_entry)
                ),
            }),
            (None, None) => {}
        }
    }
    drifts
}

/// Classifies the divergence between one recorded and one live entry.
fn diff_entry(
    position: usize,
    recorded: &CassetteEntry,
    live: &CassetteEntry,
) -> Option<EntryDrift> {
    if recorded == live {
        return None;
    }
    let recorded_family = family_of(recorded);
    let live_family = family_of(live);
    if recorded_family != live_family {
        return Some(EntryDrift {
            position,
            family: recorded_family,
            detail: format!(
                "family drift: cassette recorded a `{recorded_family}` call but the live call was `{live_family}`"
            ),
        });
    }
    let mut parts = Vec::new();
    if recorded.fingerprint() != live.fingerprint() {
        parts.push(format!(
            "request drift: recorded fingerprint `{}`, live fingerprint `{}`",
            recorded.fingerprint(),
            live.fingerprint()
        ));
    }
    if results_differ(recorded, live) {
        parts.push("result drift: recorded result differs from the live result".to_owned());
    }
    if parts.is_empty() {
        parts.push("entry drift: recorded entry differs from the live entry".to_owned());
    }
    Some(EntryDrift {
        position,
        family: recorded_family,
        detail: parts.join("; "),
    })
}

/// Writes `bytes` to `path` via a temp file and an atomic rename.
///
/// A crash between the write and the rename leaves the previous cassette intact
/// and only an orphan temp file behind, never a half-written target.
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = temp_path(path);
    if let Err(error) = std::fs::write(&tmp, bytes) {
        let _ = std::fs::remove_file(&tmp);
        return Err(error);
    }
    if let Err(error) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(error);
    }
    Ok(())
}

/// Builds a unique sibling temp path for [`write_atomic`].
fn temp_path(path: &Path) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("cassette.json");
    let tmp_name = format!(".{file_name}.tmp.{pid}.{nanos}.{counter}");
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(tmp_name),
        _ => PathBuf::from(tmp_name),
    }
}

/// A real [`LlmHandler`] wrapped to record every generation it fulfils.
pub struct RecordingLlmHandler {
    inner: Arc<dyn LlmHandler>,
    state: Arc<RecorderState>,
    redactor: Arc<dyn Redactor>,
}

#[async_trait]
impl LlmHandler for RecordingLlmHandler {
    async fn fulfill(
        &self,
        request: &ChatRequest,
        mode: LlmStepMode,
        ctx: &RunContext,
    ) -> RequirementResult {
        let result = self.inner.fulfill(request, mode, ctx).await;
        if let RequirementResult::Llm(outcome) = &result {
            let mut recorded_request = request.clone();
            self.redactor.redact_chat_request(&mut recorded_request);
            let recorded = match outcome {
                Ok(response) => {
                    let mut recorded_response = response.clone();
                    self.redactor.redact_response(&mut recorded_response);
                    LlmOutcome::Ok(recorded_response)
                }
                Err(error) => LlmOutcome::Err(error.clone()),
            };
            self.state
                .record(|index| LlmEntry::new(index, recorded_request, mode, recorded).into());
        }
        result
    }
}

/// A real [`ToolHandler`] wrapped to record every execution it fulfils.
pub struct RecordingToolHandler {
    inner: Arc<dyn ToolHandler>,
    state: Arc<RecorderState>,
    redactor: Arc<dyn Redactor>,
}

#[async_trait]
impl ToolHandler for RecordingToolHandler {
    async fn fulfill(
        &self,
        call_id: ToolCallId,
        call: &ToolCall,
        ctx: &RunContext,
    ) -> RequirementResult {
        let result = self.inner.fulfill(call_id, call, ctx).await;
        if let RequirementResult::Tool(outcome) = &result {
            let mut recorded_call = call.clone();
            self.redactor.redact_tool_call(&mut recorded_call);
            let recorded = match outcome {
                Ok(response) => {
                    let mut recorded_response = response.clone();
                    self.redactor.redact_tool_response(&mut recorded_response);
                    ToolOutcome::Ok(recorded_response)
                }
                Err(error) => ToolOutcome::Err(error.into()),
            };
            self.state
                .record(|index| ToolEntry::new(index, recorded_call, recorded).into());
        }
        result
    }
}

/// A real [`InteractionHandler`] wrapped to record every interaction it
/// resolves.
pub struct RecordingInteractionHandler {
    inner: Arc<dyn InteractionHandler>,
    state: Arc<RecorderState>,
    redactor: Arc<dyn Redactor>,
}

#[async_trait]
impl InteractionHandler for RecordingInteractionHandler {
    async fn fulfill(&self, request: &Interaction, ctx: &RunContext) -> RequirementResult {
        let result = self.inner.fulfill(request, ctx).await;
        if let RequirementResult::Interaction(response) = &result {
            let mut recorded_request = request.clone();
            self.redactor.redact_interaction(&mut recorded_request);
            let mut recorded_response = response.clone();
            self.redactor
                .redact_interaction_response(&mut recorded_response);
            self.state.record(|index| {
                InteractionEntry::new(index, recorded_request, recorded_response).into()
            });
        }
        result
    }
}

/// A real [`ReconfigHandler`] wrapped to record every reconfiguration it
/// resolves.
pub struct RecordingReconfigHandler {
    inner: Arc<dyn ReconfigHandler>,
    state: Arc<RecorderState>,
    redactor: Arc<dyn Redactor>,
}

#[async_trait]
impl ReconfigHandler for RecordingReconfigHandler {
    async fn fulfill(&self, tool_set: &ToolSetRef, ctx: &RunContext) -> RequirementResult {
        let result = self.inner.fulfill(tool_set, ctx).await;
        if let RequirementResult::Reconfig(outcome) = &result {
            let mut recorded_tool_set = tool_set.clone();
            self.redactor.redact_tool_set(&mut recorded_tool_set);
            let recorded = match outcome {
                Ok(()) => ReconfigOutcome::Ok,
                Err(error) => ReconfigOutcome::Err(error.into()),
            };
            self.state
                .record(|index| ReconfigEntry::new(index, recorded_tool_set, recorded).into());
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CassetteRecorder, RecorderError, RecorderMode, RecorderReport, UPDATE_ENV_VAR, write_atomic,
    };
    use crate::cassette::{
        Cassette, CassetteEntry, CassetteMetadata, DefaultRedactor, LlmEntry, LlmOutcome,
        REDACTED_PLACEHOLDER,
    };
    use crate::fixtures::{assistant_text, root_context, tool_call, tool_ok, usage, weather_tool};
    use crate::ids::SeqIds;
    use agent_lib::agent::{
        Interaction, InteractionHandler, InteractionResponse, LlmHandler, LlmStepMode,
        ReconfigHandler, RequirementResult, RunContext, StepId, ToolHandler, ToolSetId, ToolSetRef,
    };
    use agent_lib::client::{ChatRequest, Response};
    use agent_lib::conversation::ToolCallId;
    use agent_lib::model::extras::{ProviderExtras, ProviderId};
    use agent_lib::model::message::Message;
    use agent_lib::model::tool::ToolCall;
    use async_trait::async_trait;
    use serde_json::{Map, json};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    /// A self-deleting unique path under the OS temp dir.
    struct TempPath(PathBuf);

    impl TempPath {
        fn new(tag: &str) -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let unique = format!(
                "{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            );
            Self(std::env::temp_dir().join(format!("agent-testkit-{tag}-{unique}.json")))
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempPath {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    // ----- fake "real" handlers returning a fixed result -----

    struct FixedLlm(RequirementResult);

    #[async_trait]
    impl LlmHandler for FixedLlm {
        async fn fulfill(
            &self,
            _request: &ChatRequest,
            _mode: LlmStepMode,
            _ctx: &RunContext,
        ) -> RequirementResult {
            self.0.clone()
        }
    }

    struct FixedTool(RequirementResult);

    #[async_trait]
    impl ToolHandler for FixedTool {
        async fn fulfill(
            &self,
            _call_id: ToolCallId,
            _call: &ToolCall,
            _ctx: &RunContext,
        ) -> RequirementResult {
            self.0.clone()
        }
    }

    struct FixedInteraction(RequirementResult);

    #[async_trait]
    impl InteractionHandler for FixedInteraction {
        async fn fulfill(&self, _request: &Interaction, _ctx: &RunContext) -> RequirementResult {
            self.0.clone()
        }
    }

    struct FixedReconfig(RequirementResult);

    #[async_trait]
    impl ReconfigHandler for FixedReconfig {
        async fn fulfill(&self, _tool_set: &ToolSetRef, _ctx: &RunContext) -> RequirementResult {
            self.0.clone()
        }
    }

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

    fn provider_extras() -> ProviderExtras {
        let mut fields = Map::new();
        fields.insert("secret".to_owned(), json!("api-key-should-be-hidden"));
        fields.insert("keep".to_owned(), json!("model-preference"));
        ProviderExtras {
            provider: ProviderId::Anthropic,
            fields,
        }
    }

    fn response_with_extra(text: &str) -> Response {
        let mut response = assistant_text(text, usage(2, 1));
        response
            .extra
            .insert("secret".to_owned(), json!("response-token"));
        response
            .extra
            .insert("keep".to_owned(), json!("public-note"));
        response
    }

    /// A record/update recorder is disabled unless its env opt-in is set. The
    /// suite never sets the env var, so the default is deterministic.
    #[test]
    fn update_is_disabled_without_env_var() {
        let temp = TempPath::new("update-disabled");
        let recorder = CassetteRecorder::update(temp.path());
        assert!(
            !recorder.is_enabled(),
            "update must be gated by an env opt-in"
        );
        let reason = recorder
            .skip_reason()
            .expect("update carries a skip reason");
        assert!(
            reason.contains(UPDATE_ENV_VAR),
            "skip reason names the env var: {reason}"
        );
    }

    /// Finishing a disabled writing mode returns `Skipped` and writes no file.
    #[tokio::test]
    async fn update_without_env_var_writes_no_file() {
        let ids = SeqIds::new();
        let ctx = root_context(&ids);
        let temp = TempPath::new("update-no-write");

        let recorder = CassetteRecorder::update(temp.path());
        let tool = recorder.wrap_tool(FixedTool(RequirementResult::Tool(Ok(tool_ok(
            "call-1", "sunny",
        )))));
        let call = tool_call("call-1", "get_weather", json!({ "city": "SH" }));
        let _ = tool.fulfill(ids.tool_call_id(), &call, &ctx).await;

        let report = recorder
            .finish()
            .expect("finish must not error when skipped");
        assert!(
            matches!(
                report,
                RecorderReport::Skipped {
                    mode: RecorderMode::Update,
                    ..
                }
            ),
            "a disabled update must be skipped, got {report:?}"
        );
        assert!(
            !temp.path().exists(),
            "a disabled update must not write the cassette file"
        );
    }

    /// Record through the redactor writes a stable, review-friendly JSON file
    /// with un-allowlisted provider extras scrubbed.
    #[tokio::test]
    async fn record_writes_stable_redacted_json() {
        let ids = SeqIds::new();
        let ctx = root_context(&ids);
        let temp = TempPath::new("record-stable");

        let recorder = CassetteRecorder::record(temp.path())
            .with_enabled_override(true)
            .with_metadata(CassetteMetadata::new("record_writes_stable_redacted_json"))
            .with_redactor(DefaultRedactor::new().allow_field("keep"));

        let llm = recorder.wrap_llm(FixedLlm(RequirementResult::Llm(Ok(response_with_extra(
            "sunny",
        )))));
        let request = chat_request(vec![], Some(provider_extras()));
        let _ = llm.fulfill(&request, LlmStepMode::NonStreaming, &ctx).await;

        let report = recorder.finish().expect("record must write");
        let RecorderReport::Wrote { entry_count, .. } = report else {
            panic!("record must report a write, got {report:?}");
        };
        assert_eq!(entry_count, 1);

        let content = std::fs::read_to_string(temp.path()).expect("cassette file exists");
        let cassette = Cassette::from_json_str(&content).expect("cassette parses");
        assert_eq!(cassette.entries.len(), 1);
        let CassetteEntry::Llm(entry) = &cassette.entries[0] else {
            panic!("recorded entry must be an LLM entry");
        };

        // Request provider extras: `secret` scrubbed, allowlisted `keep` kept.
        let extras = entry
            .request
            .provider_extras
            .as_ref()
            .expect("recorded request keeps its extras shape");
        assert_eq!(extras.fields["secret"], json!(REDACTED_PLACEHOLDER));
        assert_eq!(extras.fields["keep"], json!("model-preference"));

        // Response extras redacted the same way.
        let LlmOutcome::Ok(response) = &entry.result else {
            panic!("recorded result must be Ok");
        };
        assert_eq!(response.extra["secret"], json!(REDACTED_PLACEHOLDER));
        assert_eq!(response.extra["keep"], json!("public-note"));

        // JSON is stable: re-serializing the parsed cassette reproduces the file.
        let reserialized = cassette
            .to_json_string_pretty()
            .expect("cassette serializes");
        assert_eq!(reserialized, content, "written cassette JSON is stable");
    }

    fn write_baseline_cassette(path: &Path, request: &ChatRequest, text: &str) {
        let cassette = Cassette::new(CassetteMetadata::new("baseline")).with_entry(LlmEntry::new(
            0,
            request.clone(),
            LlmStepMode::NonStreaming,
            LlmOutcome::Ok(assistant_text(text, usage(2, 1))),
        ));
        let json = cassette
            .to_json_string_pretty()
            .expect("serialize baseline");
        write_atomic(path, json.as_bytes()).expect("write baseline");
    }

    /// Verify mode reports a result drift when the live handler returns a
    /// different result than the recorded one.
    #[tokio::test]
    async fn verify_detects_result_drift() {
        let ids = SeqIds::new();
        let ctx = root_context(&ids);
        let temp = TempPath::new("verify-drift");
        let request = chat_request(vec![], None);
        write_baseline_cassette(temp.path(), &request, "sunny");

        let recorder = CassetteRecorder::verify(temp.path());
        let llm = recorder.wrap_llm(FixedLlm(RequirementResult::Llm(Ok(assistant_text(
            "rainy",
            usage(2, 1),
        )))));
        let _ = llm.fulfill(&request, LlmStepMode::NonStreaming, &ctx).await;

        let error = recorder.finish().expect_err("verify must detect drift");
        let RecorderError::Drift(drifts) = error else {
            panic!("expected a drift error, got {error:?}");
        };
        assert_eq!(drifts.len(), 1, "one entry drifted");
        assert_eq!(drifts[0].position(), 0);
        assert!(
            drifts[0].detail().contains("result drift"),
            "drift names the result divergence: {}",
            drifts[0].detail()
        );
    }

    /// Verify mode passes silently when the live result matches the cassette.
    #[tokio::test]
    async fn verify_passes_when_results_match() {
        let ids = SeqIds::new();
        let ctx = root_context(&ids);
        let temp = TempPath::new("verify-match");
        let request = chat_request(vec![], None);
        write_baseline_cassette(temp.path(), &request, "sunny");

        let recorder = CassetteRecorder::verify(temp.path());
        let llm = recorder.wrap_llm(FixedLlm(RequirementResult::Llm(Ok(assistant_text(
            "sunny",
            usage(2, 1),
        )))));
        let _ = llm.fulfill(&request, LlmStepMode::NonStreaming, &ctx).await;

        let report = recorder.finish().expect("verify must pass");
        assert_eq!(report, RecorderReport::Verified { entry_count: 1 });
    }

    /// Update overwrites (does not append to) an existing cassette.
    #[tokio::test]
    async fn update_overwrites_existing_cassette() {
        let ids = SeqIds::new();
        let ctx = root_context(&ids);
        let temp = TempPath::new("update-overwrite");
        let old_request = chat_request(vec![], None);
        write_baseline_cassette(temp.path(), &old_request, "stale");

        let recorder = CassetteRecorder::update(temp.path()).with_enabled_override(true);
        let llm = recorder.wrap_llm(FixedLlm(RequirementResult::Llm(Ok(assistant_text(
            "fresh",
            usage(2, 1),
        )))));
        let _ = llm
            .fulfill(&old_request, LlmStepMode::NonStreaming, &ctx)
            .await;

        let report = recorder.finish().expect("update writes");
        assert!(matches!(
            report,
            RecorderReport::Wrote { entry_count: 1, .. }
        ));

        let content = std::fs::read_to_string(temp.path()).expect("file exists");
        let cassette = Cassette::from_json_str(&content).expect("parses");
        assert_eq!(cassette.entries.len(), 1, "update replaced, not appended");
        let CassetteEntry::Llm(entry) = &cassette.entries[0] else {
            panic!("entry is llm");
        };
        let LlmOutcome::Ok(response) = &entry.result else {
            panic!("ok result");
        };
        assert_eq!(
            response.message.content,
            vec![crate::fixtures::text_block("fresh")]
        );
    }

    /// A recorder shares one accumulator across families, capturing a mixed turn
    /// in global dispatch order.
    #[tokio::test]
    async fn records_all_families_in_dispatch_order() {
        let ids = SeqIds::new();
        let ctx = root_context(&ids);
        let temp = TempPath::new("all-families");

        let recorder = CassetteRecorder::record(temp.path()).with_enabled_override(true);
        let llm = recorder.wrap_llm(FixedLlm(RequirementResult::Llm(Ok(assistant_text(
            "hi",
            usage(1, 1),
        )))));
        let tool = recorder.wrap_tool(FixedTool(RequirementResult::Tool(Ok(tool_ok(
            "call-1", "sunny",
        )))));
        let interaction = recorder.wrap_interaction(FixedInteraction(
            RequirementResult::Interaction(InteractionResponse::Answer("yes".to_owned())),
        ));
        let reconfig = recorder.wrap_reconfig(FixedReconfig(RequirementResult::Reconfig(Ok(()))));

        let _ = llm
            .fulfill(&chat_request(vec![], None), LlmStepMode::NonStreaming, &ctx)
            .await;
        let call = tool_call("call-1", "get_weather", json!({ "city": "SH" }));
        let _ = tool.fulfill(ids.tool_call_id(), &call, &ctx).await;
        let _ = interaction
            .fulfill(
                &Interaction::question(step_id(), "approve?".to_owned()),
                &ctx,
            )
            .await;
        let _ = reconfig
            .fulfill(&ToolSetRef::new(tool_set_id(), vec![weather_tool()]), &ctx)
            .await;

        let cassette = recorder.build_cassette();
        let families: Vec<_> = cassette
            .entries
            .iter()
            .map(|entry| match entry {
                CassetteEntry::Llm(entry) => ("llm", entry.index),
                CassetteEntry::Tool(entry) => ("tool", entry.index),
                CassetteEntry::Interaction(entry) => ("interaction", entry.index),
                CassetteEntry::Reconfig(entry) => ("reconfig", entry.index),
            })
            .collect();
        assert_eq!(
            families,
            vec![("llm", 0), ("tool", 1), ("interaction", 2), ("reconfig", 3)]
        );
    }
}
