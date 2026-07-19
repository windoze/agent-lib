//! Facade result and event types.
//!
//! This module defines the ergonomic values a facade run hands back:
//!
//! - [`Reply`] — the minimal successful result (aggregated text plus optional
//!   usage and stop reason).
//! - [`RunOutput`] — the full integration/debugging surface, including the
//!   optional underlying [`Response`], a [`UsageSummary`], and per-run traces.
//! - [`UsageSummary`] — token usage aggregated across the supervisor, local
//!   subagents, and external runtimes.
//! - [`RunEvent`] — a UI/CLI-friendly streaming event, with raw escape hatches.
//! - [`WireRunEvent`] — the official serializable projection of [`RunEvent`]
//!   (via [`RunEvent::to_wire`]) for cross-process hosts; normalized variants
//!   are lossless, the raw escape hatches collapse to opaque markers.
//! - [`IntoUserMessage`] — the input conversion used by every `ask`/`send`
//!   entry point.
//!
//! Current facade drives populate text, tool, approval, delegation, escalation,
//! raw, and terminal events as applicable. The public event enums are explicitly
//! non-exhaustive; UI and cross-process hosts should include a fallback arm when
//! matching them. See `docs/facade-api.md` §5.2 and §6.

use serde::{Deserialize, Serialize};

use crate::agent::Notification;
use crate::client::Response;
use crate::model::content::ContentBlock;
use crate::model::message::{Message, Role};
use crate::model::normalized::StopReason;
use crate::model::usage::Usage;
use crate::stream::StreamEvent;

/// The minimal successful result of a facade run.
///
/// `Reply::text` is aggregated from the text blocks of the normalized
/// [`Response`]. Any non-text content is *not* discarded: the complete response
/// is retained in [`RunOutput::response`] (see `docs/facade-api.md` §6.1).
///
/// The `usage` field uses the crate's normalized [`Usage`] accounting type. The
/// design note refers to it as `TokenUsage`; the concrete type in this crate is
/// [`crate::model::usage::Usage`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reply {
    /// Aggregated assistant text.
    text: String,
    /// Token usage reported for the underlying response, when available.
    usage: Option<Usage>,
    /// Normalized stop reason for the underlying response, when available.
    stop_reason: Option<StopReason>,
}

impl Reply {
    /// Builds a reply from already-aggregated parts.
    ///
    /// Used by the Agent facade, whose sans-io drive folds each LLM response
    /// into the [`Conversation`](crate::conversation::Conversation) and does not
    /// hand back a raw [`Response`]. The `text` is the final assistant text, the
    /// `usage` is aggregated across every step of the run, and `stop_reason` is
    /// the normalized stop reason of the final response, when known.
    pub(crate) fn from_parts(
        text: String,
        usage: Option<Usage>,
        stop_reason: Option<StopReason>,
    ) -> Self {
        Self {
            text,
            usage,
            stop_reason,
        }
    }

    /// Returns the aggregated assistant text.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Returns the token usage reported for the response, if any.
    #[must_use]
    pub fn usage(&self) -> Option<&Usage> {
        self.usage.as_ref()
    }

    /// Returns the normalized stop reason for the response, if any.
    #[must_use]
    pub fn stop_reason(&self) -> Option<&StopReason> {
        self.stop_reason.as_ref()
    }
}

impl From<&Response> for Reply {
    /// Builds a reply from a complete [`Response`], aggregating its text blocks.
    fn from(response: &Response) -> Self {
        Self {
            text: aggregate_text(&response.message.content),
            usage: Some(response.usage.clone()),
            stop_reason: Some(*response.stop_reason.value()),
        }
    }
}

/// Concatenates the text of every [`ContentBlock::Text`] block, in order.
///
/// Non-text blocks (tool use, images, thinking, ...) are skipped here but kept
/// intact in the owning [`Response`], which [`RunOutput`] preserves.
fn aggregate_text(content: &[ContentBlock]) -> String {
    let mut text = String::new();
    for block in content {
        if let ContentBlock::Text { text: chunk, .. } = block {
            text.push_str(chunk);
        }
    }
    text
}

/// The full result surface of a facade run.
///
/// This is the product-integration and debugging entry point. A Chat one-shot
/// or session run usually has `response: Some(_)`; a managed external agent may
/// have no one-to-one LLM `Response` yet still report a `reply`, `delegations`,
/// `artifacts`, and `events` (see `docs/facade-api.md` §6.2).
///
/// The concrete run path determines which trace and event collections are filled:
/// plain chat usually leaves them empty, while agent runs can populate tool,
/// approval, delegation, artifact, and escalation lifecycle data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunOutput {
    /// The minimal successful result.
    pub reply: Reply,
    /// The complete underlying LLM response, when the run produced one.
    pub response: Option<Response>,
    /// Token usage aggregated across supervisor, subagents, and external runtimes.
    pub usage: UsageSummary,
    /// Traces for tools invoked during the run (populated from Milestone 2).
    pub tool_calls: Vec<ToolTrace>,
    /// Traces for delegations performed during the run (populated from Milestone 3).
    pub delegations: Vec<DelegationTrace>,
    /// Artifacts produced by delegates during the run (populated from Milestone 4).
    pub artifacts: Vec<ArtifactRef>,
    /// The ordered normalized events observed during the run.
    ///
    /// These are the lifecycle events — [`RunEvent::ApprovalRequested`],
    /// [`RunEvent::ToolStarted`]/[`RunEvent::ToolFinished`], and the
    /// `Delegation*` family — in drive order. They are contracted to match the
    /// events [`Agent::stream`](crate::facade::Agent::stream) yields for the
    /// same run, **except** that this non-streaming vector never contains the
    /// streaming-only token [`RunEvent::TextDelta`]s or the terminal
    /// [`RunEvent::Done`] (see `docs/facade-api.md` §6.2.1). A denied tool call
    /// leaves no `ToolStarted`/`ToolFinished` here, matching the streaming path.
    pub events: Vec<RunEvent>,
}

impl From<Response> for RunOutput {
    /// Builds a single-response run output, retaining the full response.
    ///
    /// Only the supervisor usage slice is filled; the tool/delegation/artifact
    /// vectors are empty and `events` is left for the caller to attach.
    fn from(response: Response) -> Self {
        let reply = Reply::from(&response);
        let usage = UsageSummary::from_supervisor(response.usage.clone());
        Self {
            reply,
            response: Some(response),
            usage,
            tool_calls: Vec::new(),
            delegations: Vec::new(),
            artifacts: Vec::new(),
            events: Vec::new(),
        }
    }
}

impl RunOutput {
    /// Projects this run output into its serializable [`WireRunOutput`] form.
    ///
    /// Every field except `events` is forwarded verbatim (all of them —
    /// [`Reply`], the underlying [`Response`], [`UsageSummary`], and the trace
    /// vectors — are already serializable). The `events` are projected element
    /// by element through [`RunEvent::to_wire`], so any nested
    /// [`RunEvent::RawStream`]/[`RunEvent::RawNotification`] degrades to an
    /// opaque [`WireRunEvent::Raw`] marker exactly as at the top level.
    #[must_use]
    pub fn to_wire(&self) -> WireRunOutput {
        WireRunOutput {
            reply: self.reply.clone(),
            response: self.response.clone(),
            usage: self.usage.clone(),
            tool_calls: self.tool_calls.clone(),
            delegations: self.delegations.clone(),
            artifacts: self.artifacts.clone(),
            events: self.events.iter().map(RunEvent::to_wire).collect(),
        }
    }
}

/// A serializable projection of [`RunOutput`] (see [`RunOutput::to_wire`]).
///
/// [`RunOutput`] intentionally does not derive `serde` because it embeds
/// `events: Vec<RunEvent>`, and [`RunEvent`] carries non-serializable escape
/// hatches (see [`WireRunEvent`]). This mirror type holds the same fields but
/// stores the events as already-projected [`WireRunEvent`]s, so the whole
/// structure round-trips through `serde` (modulo the lossy `Raw` markers).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireRunOutput {
    /// The minimal successful result.
    pub reply: Reply,
    /// The complete underlying LLM response, when the run produced one.
    pub response: Option<Response>,
    /// Token usage aggregated across supervisor, subagents, and external runtimes.
    pub usage: UsageSummary,
    /// Traces for tools invoked during the run.
    pub tool_calls: Vec<ToolTrace>,
    /// Traces for delegations performed during the run.
    pub delegations: Vec<DelegationTrace>,
    /// Artifacts produced by delegates during the run.
    pub artifacts: Vec<ArtifactRef>,
    /// The ordered projected events observed during the run.
    pub events: Vec<WireRunEvent>,
}

/// Token usage aggregated across every participant in a run.
///
/// The facade separates usage reported by the supervisor model, by local
/// subagents, and by external runtimes so a caller can attribute cost. Use
/// [`UsageSummary::total`] for the combined figure. Plain supervisor runs fill
/// the `supervisor` slice; local subagent and managed external delegation paths
/// fill their respective slices.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageSummary {
    /// Usage reported by the top-level supervisor model.
    pub supervisor: Usage,
    /// Usage reported by local subagents (populated from Milestone 3).
    pub subagents: Usage,
    /// Usage reported by managed external runtimes (populated from Milestone 4).
    pub external: Usage,
}

impl UsageSummary {
    /// Builds a summary that only accounts for the supervisor model.
    #[must_use]
    pub fn from_supervisor(usage: Usage) -> Self {
        Self {
            supervisor: usage,
            ..Self::default()
        }
    }

    /// Returns the combined usage across supervisor, subagents, and external runtimes.
    #[must_use]
    pub fn total(&self) -> Usage {
        let mut total = self.supervisor.clone();
        total.merge(self.subagents.clone());
        total.merge(self.external.clone());
        total
    }

    /// Adds usage reported by the supervisor model.
    pub fn add_supervisor(&mut self, usage: Usage) {
        self.supervisor.merge(usage);
    }

    /// Adds usage reported by a local subagent.
    pub fn add_subagent(&mut self, usage: Usage) {
        self.subagents.merge(usage);
    }

    /// Adds usage reported by a managed external runtime.
    pub fn add_external(&mut self, usage: Usage) {
        self.external.merge(usage);
    }
}

/// A UI/CLI-oriented event emitted while a run streams.
///
/// The normalized variants are intended to be close to what a terminal or UI
/// wants to render. The two `Raw*` variants are escape hatches carrying the
/// underlying [`StreamEvent`]/[`Notification`]; they should not be the primary
/// path in simple code. Because those escape hatches may carry values whose
/// serialization is not a stable contract, `RunEvent` intentionally does not
/// derive `serde` (see `PLAN.md` R7); the normalized leaf types (for example
/// [`Reply`] and [`UsageSummary`]) remain serializable on their own.
///
/// The enum is non-exhaustive. Downstream renderers should include a wildcard arm
/// so new lifecycle events can be added without forcing a lockstep update.
///
/// # Streaming vs non-streaming
///
/// The lifecycle variants ([`ApprovalRequested`](RunEvent::ApprovalRequested),
/// [`ToolStarted`](RunEvent::ToolStarted)/[`ToolFinished`](RunEvent::ToolFinished),
/// and the `Delegation*` family) are emitted identically by the streaming path
/// ([`Agent::stream`](crate::facade::Agent::stream)) and folded into
/// [`RunOutput::events`] by the non-streaming path
/// ([`Agent::run_full`](crate::facade::Agent::run_full)). The token-level
/// [`TextDelta`](RunEvent::TextDelta) and the terminal
/// [`Done`](RunEvent::Done) are **streaming-only**; the non-streaming path never
/// fabricates token deltas (see `docs/facade-api.md` §6.2.1).
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum RunEvent {
    /// An incremental chunk of assistant text.
    ///
    /// Streaming-only: yielded by [`Agent::stream`](crate::facade::Agent::stream)
    /// as tokens arrive. The non-streaming [`RunOutput::events`] never contains
    /// this variant.
    TextDelta(String),
    /// A tool invocation has started (populated from Milestone 2).
    ToolStarted(ToolTrace),
    /// A tool invocation has finished (populated from Milestone 2).
    ToolFinished(ToolTrace),
    /// The run is waiting on an approval decision (populated from Milestone 2).
    ApprovalRequested(ApprovalRequest),
    /// A delegation has started (populated from Milestone 3).
    DelegationStarted(DelegationTrace),
    /// A delegation reported progress (populated from Milestone 3).
    DelegationProgress(DelegationProgress),
    /// A delegation emitted a message (populated from Milestone 3).
    DelegationMessage(DelegationMessage),
    /// A delegation produced an artifact (populated from Milestone 4).
    DelegationArtifact(ArtifactRef),
    /// A delegation finished successfully (populated from Milestone 3).
    DelegationFinished(DelegationTrace),
    /// A delegation failed (populated from Milestone 3).
    DelegationFailed(DelegationTrace),
    /// A delegation was escalated to a stronger delegate (populated from Milestone 5).
    Escalated(EscalationTrace),
    /// The run finished; carries the complete [`RunOutput`].
    ///
    /// The payload is boxed so this large terminal variant does not inflate the
    /// size of every `RunEvent` (the design sketch in `docs/facade-api.md` §6.3
    /// writes it unboxed). A bound value still derefs to [`RunOutput`], so
    /// field access such as `output.usage` reads the same.
    Done(Box<RunOutput>),

    /// Escape hatch: a raw client-layer stream event.
    RawStream(StreamEvent),
    /// Escape hatch: a raw agent-layer notification.
    RawNotification(Notification),
}

impl RunEvent {
    /// Projects this event into its official serializable [`WireRunEvent`] form.
    ///
    /// This is the single, explicit, one-way bridge for cross-process hosts that
    /// need to ship run events over a wire. It exists precisely because
    /// [`RunEvent`] does not (and will not) derive `serde`: the projection is
    /// *lossy by design*. Every normalized variant forwards its already
    /// serializable payload verbatim, but the two escape hatches
    /// ([`RunEvent::RawStream`]/[`RunEvent::RawNotification`]) collapse to an
    /// opaque [`WireRunEvent::Raw`] marker that records only which escape hatch
    /// fired — their underlying `StreamEvent`/`Notification` payload is *not*
    /// carried, keeping R7 intact (their serialization is not a stable
    /// contract).
    #[must_use]
    pub fn to_wire(&self) -> WireRunEvent {
        match self {
            RunEvent::TextDelta(text) => WireRunEvent::TextDelta(text.clone()),
            RunEvent::ToolStarted(trace) => WireRunEvent::ToolStarted(trace.clone()),
            RunEvent::ToolFinished(trace) => WireRunEvent::ToolFinished(trace.clone()),
            RunEvent::ApprovalRequested(req) => WireRunEvent::ApprovalRequested(req.clone()),
            RunEvent::DelegationStarted(trace) => WireRunEvent::DelegationStarted(trace.clone()),
            RunEvent::DelegationProgress(progress) => {
                WireRunEvent::DelegationProgress(progress.clone())
            }
            RunEvent::DelegationMessage(message) => {
                WireRunEvent::DelegationMessage(message.clone())
            }
            RunEvent::DelegationArtifact(artifact) => {
                WireRunEvent::DelegationArtifact(artifact.clone())
            }
            RunEvent::DelegationFinished(trace) => WireRunEvent::DelegationFinished(trace.clone()),
            RunEvent::DelegationFailed(trace) => WireRunEvent::DelegationFailed(trace.clone()),
            RunEvent::Escalated(trace) => WireRunEvent::Escalated(trace.clone()),
            RunEvent::Done(output) => WireRunEvent::Done(Box::new(output.to_wire())),
            RunEvent::RawStream(_) => WireRunEvent::Raw(RawEventKind::Stream),
            RunEvent::RawNotification(_) => WireRunEvent::Raw(RawEventKind::Notification),
        }
    }
}

/// Identifies which [`RunEvent`] escape hatch a [`WireRunEvent::Raw`] marker
/// stands in for.
///
/// The projection deliberately drops the escape hatch payload (see
/// [`RunEvent::to_wire`]); this enum preserves only the *kind* of raw event so a
/// consumer can tell a raw client stream event from a raw agent notification
/// without gaining a stable-serialization contract over their contents.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RawEventKind {
    /// Stands in for [`RunEvent::RawStream`] (a raw client-layer stream event).
    Stream,
    /// Stands in for [`RunEvent::RawNotification`] (a raw agent-layer notification).
    Notification,
}

/// The official serializable projection of [`RunEvent`] (see
/// [`RunEvent::to_wire`]).
///
/// [`RunEvent`] intentionally does not derive `serde` (see `PLAN.md` R7),
/// because its [`RunEvent::RawStream`]/[`RunEvent::RawNotification`] escape
/// hatches carry values whose serialization is not a stable contract. Rather
/// than forcing every cross-process host to re-derive its own wire enum, this
/// type is the single canonical bridge:
///
/// - **Normalized variants are lossless**: `TextDelta`, `ToolStarted`,
///   `ToolFinished`, `ApprovalRequested`, the `Delegation*` family, `Escalated`,
///   and `Done` forward their already-serializable payloads verbatim (via
///   [`WireRunOutput`] for `Done`), so `to_wire()` followed by a `serde_json`
///   round-trip reproduces the same value.
/// - **`Raw` is opaque and lossy**: both escape hatches collapse to
///   [`WireRunEvent::Raw`], which records only a [`RawEventKind`] and carries no
///   payload. This is what keeps R7 intact — the projection never promotes the
///   escape hatch serialization to a stable contract.
///
/// The variants are adjacently tagged (`{"type": ..., "data": ...}`,
/// `snake_case`), matching [`Notification`]'s wire shape.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
#[non_exhaustive]
pub enum WireRunEvent {
    /// An incremental chunk of assistant text.
    TextDelta(String),
    /// A tool invocation has started.
    ToolStarted(ToolTrace),
    /// A tool invocation has finished.
    ToolFinished(ToolTrace),
    /// The run is waiting on an approval decision.
    ApprovalRequested(ApprovalRequest),
    /// A delegation has started.
    DelegationStarted(DelegationTrace),
    /// A delegation reported progress.
    DelegationProgress(DelegationProgress),
    /// A delegation emitted a message.
    DelegationMessage(DelegationMessage),
    /// A delegation produced an artifact.
    DelegationArtifact(ArtifactRef),
    /// A delegation finished successfully.
    DelegationFinished(DelegationTrace),
    /// A delegation failed.
    DelegationFailed(DelegationTrace),
    /// A delegation was escalated to a stronger delegate.
    Escalated(EscalationTrace),
    /// The run finished; carries the projected [`WireRunOutput`].
    ///
    /// The payload is boxed for the same reason as [`RunEvent::Done`]: it keeps
    /// this large terminal variant from inflating the size of every
    /// `WireRunEvent`.
    Done(Box<WireRunOutput>),
    /// Opaque stand-in for a [`RunEvent`] escape hatch.
    ///
    /// Carries only which escape hatch fired (see [`RawEventKind`]); the
    /// underlying `StreamEvent`/`Notification` payload is intentionally dropped.
    Raw(RawEventKind),
}

/// Placeholder trace for a single tool invocation.
///
/// The field set is minimal and may grow (the type is `#[non_exhaustive]`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ToolTrace {
    /// Name of the invoked tool.
    pub name: String,
    /// Identifier of the tool call this trace describes.
    pub call_id: String,
}

/// An approval request surfaced to the caller (`docs/facade-api.md` §9).
///
/// Populated by the Agent facade (Milestone 2, enriched in Milestone 7-3) so a
/// UI/CLI can render a meaningful approval box. The field set may still grow
/// (the type is `#[non_exhaustive]`).
///
/// # Fields and redaction
///
/// - [`tool_name`](Self::tool_name) — the tool whose execution is paused.
/// - [`call_id`](Self::call_id) — the framework tool-call id the decision must
///   address, stringified. `None` only for the synchronous external-delegate
///   start path, which has no framework call id.
/// - [`reason`](Self::reason) — the stable, model-visible reason carried by the
///   underlying [`ApprovalRequirement`](crate::agent::ApprovalRequirement), if
///   any.
/// - [`input`](Self::input) — a **compact, redaction-safe** one-line summary of
///   the tool call arguments, not the raw payload. Object keys that look like
///   credentials (for example `token`, `api_key`, `password`) have their values
///   replaced with `<redacted>`, and the summary is truncated to a bounded
///   length, so a value can be logged or shipped over a wire without leaking
///   secrets or large payloads. `None` when the call carried no arguments.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ApprovalRequest {
    /// Name of the tool whose execution is awaiting approval.
    pub tool_name: String,
    /// Framework tool-call id the approval decision must address (stringified).
    ///
    /// `None` for the synchronous external-delegate start path, which is gated
    /// before any framework call id exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    /// Stable, model-visible reason for the pause, if the requirement carried one.
    pub reason: Option<String>,
    /// Compact, redaction-safe summary of the tool arguments (never the raw
    /// payload); `None` when the call carried no arguments.
    pub input: Option<String>,
}

impl ApprovalRequest {
    /// Builds a request naming only `tool_name`, leaving the enriched fields
    /// empty.
    ///
    /// Used by the synchronous external-delegate start path, which decides
    /// before any framework [`call_id`](Self::call_id), reason, or tool input is
    /// available.
    #[must_use]
    pub fn for_tool(tool_name: impl Into<String>) -> Self {
        Self {
            tool_name: tool_name.into(),
            call_id: None,
            reason: None,
            input: None,
        }
    }
}

/// The terminal outcome of a single delegation (`docs/facade-api.md` §10.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DelegationStatus {
    /// The delegate ran to completion and its summary was folded back.
    Completed,
    /// The delegate failed; the error was folded back to the supervising model.
    Failed,
}

/// A trace describing one delegation to a local subagent (`docs/facade-api.md`
/// §10.2).
///
/// Produced by the model-routed delegation path (Milestone 3): when the
/// supervising model calls an `ask_<name>` delegation tool, the child machine is
/// driven to completion and one of these is recorded into
/// [`RunOutput::delegations`]. The field set is minimal and may grow (the type
/// is `#[non_exhaustive]`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct DelegationTrace {
    /// Name of the delegate that handled the task.
    pub delegate: String,
    /// Whether the delegation completed or failed.
    pub status: DelegationStatus,
    /// Token usage reported by the child machine for the delegated turn.
    pub usage: Usage,
}

/// Placeholder for a progress update reported by a running delegation.
///
/// Populated by the subagent/external milestones (Milestone 3/4). The field set
/// is minimal and may grow (the type is `#[non_exhaustive]`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct DelegationProgress {
    /// Name of the delegate reporting progress.
    pub delegate: String,
    /// A human-readable progress message.
    pub message: String,
}

/// Placeholder for a message emitted by a running delegation.
///
/// Populated by the subagent/external milestones (Milestone 3/4). The field set
/// is minimal and may grow (the type is `#[non_exhaustive]`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct DelegationMessage {
    /// Name of the delegate that emitted the message.
    pub delegate: String,
    /// The message text.
    pub message: String,
}

/// Placeholder reference to an artifact produced by a delegate.
///
/// Populated by the managed external agent milestone (Milestone 4). The field
/// set is minimal and may grow (the type is `#[non_exhaustive]`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ArtifactRef {
    /// Path or identifier locating the artifact.
    pub path: String,
}

/// Placeholder trace describing a dispatcher escalation.
///
/// Populated by the dispatcher/escalator milestone (Milestone 5). The field set
/// is minimal and may grow (the type is `#[non_exhaustive]`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct EscalationTrace {
    /// The delegate the task was escalated from.
    pub from: String,
    /// The delegate the task was escalated to.
    pub to: String,
}

/// Conversion from an ergonomic input into a user [`Message`].
///
/// Every `ask`/`send`/`stream` entry point accepts `impl IntoUserMessage`, so a
/// caller can pass a `&str`, a `String`, an already-built [`Message`], or a
/// `Vec<ContentBlock>`. Later versions may extend this to images, files, and
/// tool results (see `docs/facade-api.md` §5.2).
pub trait IntoUserMessage {
    /// Converts `self` into a user-role [`Message`].
    fn into_user_message(self) -> Message;
}

impl IntoUserMessage for Message {
    fn into_user_message(self) -> Message {
        self
    }
}

impl IntoUserMessage for Vec<ContentBlock> {
    fn into_user_message(self) -> Message {
        Message {
            role: Role::User,
            content: self,
        }
    }
}

impl IntoUserMessage for String {
    fn into_user_message(self) -> Message {
        Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: self,
                extra: Default::default(),
            }],
        }
    }
}

impl IntoUserMessage for &str {
    fn into_user_message(self) -> Message {
        self.to_owned().into_user_message()
    }
}

#[cfg(test)]
mod tests;
