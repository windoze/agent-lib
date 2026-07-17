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
//! - [`IntoUserMessage`] — the input conversion used by every `ask`/`send`
//!   entry point.
//!
//! Milestone 1 only produces the [`RunEvent::TextDelta`], [`RunEvent::Done`],
//! [`RunEvent::RawStream`], and [`RunEvent::RawNotification`] variants and the
//! supervisor slice of [`UsageSummary`]; the delegation- and tool-related
//! variants and trace types are defined now (so the enum shape is stable) but
//! are populated by later milestones. See `docs/facade-api.md` §5.2 and §6.

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
            stop_reason: Some(response.stop_reason.value),
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
/// Milestone 1 leaves `tool_calls`, `delegations`, `artifacts`, and `events`
/// empty; later milestones populate them.
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

/// Token usage aggregated across every participant in a run.
///
/// The facade separates usage reported by the supervisor model, by local
/// subagents, and by external runtimes so a caller can attribute cost. Use
/// [`UsageSummary::total`] for the combined figure. Milestone 1 only fills the
/// `supervisor` slice.
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
/// Milestone 1 only produces [`RunEvent::TextDelta`], [`RunEvent::Done`],
/// [`RunEvent::RawStream`], and [`RunEvent::RawNotification`]. The remaining
/// tool- and delegation-related variants are defined now so the enum shape is
/// stable, and are produced by later milestones.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunEvent {
    /// An incremental chunk of assistant text.
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

/// Placeholder trace for a single tool invocation.
///
/// Milestone 1 never produces this; the Agent facade (Milestone 2) fills it in.
/// The field set is minimal and may grow (the type is `#[non_exhaustive]`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ToolTrace {
    /// Name of the invoked tool.
    pub name: String,
    /// Identifier of the tool call this trace describes.
    pub call_id: String,
}

/// Placeholder for an approval request surfaced to the caller.
///
/// Populated by the Agent facade (Milestone 2, `docs/facade-api.md` §9). The
/// field set is minimal and may grow (the type is `#[non_exhaustive]`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ApprovalRequest {
    /// Name of the tool whose execution is awaiting approval.
    pub tool_name: String,
}

/// Placeholder trace describing one delegation to a subagent or external agent.
///
/// Populated by the subagent/external milestones (Milestone 3/4). The field set
/// is minimal and may grow (the type is `#[non_exhaustive]`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct DelegationTrace {
    /// Name of the delegate that handled the task.
    pub delegate: String,
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
