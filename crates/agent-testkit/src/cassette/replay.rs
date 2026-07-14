//! Offline replay handlers that serve a recorded [`Cassette`] back through the
//! four effect handler traits.
//!
//! Replay mode never calls a real backend: each handler owns the recorded
//! entries of *one* effect family and returns them in dispatch order, matching
//! every incoming request by its [`request fingerprint`](super::request_fingerprint)
//! (which ignores volatile per-run ids). A [`CassettePlayer`] builds all four
//! handlers from one cassette so a whole turn can run against
//! [`CassetteLlmHandler`], [`CassetteToolHandler`],
//! [`CassetteInteractionHandler`], and [`CassetteReconfigHandler`] with no
//! network, credentials, or live tool backend.
//!
//! # Mismatch handling
//!
//! When a live request does not match the next recorded entry — a diverging
//! fingerprint or a drained family — the handler raises a [`ReplayMismatch`]
//! carrying the cassette label, entry index, family, expected/actual
//! fingerprints, and a request summary. Following the kit's family-alignment
//! rule, the three families that carry an error channel fold the mismatch into a
//! family-aligned failure ([`RequirementResult::Llm(Err(..))`](RequirementResult::Llm),
//! [`Tool(Err(..))`](RequirementResult::Tool),
//! [`Reconfig(Err(..))`](RequirementResult::Reconfig)); the interaction family,
//! whose [`InteractionResponse`](agent_lib::agent::InteractionResponse) has no
//! error variant, cannot represent the
//! failure in-band and therefore panics with the same message.
//!
//! Every handler records its calls in the same observable [`CallLog`] the
//! scripted handlers use, so a test can assert on the replayed traffic.

use std::sync::{Arc, Mutex};

use agent_lib::agent::{
    Interaction, InteractionHandler, LlmHandler, LlmStepMode, ReconfigHandler, RequirementKindTag,
    RequirementResult, RunContext, ToolHandler, ToolRuntimeError, ToolSetRef,
};
use agent_lib::client::{ChatRequest, ClientError};
use agent_lib::conversation::ToolCallId;
use agent_lib::model::tool::ToolCall;
use async_trait::async_trait;
use std::fmt;

use crate::handlers::{InteractionCallLog, LlmCallLog, ReconfigCallLog, ToolCallLog};
use crate::script::CallLog;

use super::{
    Cassette, CassetteEntry, InteractionEntry, LlmEntry, LlmOutcome, ReconfigEntry,
    ReconfigOutcome, ToolEntry, ToolOutcome, request_fingerprint,
};

/// Why a replay handler could not serve a request from its cassette.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplayMismatchKind {
    /// The next recorded entry's fingerprint did not match the live request.
    Fingerprint,
    /// The cassette held no further entry of this family to serve.
    Exhausted,
}

impl fmt::Display for ReplayMismatchKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fingerprint => formatter.write_str("fingerprint mismatch"),
            Self::Exhausted => formatter.write_str("exhausted"),
        }
    }
}

/// A classified failure raised when a replay handler cannot serve a request.
///
/// It is the "clear error" the replay contract promises: a self-describing
/// value naming the cassette, the family, the recorded [`entry index`](Self::entry_index),
/// the [`expected`](Self::expected_fingerprint) and [`actual`](Self::actual_fingerprint)
/// fingerprints, and a [`request summary`](Self::request_summary). Its
/// [`Display`](std::fmt::Display)
/// renders every field on one line so a folded error string stays greppable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplayMismatch {
    kind: ReplayMismatchKind,
    label: Arc<str>,
    family: RequirementKindTag,
    entry_index: Option<usize>,
    family_position: usize,
    recorded_len: usize,
    expected_fingerprint: Option<String>,
    actual_fingerprint: String,
    request_summary: String,
}

impl ReplayMismatch {
    /// Returns whether this was a fingerprint divergence or a drained family.
    #[must_use]
    pub const fn kind(&self) -> ReplayMismatchKind {
        self.kind
    }

    /// Returns the cassette path/label the replay handler was built with.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Returns the effect family that could not be served.
    #[must_use]
    pub const fn family(&self) -> RequirementKindTag {
        self.family
    }

    /// Returns the dispatch index of the expected entry, or `None` when the
    /// family was already drained.
    #[must_use]
    pub const fn entry_index(&self) -> Option<usize> {
        self.entry_index
    }

    /// Returns the recorded fingerprint of the expected entry, or `None` when
    /// the family was already drained.
    #[must_use]
    pub fn expected_fingerprint(&self) -> Option<&str> {
        self.expected_fingerprint.as_deref()
    }

    /// Returns the fingerprint computed for the live request.
    #[must_use]
    pub fn actual_fingerprint(&self) -> &str {
        &self.actual_fingerprint
    }

    /// Returns the human-readable summary of the live request.
    #[must_use]
    pub fn request_summary(&self) -> &str {
        &self.request_summary
    }
}

impl fmt::Display for ReplayMismatch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "cassette `{}` replay {} for the {} effect: request #{} (0-based)",
            self.label, self.kind, self.family, self.family_position
        )?;
        match self.kind {
            ReplayMismatchKind::Fingerprint => write!(
                formatter,
                " does not match recorded entry #{} — expected fingerprint `{}`, actual fingerprint `{}`",
                self.entry_index.unwrap_or(self.family_position),
                self.expected_fingerprint.as_deref().unwrap_or_default(),
                self.actual_fingerprint,
            )?,
            ReplayMismatchKind::Exhausted => write!(
                formatter,
                " has no recorded entry (cassette defined {} {} entr{}) — actual fingerprint `{}`",
                self.recorded_len,
                self.family,
                if self.recorded_len == 1 { "y" } else { "ies" },
                self.actual_fingerprint,
            )?,
        }
        write!(formatter, "; request: {}", self.request_summary)
    }
}

impl std::error::Error for ReplayMismatch {}

/// Builds all four replay handlers from one recorded [`Cassette`].
///
/// The player holds the cassette so a test constructs a cohesive set of handlers
/// — one per effect family — that each replay their own entries in dispatch
/// order. Every produced handler is independent (it owns its own cursor and call
/// log), so building a second handler of a family restarts that family's replay.
#[derive(Clone, Debug)]
pub struct CassettePlayer {
    cassette: Arc<Cassette>,
    label: Arc<str>,
}

impl CassettePlayer {
    /// Builds a player over `cassette`, labelled by its source path/name.
    #[must_use]
    pub fn new(cassette: Cassette, label: impl Into<Arc<str>>) -> Self {
        Self {
            cassette: Arc::new(cassette),
            label: label.into(),
        }
    }

    /// Builds a player over a shared `cassette`.
    #[must_use]
    pub fn from_arc(cassette: Arc<Cassette>, label: impl Into<Arc<str>>) -> Self {
        Self {
            cassette,
            label: label.into(),
        }
    }

    /// Returns the cassette this player replays.
    #[must_use]
    pub fn cassette(&self) -> &Cassette {
        &self.cassette
    }

    /// Returns the label carried into every [`ReplayMismatch`].
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Builds a replay handler for the LLM family.
    #[must_use]
    pub fn llm_handler(&self) -> CassetteLlmHandler {
        CassetteLlmHandler::from_cassette(&self.cassette, self.label.clone())
    }

    /// Builds a replay handler for the tool family.
    #[must_use]
    pub fn tool_handler(&self) -> CassetteToolHandler {
        CassetteToolHandler::from_cassette(&self.cassette, self.label.clone())
    }

    /// Builds a replay handler for the interaction family.
    #[must_use]
    pub fn interaction_handler(&self) -> CassetteInteractionHandler {
        CassetteInteractionHandler::from_cassette(&self.cassette, self.label.clone())
    }

    /// Builds a replay handler for the reconfiguration family.
    #[must_use]
    pub fn reconfig_handler(&self) -> CassetteReconfigHandler {
        CassetteReconfigHandler::from_cassette(&self.cassette, self.label.clone())
    }
}

/// A per-family replay cursor over an ordered list of recorded entries.
///
/// It matches the next entry against a live request's fingerprint, advancing
/// only on a match so a diverging request is reported against the same expected
/// entry every time.
struct ReplayCursor<E> {
    entries: Vec<E>,
    label: Arc<str>,
    family: RequirementKindTag,
    next: Mutex<usize>,
}

impl<E> ReplayCursor<E> {
    fn new(entries: Vec<E>, label: Arc<str>, family: RequirementKindTag) -> Self {
        Self {
            entries,
            label,
            family,
            next: Mutex::new(0),
        }
    }

    /// Matches `actual_fingerprint` against the next entry, returning the matched
    /// entry's recorded fingerprint holder or a classified [`ReplayMismatch`].
    ///
    /// `index_of` and `fingerprint_of` project the entry's dispatch index and
    /// recorded fingerprint so this cursor stays generic over the four entry
    /// types.
    fn advance<'a>(
        &'a self,
        actual_fingerprint: &str,
        request_summary: impl FnOnce() -> String,
        index_of: impl Fn(&E) -> usize,
        fingerprint_of: impl Fn(&E) -> &str,
    ) -> Result<&'a E, Box<ReplayMismatch>> {
        let mut next = self.next.lock().expect("replay cursor mutex poisoned");
        let position = *next;
        match self.entries.get(position) {
            None => Err(Box::new(ReplayMismatch {
                kind: ReplayMismatchKind::Exhausted,
                label: self.label.clone(),
                family: self.family,
                entry_index: None,
                family_position: position,
                recorded_len: self.entries.len(),
                expected_fingerprint: None,
                actual_fingerprint: actual_fingerprint.to_owned(),
                request_summary: request_summary(),
            })),
            Some(entry) if fingerprint_of(entry) == actual_fingerprint => {
                *next = position + 1;
                Ok(entry)
            }
            Some(entry) => Err(Box::new(ReplayMismatch {
                kind: ReplayMismatchKind::Fingerprint,
                label: self.label.clone(),
                family: self.family,
                entry_index: Some(index_of(entry)),
                family_position: position,
                recorded_len: self.entries.len(),
                expected_fingerprint: Some(fingerprint_of(entry).to_owned()),
                actual_fingerprint: actual_fingerprint.to_owned(),
                request_summary: request_summary(),
            })),
        }
    }
}

/// Collects the entries of one family from `cassette` in dispatch order.
fn family_entries<E>(cassette: &Cassette, select: impl Fn(&CassetteEntry) -> Option<&E>) -> Vec<E>
where
    E: Clone,
{
    cassette
        .entries
        .iter()
        .filter_map(select)
        .cloned()
        .collect()
}

/// Replays the recorded LLM generations of a [`Cassette`].
pub struct CassetteLlmHandler {
    cursor: ReplayCursor<LlmEntry>,
    log: Arc<LlmCallLog>,
}

impl CassetteLlmHandler {
    /// Builds a handler over the LLM entries of `cassette`.
    #[must_use]
    pub fn from_cassette(cassette: &Cassette, label: impl Into<Arc<str>>) -> Self {
        let entries = family_entries(cassette, |entry| match entry {
            CassetteEntry::Llm(entry) => Some(entry),
            _ => None,
        });
        Self {
            cursor: ReplayCursor::new(entries, label.into(), RequirementKindTag::Llm),
            log: Arc::new(CallLog::new()),
        }
    }

    /// Returns the shared call log recording every replayed generation.
    #[must_use]
    pub fn log(&self) -> &Arc<LlmCallLog> {
        &self.log
    }
}

#[async_trait]
impl LlmHandler for CassetteLlmHandler {
    async fn fulfill(
        &self,
        request: &ChatRequest,
        _mode: LlmStepMode,
        _ctx: &RunContext,
    ) -> RequirementResult {
        let ticket = self.log.begin(request.clone());
        let fingerprint = request_fingerprint(request);
        let result = match self.cursor.advance(
            &fingerprint,
            || chat_request_summary(request),
            |entry| entry.index,
            |entry| &entry.fingerprint,
        ) {
            Ok(entry) => llm_result(entry.result.clone()),
            Err(mismatch) => RequirementResult::Llm(Err(ClientError::Other(mismatch.to_string()))),
        };
        self.log.complete(ticket, result.clone());
        result
    }
}

/// Replays the recorded tool executions of a [`Cassette`].
pub struct CassetteToolHandler {
    cursor: ReplayCursor<ToolEntry>,
    log: Arc<ToolCallLog>,
}

impl CassetteToolHandler {
    /// Builds a handler over the tool entries of `cassette`.
    #[must_use]
    pub fn from_cassette(cassette: &Cassette, label: impl Into<Arc<str>>) -> Self {
        let entries = family_entries(cassette, |entry| match entry {
            CassetteEntry::Tool(entry) => Some(entry),
            _ => None,
        });
        Self {
            cursor: ReplayCursor::new(entries, label.into(), RequirementKindTag::Tool),
            log: Arc::new(CallLog::new()),
        }
    }

    /// Returns the shared call log recording every replayed execution.
    #[must_use]
    pub fn log(&self) -> &Arc<ToolCallLog> {
        &self.log
    }
}

#[async_trait]
impl ToolHandler for CassetteToolHandler {
    async fn fulfill(
        &self,
        _call_id: ToolCallId,
        call: &ToolCall,
        _ctx: &RunContext,
    ) -> RequirementResult {
        let ticket = self.log.begin(call.clone());
        let fingerprint = request_fingerprint(call);
        let result = match self.cursor.advance(
            &fingerprint,
            || tool_call_summary(call),
            |entry| entry.index,
            |entry| &entry.fingerprint,
        ) {
            Ok(entry) => tool_result(entry.result.clone()),
            Err(mismatch) => RequirementResult::Tool(Err(ToolRuntimeError::ExecutionFailed {
                tool_name: call.name.clone(),
                message: mismatch.to_string(),
            })),
        };
        self.log.complete(ticket, result.clone());
        result
    }
}

/// Replays the recorded interactions of a [`Cassette`].
///
/// The interaction family's [`InteractionResponse`](agent_lib::agent::InteractionResponse)
/// has no error variant, so a
/// [`ReplayMismatch`] cannot be folded back in-band: this handler **panics** with
/// the mismatch message instead, failing the test loudly.
pub struct CassetteInteractionHandler {
    cursor: ReplayCursor<InteractionEntry>,
    log: Arc<InteractionCallLog>,
}

impl CassetteInteractionHandler {
    /// Builds a handler over the interaction entries of `cassette`.
    #[must_use]
    pub fn from_cassette(cassette: &Cassette, label: impl Into<Arc<str>>) -> Self {
        let entries = family_entries(cassette, |entry| match entry {
            CassetteEntry::Interaction(entry) => Some(entry),
            _ => None,
        });
        Self {
            cursor: ReplayCursor::new(entries, label.into(), RequirementKindTag::Interaction),
            log: Arc::new(CallLog::new()),
        }
    }

    /// Returns the shared call log recording every replayed interaction.
    #[must_use]
    pub fn log(&self) -> &Arc<InteractionCallLog> {
        &self.log
    }
}

#[async_trait]
impl InteractionHandler for CassetteInteractionHandler {
    async fn fulfill(&self, request: &Interaction, _ctx: &RunContext) -> RequirementResult {
        let fingerprint = request_fingerprint(request);
        let response = match self.cursor.advance(
            &fingerprint,
            || interaction_summary(request),
            |entry| entry.index,
            |entry| &entry.fingerprint,
        ) {
            Ok(entry) => entry.result.clone(),
            Err(mismatch) => panic!("{mismatch}"),
        };
        self.log.record(request.clone(), response.clone());
        RequirementResult::Interaction(response)
    }
}

/// Replays the recorded tool-set reconfigurations of a [`Cassette`].
pub struct CassetteReconfigHandler {
    cursor: ReplayCursor<ReconfigEntry>,
    log: Arc<ReconfigCallLog>,
}

impl CassetteReconfigHandler {
    /// Builds a handler over the reconfiguration entries of `cassette`.
    #[must_use]
    pub fn from_cassette(cassette: &Cassette, label: impl Into<Arc<str>>) -> Self {
        let entries = family_entries(cassette, |entry| match entry {
            CassetteEntry::Reconfig(entry) => Some(entry),
            _ => None,
        });
        Self {
            cursor: ReplayCursor::new(entries, label.into(), RequirementKindTag::Reconfig),
            log: Arc::new(CallLog::new()),
        }
    }

    /// Returns the shared call log recording every replayed reconfiguration.
    #[must_use]
    pub fn log(&self) -> &Arc<ReconfigCallLog> {
        &self.log
    }
}

#[async_trait]
impl ReconfigHandler for CassetteReconfigHandler {
    async fn fulfill(&self, tool_set: &ToolSetRef, _ctx: &RunContext) -> RequirementResult {
        let ticket = self.log.begin(tool_set.clone());
        let fingerprint = request_fingerprint(tool_set);
        let result = match self.cursor.advance(
            &fingerprint,
            || tool_set_summary(tool_set),
            |entry| entry.index,
            |entry| &entry.fingerprint,
        ) {
            Ok(entry) => reconfig_result(entry.result.clone()),
            Err(mismatch) => RequirementResult::Reconfig(Err(ToolRuntimeError::InvalidRegistry {
                message: mismatch.to_string(),
            })),
        };
        self.log.complete(ticket, result.clone());
        result
    }
}

// ----- outcome -> result folding -----

/// Folds a recorded [`LlmOutcome`] into its family-aligned result.
fn llm_result(outcome: LlmOutcome) -> RequirementResult {
    match outcome {
        LlmOutcome::Ok(response) => RequirementResult::Llm(Ok(response)),
        LlmOutcome::Err(error) => RequirementResult::Llm(Err(error)),
    }
}

/// Folds a recorded [`ToolOutcome`] into its family-aligned result.
fn tool_result(outcome: ToolOutcome) -> RequirementResult {
    match outcome {
        ToolOutcome::Ok(response) => RequirementResult::Tool(Ok(response)),
        ToolOutcome::Err(error) => RequirementResult::Tool(Err(error.into())),
    }
}

/// Folds a recorded [`ReconfigOutcome`] into its family-aligned result.
fn reconfig_result(outcome: ReconfigOutcome) -> RequirementResult {
    match outcome {
        ReconfigOutcome::Ok => RequirementResult::Reconfig(Ok(())),
        ReconfigOutcome::Err(error) => RequirementResult::Reconfig(Err(error.into())),
    }
}

// ----- request summaries -----

/// Summarizes an LLM request for a [`ReplayMismatch`].
fn chat_request_summary(request: &ChatRequest) -> String {
    format!(
        "model `{}`, {} message(s), {} tool(s), stream={}",
        request.model,
        request.messages.len(),
        request.tools.len(),
        request.stream
    )
}

/// Summarizes a tool call for a [`ReplayMismatch`].
fn tool_call_summary(call: &ToolCall) -> String {
    format!("tool `{}` (provider call id `{}`)", call.name, call.id)
}

/// Summarizes an interaction request for a [`ReplayMismatch`].
fn interaction_summary(request: &Interaction) -> String {
    format!(
        "{} interaction for step `{}`",
        request.kind().tag(),
        request.step_id()
    )
}

/// Summarizes a tool-set reference for a [`ReplayMismatch`].
fn tool_set_summary(tool_set: &ToolSetRef) -> String {
    format!(
        "tool set `{}` with {} declared tool(s)",
        tool_set.id(),
        tool_set.tools().len()
    )
}

#[cfg(test)]
mod tests {
    use super::{CassettePlayer, ReplayMismatchKind};
    use crate::cassette::{
        Cassette, CassetteMetadata, CassetteToolError, InteractionEntry, LlmEntry, LlmOutcome,
        ReconfigEntry, ReconfigOutcome, ToolEntry, ToolOutcome,
    };
    use crate::fixtures::{
        assistant_text, assistant_tool_use, root_context, tool_call, tool_ok, usage, user_message,
        weather_tool,
    };
    use crate::ids::SeqIds;
    use agent_lib::agent::{
        Interaction, InteractionHandler, InteractionResponse, LlmHandler, LlmStepMode,
        ReconfigHandler, RequirementResult, StepId, ToolHandler, ToolRuntimeError, ToolSetId,
        ToolSetRef,
    };
    use agent_lib::client::{ChatRequest, ClientError};
    use agent_lib::model::content::ContentBlock;
    use agent_lib::model::message::{Message, Role};
    use agent_lib::model::tool::ToolCall;
    use serde_json::{Map, json};

    const LABEL: &str = "tests/cassettes/replay.json";

    fn step_id() -> StepId {
        StepId::parse_str("018f0d9c-7b6a-7c12-8f31-1234567890a3").expect("step id")
    }

    fn tool_set_id() -> ToolSetId {
        ToolSetId::parse_str("018f0d9c-7b6a-7c12-8f31-1234567890a4").expect("tool set id")
    }

    fn chat_request(messages: Vec<Message>) -> ChatRequest {
        ChatRequest {
            model: "test-model".to_owned(),
            messages,
            tools: vec![weather_tool()],
            system: Some("system".to_owned()),
            max_tokens: 512,
            temperature: Some(0.2),
            stream: false,
            provider_extras: None,
        }
    }

    fn assistant_tool_use_message(call: &ToolCall) -> Message {
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

    /// A cassette covering user -> LLM tool_use -> tool result -> LLM final text.
    fn roundtrip_cassette() -> Cassette {
        let call = tool_call("call-weather", "get_weather", json!({ "city": "SH" }));
        let mut cassette = Cassette::new(CassetteMetadata::new("replay_roundtrip"));
        cassette.push(LlmEntry::new(
            0,
            chat_request(vec![user_message("weather?")]),
            LlmStepMode::NonStreaming,
            LlmOutcome::Ok(assistant_tool_use(vec![call.clone()], usage(5, 2))),
        ));
        cassette.push(ToolEntry::new(
            1,
            call.clone(),
            ToolOutcome::Ok(tool_ok("call-weather", "sunny")),
        ));
        cassette.push(LlmEntry::new(
            2,
            chat_request(vec![
                user_message("weather?"),
                assistant_tool_use_message(&call),
            ]),
            LlmStepMode::NonStreaming,
            LlmOutcome::Ok(assistant_text("It is sunny.", usage(6, 4))),
        ));
        cassette
    }

    #[tokio::test]
    async fn replay_returns_recorded_results_in_order() {
        let ids = SeqIds::new();
        let ctx = root_context(&ids);
        let player = CassettePlayer::new(roundtrip_cassette(), LABEL);
        let llm = player.llm_handler();
        let tool = player.tool_handler();

        let first = llm
            .fulfill(
                &chat_request(vec![user_message("weather?")]),
                LlmStepMode::NonStreaming,
                &ctx,
            )
            .await;
        let RequirementResult::Llm(Ok(response)) = &first else {
            panic!("first generation must replay a tool-use response, got {first:?}");
        };
        assert!(matches!(
            response.message.content.first(),
            Some(ContentBlock::ToolUse { .. })
        ));

        let call = tool_call("call-weather", "get_weather", json!({ "city": "SH" }));
        let tool_result = tool.fulfill(ids.tool_call_id(), &call, &ctx).await;
        let RequirementResult::Tool(Ok(tool_response)) = &tool_result else {
            panic!("tool call must replay a recorded ok response, got {tool_result:?}");
        };
        assert_eq!(tool_response.tool_call_id, "call-weather");

        let second = llm
            .fulfill(
                &chat_request(vec![
                    user_message("weather?"),
                    assistant_tool_use_message(&call),
                ]),
                LlmStepMode::NonStreaming,
                &ctx,
            )
            .await;
        let RequirementResult::Llm(Ok(response)) = &second else {
            panic!("final generation must replay a text response, got {second:?}");
        };
        assert_eq!(
            response.message.content,
            vec![crate::fixtures::text_block("It is sunny.")]
        );

        assert_eq!(llm.log().len(), 2);
        assert_eq!(llm.log().completed_len(), 2);
        assert_eq!(tool.log().len(), 1);
    }

    #[tokio::test]
    async fn replay_matches_across_volatile_ids() {
        // The recorded request carries one provider tool-call id; the live
        // request carries a different one. Fingerprint matching ignores it.
        let ids = SeqIds::new();
        let ctx = root_context(&ids);
        let recorded_call = tool_call("call-recorded", "get_weather", json!({ "city": "SH" }));
        let live_call = tool_call("call-live", "get_weather", json!({ "city": "SH" }));

        let mut cassette = Cassette::new(CassetteMetadata::new("volatile"));
        cassette.push(LlmEntry::new(
            0,
            chat_request(vec![assistant_tool_use_message(&recorded_call)]),
            LlmStepMode::NonStreaming,
            LlmOutcome::Ok(assistant_text("ok", usage(1, 1))),
        ));
        let llm = CassettePlayer::new(cassette, LABEL).llm_handler();

        let result = llm
            .fulfill(
                &chat_request(vec![assistant_tool_use_message(&live_call)]),
                LlmStepMode::NonStreaming,
                &ctx,
            )
            .await;
        assert!(
            matches!(result, RequirementResult::Llm(Ok(_))),
            "a volatile-id-only difference must still replay, got {result:?}"
        );
    }

    #[tokio::test]
    async fn llm_request_mismatch_reports_entry_index_and_fingerprint() {
        let ids = SeqIds::new();
        let ctx = root_context(&ids);
        let mut cassette = Cassette::new(CassetteMetadata::new("mismatch"));
        cassette.push(LlmEntry::new(
            0,
            chat_request(vec![user_message("weather in SH?")]),
            LlmStepMode::NonStreaming,
            LlmOutcome::Ok(assistant_text("sunny", usage(1, 1))),
        ));
        let expected = cassette.entries[0].fingerprint().to_owned();
        let llm = CassettePlayer::new(cassette, LABEL).llm_handler();

        // A logically different request diverges in fingerprint.
        let live = chat_request(vec![user_message("weather in BJ?")]);
        let actual = crate::cassette::request_fingerprint(&live);
        let result = llm.fulfill(&live, LlmStepMode::NonStreaming, &ctx).await;

        let RequirementResult::Llm(Err(ClientError::Other(message))) = &result else {
            panic!("a fingerprint mismatch must fold into an LLM error, got {result:?}");
        };
        assert!(
            message.contains(LABEL),
            "message names the cassette: {message}"
        );
        assert!(
            message.contains("entry #0"),
            "message names the entry index: {message}"
        );
        assert!(
            message.contains(&expected),
            "message names the expected fingerprint: {message}"
        );
        assert!(
            message.contains(&actual),
            "message names the actual fingerprint: {message}"
        );
        assert_ne!(expected, actual);
    }

    #[tokio::test]
    async fn llm_exhaustion_reports_a_clear_error() {
        let ids = SeqIds::new();
        let ctx = root_context(&ids);
        let llm =
            CassettePlayer::new(Cassette::new(CassetteMetadata::new("empty")), LABEL).llm_handler();

        let result = llm
            .fulfill(
                &chat_request(vec![user_message("hello")]),
                LlmStepMode::NonStreaming,
                &ctx,
            )
            .await;
        let RequirementResult::Llm(Err(ClientError::Other(message))) = &result else {
            panic!("an exhausted replay must fold into an LLM error, got {result:?}");
        };
        assert!(message.contains("exhausted"), "message: {message}");
        assert!(message.contains("request #0"), "message: {message}");
    }

    #[tokio::test]
    async fn tool_mismatch_folds_into_execution_failed() {
        let ids = SeqIds::new();
        let ctx = root_context(&ids);
        let mut cassette = Cassette::new(CassetteMetadata::new("tool_mismatch"));
        cassette.push(ToolEntry::new(
            0,
            tool_call("call-1", "get_weather", json!({ "city": "SH" })),
            ToolOutcome::Ok(tool_ok("call-1", "sunny")),
        ));
        let tool = CassettePlayer::new(cassette, LABEL).tool_handler();

        let live = tool_call("call-1", "get_weather", json!({ "city": "BJ" }));
        let result = tool.fulfill(ids.tool_call_id(), &live, &ctx).await;
        let RequirementResult::Tool(Err(ToolRuntimeError::ExecutionFailed { tool_name, message })) =
            &result
        else {
            panic!("a tool fingerprint mismatch must fold into ExecutionFailed, got {result:?}");
        };
        assert_eq!(tool_name, "get_weather");
        assert!(message.contains("entry #0"), "message: {message}");
        assert!(message.contains("fingerprint"), "message: {message}");
    }

    #[tokio::test]
    async fn tool_replays_recorded_runtime_error() {
        let ids = SeqIds::new();
        let ctx = root_context(&ids);
        let call = tool_call("call-1", "get_weather", json!({ "city": "SH" }));
        let mut cassette = Cassette::new(CassetteMetadata::new("tool_err"));
        cassette.push(ToolEntry::new(
            0,
            call.clone(),
            ToolOutcome::Err(CassetteToolError::ExecutionFailed {
                tool_name: "get_weather".to_owned(),
                message: "backend down".to_owned(),
            }),
        ));
        let tool = CassettePlayer::new(cassette, LABEL).tool_handler();

        let result = tool.fulfill(ids.tool_call_id(), &call, &ctx).await;
        let RequirementResult::Tool(Err(ToolRuntimeError::ExecutionFailed { message, .. })) =
            &result
        else {
            panic!("recorded runtime error must replay verbatim, got {result:?}");
        };
        assert_eq!(message, "backend down");
    }

    #[tokio::test]
    async fn reconfig_replays_ok_and_folds_mismatch() {
        let ids = SeqIds::new();
        let ctx = root_context(&ids);
        let tool_set = ToolSetRef::new(tool_set_id(), vec![weather_tool()]);
        let mut cassette = Cassette::new(CassetteMetadata::new("reconfig"));
        cassette.push(ReconfigEntry::new(0, tool_set.clone(), ReconfigOutcome::Ok));
        let reconfig = CassettePlayer::new(cassette, LABEL).reconfig_handler();

        let ok = reconfig.fulfill(&tool_set, &ctx).await;
        assert!(matches!(ok, RequirementResult::Reconfig(Ok(()))));

        // A different tool set (no declared tools) diverges in fingerprint.
        let other = ToolSetRef::new(tool_set_id(), Vec::new());
        let mismatch = reconfig.fulfill(&other, &ctx).await;
        let RequirementResult::Reconfig(Err(ToolRuntimeError::InvalidRegistry { message })) =
            &mismatch
        else {
            panic!("a reconfig mismatch must fold into InvalidRegistry, got {mismatch:?}");
        };
        assert!(message.contains("exhausted"), "message: {message}");
    }

    #[tokio::test]
    async fn interaction_replays_recorded_response() {
        let ids = SeqIds::new();
        let ctx = root_context(&ids);
        let request = Interaction::question(step_id(), "approve?".to_owned());
        let mut cassette = Cassette::new(CassetteMetadata::new("interaction"));
        cassette.push(InteractionEntry::new(
            0,
            request.clone(),
            InteractionResponse::Answer("yes".to_owned()),
        ));
        let interaction = CassettePlayer::new(cassette, LABEL).interaction_handler();

        let result = interaction.fulfill(&request, &ctx).await;
        assert!(
            matches!(
                &result,
                RequirementResult::Interaction(InteractionResponse::Answer(text)) if text == "yes"
            ),
            "interaction must replay the recorded answer, got {result:?}"
        );
        assert_eq!(interaction.log().len(), 1);
    }

    #[tokio::test]
    #[should_panic(expected = "fingerprint mismatch")]
    async fn interaction_mismatch_panics_with_clear_error() {
        let ids = SeqIds::new();
        let ctx = root_context(&ids);
        let recorded = Interaction::question(step_id(), "approve deploy?".to_owned());
        let mut cassette = Cassette::new(CassetteMetadata::new("interaction_mismatch"));
        cassette.push(InteractionEntry::new(
            0,
            recorded,
            InteractionResponse::Answer("yes".to_owned()),
        ));
        let interaction = CassettePlayer::new(cassette, LABEL).interaction_handler();

        let live = Interaction::question(step_id(), "approve rollback?".to_owned());
        let _ = interaction.fulfill(&live, &ctx).await;
    }

    #[test]
    fn mismatch_kind_and_accessors_are_exposed() {
        let ids = SeqIds::new();
        let _ = &ids;
        let mut cassette = Cassette::new(CassetteMetadata::new("accessors"));
        cassette.push(LlmEntry::new(
            0,
            chat_request(vec![user_message("a")]),
            LlmStepMode::NonStreaming,
            LlmOutcome::Ok(assistant_text("x", usage(1, 1))),
        ));
        let player = CassettePlayer::new(cassette, LABEL);
        assert_eq!(player.label(), LABEL);
        assert_eq!(player.cassette().entries.len(), 1);
        // Exercise the enum's Display for both variants.
        assert_eq!(ReplayMismatchKind::Exhausted.to_string(), "exhausted");
        assert_eq!(
            ReplayMismatchKind::Fingerprint.to_string(),
            "fingerprint mismatch"
        );
    }
}
