//! The scripted effect model, strict mode, and observable call log used by the
//! scripted handlers.
//!
//! An agent-layer test expresses "the Nth call returns this" as data. This
//! module turns that intent into reusable primitives that the scripted effect
//! handlers ([`crate::handlers`], milestone M2-2) plug into:
//!
//! - [`ScriptStep`] and the per-family step types ([`LlmStep`], [`ToolStep`],
//!   [`InteractionStep`], [`ReconfigStep`]) describe one fulfilled effect as a
//!   family-aligned [`RequirementResult`] payload.
//! - [`Script`] drives a queue of steps in dispatch order and, when the queue is
//!   exhausted, applies a [`StrictMode`] to either return a classified
//!   [`ScriptError`] (the default) or panic (opt-in for panic-asserting tests).
//! - [`CallLog`] records what each handler was asked and what it returned, in
//!   both dispatch and completion order, so a test can assert on the observed
//!   traffic even when calls complete out of order (milestone M5).
//!
//! The v1 [`Script`] matches steps purely by dispatch order. The [`ScriptStep`]
//! trait already exposes a [`match_key`](ScriptStep::match_key) so tool and
//! interaction scripts can later match by key, but that path is intentionally
//! unused for now.

use std::collections::VecDeque;
use std::fmt;
use std::sync::Mutex;

use agent_lib::agent::{
    ApprovalResponse, InteractionResponse, RequirementKindTag, RequirementResult, ToolRuntimeError,
};
use agent_lib::client::{ClientError, Response};
use agent_lib::model::tool::{ToolCall, ToolResponse};
use agent_lib::model::usage::Usage;

use crate::fixtures::{assistant_text, assistant_tool_use, tool_error_response, tool_ok, usage};

/// What a [`Script`] does when it is asked for a step past the end of its queue.
///
/// The default, [`StrictMode::Error`], keeps a drained turn observable: the
/// script returns a classified [`ScriptError`] that a handler folds back into a
/// family-aligned failure. [`StrictMode::Panic`] is opt-in for tests that
/// specifically assert an over-run aborts the process.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum StrictMode {
    /// Return a classified [`ScriptError`] when the script is exhausted.
    #[default]
    Error,
    /// Panic when the script is exhausted. Opt-in for panic-asserting tests.
    Panic,
}

/// A classified failure raised while consuming a [`Script`].
///
/// The only variant today is exhaustion. Its message carries the requirement
/// family, the zero-based dispatch index of the over-running call, the number of
/// steps the script defined, and an optional cassette/scenario label, so a test
/// can assert on a stable, self-describing string.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScriptError {
    /// A step was requested after the scripted queue was drained.
    Exhausted {
        /// Requirement family whose script ran out.
        family: RequirementKindTag,
        /// Zero-based dispatch index of the call that found the script empty.
        call_index: usize,
        /// Number of steps the script was defined with.
        script_len: usize,
        /// Optional cassette/scenario label carried for diagnostics.
        label: Option<String>,
    },
}

impl fmt::Display for ScriptError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Exhausted {
                family,
                call_index,
                script_len,
                label,
            } => {
                write!(formatter, "{family} script")?;
                if let Some(label) = label {
                    write!(formatter, " `{label}`")?;
                }
                write!(
                    formatter,
                    " exhausted: requested step for call #{call_index} (0-based) but the script \
                     defined only {script_len} step(s)"
                )
            }
        }
    }
}

impl std::error::Error for ScriptError {}

/// One scripted, family-aligned effect result.
///
/// Each implementor carries the payload of exactly one
/// [`RequirementResult`] family and reports that family through
/// [`ScriptStep::FAMILY`], so a [`Script`] can name the
/// family in diagnostics even when its queue is empty.
pub trait ScriptStep: Send + Sync + 'static {
    /// The requirement family this step fulfils.
    const FAMILY: RequirementKindTag;

    /// Converts this step into its family-aligned effect result.
    fn into_result(self) -> RequirementResult;

    /// An optional order-independent match key.
    ///
    /// Reserved for future key-based matching of tool and interaction scripts;
    /// the v1 [`Script`] matches purely by dispatch order and ignores this.
    fn match_key(&self) -> Option<&str> {
        None
    }
}

/// A scripted LLM generation result (a [`RequirementResult::Llm`] payload).
#[derive(Clone, Debug)]
pub struct LlmStep {
    outcome: Result<Response, ClientError>,
}

impl LlmStep {
    /// Scripts an assistant text response that stops on `end_turn`.
    ///
    /// The response carries a zero [`Usage`]; use [`LlmStep::response`] to script
    /// a specific usage or stop reason.
    #[must_use]
    pub fn text(text: &str) -> Self {
        Self {
            outcome: Ok(assistant_text(text, usage(0, 0))),
        }
    }

    /// Scripts an assistant tool-use response that stops on `tool_use`.
    #[must_use]
    pub fn tool_use(calls: Vec<ToolCall>) -> Self {
        Self {
            outcome: Ok(assistant_tool_use(calls, usage(0, 0))),
        }
    }

    /// Scripts an explicit assistant [`Response`].
    #[must_use]
    pub fn response(response: Response) -> Self {
        Self {
            outcome: Ok(response),
        }
    }

    /// Scripts a transport-layer [`ClientError`] failure path.
    #[must_use]
    pub fn error(error: ClientError) -> Self {
        Self {
            outcome: Err(error),
        }
    }

    /// Overrides the [`Usage`] of a successful response, leaving errors intact.
    #[must_use]
    pub fn with_usage(mut self, usage: Usage) -> Self {
        if let Ok(response) = &mut self.outcome {
            response.usage = usage;
        }
        self
    }
}

impl ScriptStep for LlmStep {
    const FAMILY: RequirementKindTag = RequirementKindTag::Llm;

    fn into_result(self) -> RequirementResult {
        RequirementResult::Llm(self.outcome)
    }
}

/// A scripted tool execution result (a [`RequirementResult::Tool`] payload).
#[derive(Clone, Debug)]
pub struct ToolStep {
    outcome: Result<ToolResponse, ToolRuntimeError>,
    key: Option<String>,
}

impl ToolStep {
    /// Scripts a successful ([`ToolStatus::Ok`](agent_lib::model::tool::ToolStatus::Ok))
    /// text response for `provider_call_id`, which also becomes the match key.
    #[must_use]
    pub fn ok(provider_call_id: &str, text: &str) -> Self {
        Self {
            outcome: Ok(tool_ok(provider_call_id, text)),
            key: Some(provider_call_id.to_owned()),
        }
    }

    /// Scripts a model-visible failed
    /// ([`ToolStatus::Error`](agent_lib::model::tool::ToolStatus::Error)) text
    /// response for `provider_call_id`, which also becomes the match key.
    #[must_use]
    pub fn error(provider_call_id: &str, text: &str) -> Self {
        Self {
            outcome: Ok(tool_error_response(provider_call_id, text)),
            key: Some(provider_call_id.to_owned()),
        }
    }

    /// Scripts an explicit [`ToolResponse`], keyed by its `tool_call_id`.
    #[must_use]
    pub fn response(response: ToolResponse) -> Self {
        let key = Some(response.tool_call_id.clone());
        Self {
            outcome: Ok(response),
            key,
        }
    }

    /// Scripts a [`ToolRuntimeError`] failure path (tool machinery failing
    /// before producing a response), with no match key.
    #[must_use]
    pub fn runtime_error(error: ToolRuntimeError) -> Self {
        Self {
            outcome: Err(error),
            key: None,
        }
    }

    /// Overrides the order-independent match key (reserved; unused in v1).
    #[must_use]
    pub fn with_key(mut self, key: impl Into<String>) -> Self {
        self.key = Some(key.into());
        self
    }
}

impl ScriptStep for ToolStep {
    const FAMILY: RequirementKindTag = RequirementKindTag::Tool;

    fn into_result(self) -> RequirementResult {
        RequirementResult::Tool(self.outcome)
    }

    fn match_key(&self) -> Option<&str> {
        self.key.as_deref()
    }
}

/// A scripted interaction result (a [`RequirementResult::Interaction`] payload).
#[derive(Clone, Debug)]
pub struct InteractionStep {
    response: InteractionResponse,
    key: Option<String>,
}

impl InteractionStep {
    /// Scripts a free-form answer to a question interaction.
    #[must_use]
    pub fn answer(text: &str) -> Self {
        Self {
            response: InteractionResponse::answer(text.to_owned()),
            key: None,
        }
    }

    /// Scripts a zero-based selected index for a choice interaction.
    #[must_use]
    pub fn choice(index: usize) -> Self {
        Self {
            response: InteractionResponse::Choice(index),
            key: None,
        }
    }

    /// Scripts an [`ApprovalResponse`] for an approval interaction.
    #[must_use]
    pub fn approval(response: ApprovalResponse) -> Self {
        Self {
            response: InteractionResponse::Approval(response),
            key: None,
        }
    }

    /// Scripts an explicit [`InteractionResponse`].
    #[must_use]
    pub fn response(response: InteractionResponse) -> Self {
        Self {
            response,
            key: None,
        }
    }

    /// Overrides the order-independent match key (reserved; unused in v1).
    #[must_use]
    pub fn with_key(mut self, key: impl Into<String>) -> Self {
        self.key = Some(key.into());
        self
    }
}

impl ScriptStep for InteractionStep {
    const FAMILY: RequirementKindTag = RequirementKindTag::Interaction;

    fn into_result(self) -> RequirementResult {
        RequirementResult::Interaction(self.response)
    }

    fn match_key(&self) -> Option<&str> {
        self.key.as_deref()
    }
}

/// A scripted tool-set reconfiguration result (a
/// [`RequirementResult::Reconfig`] payload).
#[derive(Clone, Debug)]
pub struct ReconfigStep {
    outcome: Result<(), ToolRuntimeError>,
}

impl ReconfigStep {
    /// Scripts a successful registry swap confirmation.
    #[must_use]
    pub fn ok() -> Self {
        Self { outcome: Ok(()) }
    }

    /// Scripts a [`ToolRuntimeError`] resolution/validation failure path.
    #[must_use]
    pub fn error(error: ToolRuntimeError) -> Self {
        Self {
            outcome: Err(error),
        }
    }
}

impl ScriptStep for ReconfigStep {
    const FAMILY: RequirementKindTag = RequirementKindTag::Reconfig;

    fn into_result(self) -> RequirementResult {
        RequirementResult::Reconfig(self.outcome)
    }
}

/// An ordered, single-family queue of scripted effect results.
///
/// A handler pops one step per fulfilled requirement with [`Script::next_step`].
/// Steps are matched purely by dispatch order. When the queue is drained the
/// script applies its [`StrictMode`]: it returns a classified [`ScriptError`]
/// (the default) or panics (opt-in).
///
/// The queue uses interior mutability so a handler can pop steps behind a shared
/// reference (`&self`) and a test can keep an `Arc` clone to inspect it.
#[derive(Debug)]
pub struct Script<S: ScriptStep> {
    state: Mutex<ScriptQueue<S>>,
    strict: StrictMode,
    label: Option<String>,
    defined_len: usize,
}

#[derive(Debug)]
struct ScriptQueue<S> {
    steps: VecDeque<S>,
    dispatched: usize,
}

impl<S: ScriptStep> Script<S> {
    /// Builds a script from `steps`, consumed in the given order, defaulting to
    /// [`StrictMode::Error`] and no label.
    #[must_use]
    pub fn new(steps: impl IntoIterator<Item = S>) -> Self {
        let steps: VecDeque<S> = steps.into_iter().collect();
        let defined_len = steps.len();
        Self {
            state: Mutex::new(ScriptQueue {
                steps,
                dispatched: 0,
            }),
            strict: StrictMode::Error,
            label: None,
            defined_len,
        }
    }

    /// Sets the exhaustion behaviour.
    #[must_use]
    pub fn with_strict_mode(mut self, strict: StrictMode) -> Self {
        self.strict = strict;
        self
    }

    /// Attaches a cassette/scenario label surfaced in exhaustion diagnostics.
    #[must_use]
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Returns the configured exhaustion behaviour.
    #[must_use]
    pub fn strict_mode(&self) -> StrictMode {
        self.strict
    }

    /// Returns the diagnostic label, if any.
    #[must_use]
    pub fn label(&self) -> Option<&str> {
        self.label.as_deref()
    }

    /// Returns the number of steps the script was defined with.
    #[must_use]
    pub fn defined_len(&self) -> usize {
        self.defined_len
    }

    /// Returns how many steps have been dispatched so far (including over-runs).
    #[must_use]
    pub fn dispatched(&self) -> usize {
        self.lock().dispatched
    }

    /// Returns how many scripted steps remain undelivered.
    #[must_use]
    pub fn remaining(&self) -> usize {
        self.lock().steps.len()
    }

    /// Returns whether the script has no steps left to deliver.
    #[must_use]
    pub fn is_exhausted(&self) -> bool {
        self.lock().steps.is_empty()
    }

    /// Delivers the next scripted step in dispatch order.
    ///
    /// # Errors
    ///
    /// Under [`StrictMode::Error`], returns [`ScriptError::Exhausted`] once the
    /// queue is drained.
    ///
    /// # Panics
    ///
    /// Under [`StrictMode::Panic`], panics once the queue is drained.
    pub fn next_step(&self) -> Result<S, ScriptError> {
        let mut state = self.lock();
        let call_index = state.dispatched;
        state.dispatched += 1;
        if let Some(step) = state.steps.pop_front() {
            return Ok(step);
        }
        drop(state);
        let error = ScriptError::Exhausted {
            family: S::FAMILY,
            call_index,
            script_len: self.defined_len,
            label: self.label.clone(),
        };
        match self.strict {
            StrictMode::Error => Err(error),
            StrictMode::Panic => panic!("{error}"),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ScriptQueue<S>> {
        self.state.lock().expect("script queue mutex poisoned")
    }
}

/// One recorded call against a scripted handler.
///
/// `request` is captured when the call begins; `result` and `completion_index`
/// are filled in when it completes, so a still-running call is observable as a
/// record whose `result` is `None`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallRecord<Req, Res> {
    /// Zero-based order in which this call began (dispatch order).
    pub call_index: usize,
    /// Summary of what the handler was asked.
    pub request: Req,
    /// Summary of what the handler returned, once the call completed.
    pub result: Option<Res>,
    /// Zero-based order in which this call completed, once it completed.
    pub completion_index: Option<usize>,
}

/// A handle tying a [`CallLog::begin`] to its later [`CallLog::complete`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CallTicket(usize);

/// An observable log of calls made against a scripted handler.
///
/// The log separates dispatch order ([`CallRecord::call_index`]) from completion
/// order ([`CallRecord::completion_index`]) so a test can assert on both even
/// when concurrent calls finish out of order. It uses interior mutability so a
/// handler records behind a shared reference while a test reads it.
///
/// The log also tracks *peak concurrency*: the maximum number of calls that
/// were in flight (begun but not yet completed) at once. Because every
/// [`begin`](Self::begin) and [`complete`](Self::complete) crosses the same
/// mutex, the log is the single serialization point where an in-flight gauge can
/// be maintained without a separate observer. A delay/barrier handler wrapper
/// (milestone M5) drives high concurrency; the peak it produces is read back
/// here through [`peak_concurrency`](Self::peak_concurrency).
#[derive(Debug)]
pub struct CallLog<Req, Res> {
    state: Mutex<CallLogState<Req, Res>>,
}

#[derive(Debug)]
struct CallLogState<Req, Res> {
    records: Vec<CallRecord<Req, Res>>,
    completed: usize,
    in_flight: usize,
    peak_in_flight: usize,
}

impl<Req, Res> Default for CallLog<Req, Res> {
    fn default() -> Self {
        Self::new()
    }
}

impl<Req, Res> CallLog<Req, Res> {
    /// Builds an empty call log.
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: Mutex::new(CallLogState {
                records: Vec::new(),
                completed: 0,
                in_flight: 0,
                peak_in_flight: 0,
            }),
        }
    }

    /// Records the start of a call and returns a [`CallTicket`] to complete it.
    pub fn begin(&self, request: Req) -> CallTicket {
        let mut state = self.lock();
        let call_index = state.records.len();
        state.records.push(CallRecord {
            call_index,
            request,
            result: None,
            completion_index: None,
        });
        state.in_flight += 1;
        if state.in_flight > state.peak_in_flight {
            state.peak_in_flight = state.in_flight;
        }
        CallTicket(call_index)
    }

    /// Records the completion of the call identified by `ticket`.
    ///
    /// Completing the same ticket twice keeps the first completion order and
    /// overwrites only the stored result.
    pub fn complete(&self, ticket: CallTicket, result: Res) {
        let mut state = self.lock();
        let first_completion = state.records[ticket.0].completion_index.is_none();
        let completion_index = if first_completion {
            let index = state.completed;
            state.completed += 1;
            state.in_flight = state.in_flight.saturating_sub(1);
            Some(index)
        } else {
            state.records[ticket.0].completion_index
        };
        let record = &mut state.records[ticket.0];
        record.result = Some(result);
        record.completion_index = completion_index;
    }

    /// Records a call that begins and completes atomically.
    pub fn record(&self, request: Req, result: Res) -> CallTicket {
        let ticket = self.begin(request);
        self.complete(ticket, result);
        ticket
    }

    /// Returns how many calls have begun.
    #[must_use]
    pub fn len(&self) -> usize {
        self.lock().records.len()
    }

    /// Returns whether no call has begun.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lock().records.is_empty()
    }

    /// Returns how many calls have completed.
    #[must_use]
    pub fn completed_len(&self) -> usize {
        self.lock().completed
    }

    /// Returns the peak number of calls in flight (begun but not completed) at
    /// any single moment over this log's lifetime.
    ///
    /// A log that only ever ran calls one at a time (or through
    /// [`record`](Self::record), which begins and completes atomically) reports
    /// `1` once any call has run, and `0` before the first call. Concurrent
    /// begins raise the peak until their matching completions bring the
    /// in-flight count back down.
    #[must_use]
    pub fn peak_concurrency(&self) -> usize {
        self.lock().peak_in_flight
    }

    /// Runs `visitor` against the recorded calls in dispatch order.
    pub fn with_records<T>(&self, visitor: impl FnOnce(&[CallRecord<Req, Res>]) -> T) -> T {
        visitor(&self.lock().records)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, CallLogState<Req, Res>> {
        self.state.lock().expect("call log mutex poisoned")
    }
}

impl<Req: Clone, Res: Clone> CallLog<Req, Res> {
    /// Returns a snapshot of the recorded calls in dispatch order.
    #[must_use]
    pub fn records(&self) -> Vec<CallRecord<Req, Res>> {
        self.lock().records.clone()
    }

    /// Returns the recorded request summaries in dispatch order.
    #[must_use]
    pub fn requests(&self) -> Vec<Req> {
        self.lock()
            .records
            .iter()
            .map(|record| record.request.clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CallLog, InteractionStep, LlmStep, ReconfigStep, Script, ScriptError, ScriptStep,
        StrictMode, ToolStep,
    };
    use agent_lib::agent::{RequirementKindTag, RequirementResult, ToolRuntimeError};
    use agent_lib::client::ClientError;
    use agent_lib::model::tool::ToolStatus;

    fn tag_of(result: &RequirementResult) -> RequirementKindTag {
        result.tag()
    }

    #[test]
    fn steps_convert_to_their_result_family() {
        assert_eq!(
            tag_of(&LlmStep::text("hi").into_result()),
            RequirementKindTag::Llm
        );
        assert_eq!(
            tag_of(&ToolStep::ok("call-1", "sunny").into_result()),
            RequirementKindTag::Tool
        );
        assert_eq!(
            tag_of(&InteractionStep::answer("yes").into_result()),
            RequirementKindTag::Interaction
        );
        assert_eq!(
            tag_of(&ReconfigStep::ok().into_result()),
            RequirementKindTag::Reconfig
        );
    }

    #[test]
    fn tool_step_error_is_a_model_visible_error_response() {
        let RequirementResult::Tool(Ok(response)) = ToolStep::error("call-1", "boom").into_result()
        else {
            panic!("ToolStep::error scripts an Ok(ToolResponse) with error status");
        };
        assert_eq!(response.status, ToolStatus::Error);
    }

    #[test]
    fn tool_step_runtime_error_stays_in_the_tool_family_err_path() {
        let result = ToolStep::runtime_error(ToolRuntimeError::UnknownTool {
            name: "nope".to_owned(),
        })
        .into_result();
        assert!(matches!(result, RequirementResult::Tool(Err(_))));
    }

    #[test]
    fn script_consumes_steps_in_dispatch_order() {
        let script = Script::new([
            ToolStep::ok("call-a", "A"),
            ToolStep::ok("call-b", "B"),
            ToolStep::ok("call-c", "C"),
        ]);
        assert_eq!(script.defined_len(), 3);
        assert_eq!(script.remaining(), 3);

        let keys: Vec<String> = (0..3)
            .map(|_| {
                let step = script.next_step().expect("step available");
                step.match_key()
                    .expect("tool step carries a key")
                    .to_owned()
            })
            .collect();

        assert_eq!(keys, vec!["call-a", "call-b", "call-c"]);
        assert_eq!(script.dispatched(), 3);
        assert_eq!(script.remaining(), 0);
        assert!(script.is_exhausted());
    }

    #[test]
    fn call_log_records_request_result_and_orders() {
        let log: CallLog<&str, &str> = CallLog::new();
        assert!(log.is_empty());

        // Two calls begin in dispatch order 0, 1 but complete in order 1, 0.
        let first = log.begin("req-0");
        let second = log.begin("req-1");
        assert_eq!(log.len(), 2);
        assert_eq!(log.completed_len(), 0);

        log.complete(second, "res-1");
        log.complete(first, "res-0");

        let records = log.records();
        assert_eq!(records.len(), 2);

        assert_eq!(records[0].call_index, 0);
        assert_eq!(records[0].request, "req-0");
        assert_eq!(records[0].result, Some("res-0"));
        assert_eq!(records[0].completion_index, Some(1));

        assert_eq!(records[1].call_index, 1);
        assert_eq!(records[1].request, "req-1");
        assert_eq!(records[1].result, Some("res-1"));
        assert_eq!(records[1].completion_index, Some(0));

        assert_eq!(log.requests(), vec!["req-0", "req-1"]);
        assert_eq!(log.completed_len(), 2);
    }

    #[test]
    fn call_log_record_begins_and_completes_atomically() {
        let log: CallLog<u8, u8> = CallLog::new();
        log.record(1, 10);
        log.record(2, 20);
        log.with_records(|records| {
            assert_eq!(records.len(), 2);
            assert_eq!(records[0].completion_index, Some(0));
            assert_eq!(records[1].completion_index, Some(1));
        });
    }

    #[test]
    fn call_log_tracks_peak_concurrency() {
        let log: CallLog<u8, u8> = CallLog::new();
        assert_eq!(log.peak_concurrency(), 0, "no call has begun yet");

        // Two overlapping calls raise the peak to 2; a third begins only after
        // both complete, so the in-flight count never exceeds 2.
        let a = log.begin(1);
        let b = log.begin(2);
        assert_eq!(log.peak_concurrency(), 2);
        log.complete(b, 20);
        log.complete(a, 10);

        let c = log.begin(3);
        assert_eq!(log.peak_concurrency(), 2, "the peak is a high-water mark");
        log.complete(c, 30);
        assert_eq!(log.peak_concurrency(), 2);
    }

    #[test]
    fn call_log_sequential_calls_peak_at_one() {
        let log: CallLog<u8, u8> = CallLog::new();
        log.record(1, 10);
        log.record(2, 20);
        assert_eq!(
            log.peak_concurrency(),
            1,
            "atomic begin/complete never overlaps"
        );
    }

    #[test]
    fn exhausted_script_returns_a_classified_error_by_default() {
        let script = Script::new([LlmStep::text("only")]);
        assert_eq!(script.strict_mode(), StrictMode::Error);

        script.next_step().expect("first step is available");
        let error = script.next_step().expect_err("second call over-runs");

        let ScriptError::Exhausted {
            family,
            call_index,
            script_len,
            label,
        } = error.clone();
        assert_eq!(family, RequirementKindTag::Llm);
        assert_eq!(call_index, 1);
        assert_eq!(script_len, 1);
        assert_eq!(label, None);

        let message = error.to_string();
        assert!(
            message.contains("llm"),
            "message names the family: {message}"
        );
        assert!(
            message.contains("call #1"),
            "message names the call index: {message}"
        );
        assert!(
            message.contains("1 step"),
            "message names the script length: {message}"
        );
    }

    #[test]
    fn exhaustion_error_includes_the_optional_label() {
        let script = Script::<ReconfigStep>::new([]).with_label("weather-scenario");
        let error = script.next_step().expect_err("empty script over-runs");
        let message = error.to_string();
        assert!(
            message.contains("weather-scenario"),
            "message names the label: {message}"
        );
        assert!(
            message.contains("reconfig"),
            "message names the family: {message}"
        );
    }

    #[test]
    fn error_mode_does_not_panic_on_exhaustion() {
        let script = Script::<LlmStep>::new([]);
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| script.next_step()));
        assert!(
            outcome.is_ok(),
            "the default Error mode must not panic on exhaustion"
        );
        assert!(matches!(
            outcome.unwrap(),
            Err(ScriptError::Exhausted { .. })
        ));
    }

    #[test]
    fn panic_mode_panics_only_when_opted_in() {
        let script = Script::<LlmStep>::new([]).with_strict_mode(StrictMode::Panic);
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| script.next_step()));
        assert!(
            outcome.is_err(),
            "opt-in Panic mode must panic on exhaustion"
        );
    }

    #[test]
    fn llm_step_error_stays_in_the_llm_family_err_path() {
        let result = LlmStep::error(ClientError::Timeout).into_result();
        assert!(matches!(
            result,
            RequirementResult::Llm(Err(ClientError::Timeout))
        ));
    }
}
