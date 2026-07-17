//! Serializable external-agent machine state and its recovery cursor.
//!
//! [`ExternalAgentState`] is the data half of a running external-agent machine,
//! mirroring [`AgentState`](crate::agent::AgentState): it owns exactly one live
//! [`Conversation`], records the resumable [`ExternalSessionRef`], the active
//! tool declarations, and a data-only [`ExternalAgentCursor`] for pause/restore.
//! Live handles (CLI process, SDK client, stdout reader, watcher, task set) live
//! in [`ExternalRuntimeHandles`](super::ExternalRuntimeHandles) instead of this
//! serde shape (design §4.2).

use crate::{
    agent::{
        CursorRequirement, Interaction, ToolWaitRequirements,
        external::{
            ExternalAgentSpec, ExternalArtifactRef, ExternalSessionRef, ExternalSubagentRequestId,
            ExternalToolBatchId,
        },
        spec::ToolSetRef,
    },
    conversation::{Conversation, ConversationError, ConversationSnapshot},
};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de, ser};

/// Data-only recovery cursor for an external-agent machine.
///
/// The cursor records which decision point the machine is parked on, and — for
/// the awaiting variants — the precise [`CursorRequirement`] it is stuck on, so a
/// driver can serialize a paused machine, restore it elsewhere, and rebuild the
/// pending-requirement registry straight from the cursor. It never contains a
/// live session, process, task handle, or interaction responder (design §4.2).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", content = "data", rename_all = "snake_case")]
pub enum ExternalAgentCursor {
    /// No external session step is currently outstanding.
    #[default]
    Idle,
    /// The machine emitted a `NeedExternalSession` requirement and is waiting for
    /// the session to advance to its next decision point.
    AwaitingSession {
        /// The outstanding external-session requirement being awaited.
        requirement: CursorRequirement,
    },
    /// A session paused for an interaction; the machine emitted a
    /// `NeedInteraction` requirement and is waiting for the host's response.
    AwaitingInteraction {
        /// The outstanding interaction requirement being awaited.
        requirement: CursorRequirement,
        /// Identifier of the paused action the resolved interaction answers,
        /// fed back through [`ExternalSessionInput::RespondInteraction`].
        ///
        /// [`ExternalSessionInput::RespondInteraction`]:
        /// crate::agent::external::ExternalSessionInput::RespondInteraction
        pending_action: String,
        /// The neutral [`Interaction`] the runtime paused for, retained so the
        /// machine can validate the host's [`InteractionResponse`] against it —
        /// via [`Interaction::accepts_response`] — *before* relaying it back to
        /// the runtime as a
        /// [`RespondInteraction`](crate::agent::external::ExternalSessionInput::RespondInteraction).
        /// A response of the wrong family, an out-of-range choice index, or a
        /// mismatched permission `action_id` is rejected into an error cursor
        /// instead of being forwarded to the runtime.
        ///
        /// [`InteractionResponse`]: crate::agent::InteractionResponse
        interaction: Interaction,
    },
    /// A session paused for a batch of tool calls; the machine bridged each call
    /// into a host requirement and is waiting for every result.
    ///
    /// A plain call bridges into a `NeedTool`; a `spawn_agent` call is a
    /// scope-deepening operation that bridges into a `NeedSubagent` whose child
    /// output folds back into this same batch as a tool result (design §8.3).
    ///
    /// The cursor persists only the resumable addressing: the runtime-assigned
    /// [`ExternalToolBatchId`] echoed back through
    /// [`ExternalSessionInput::RespondToolResults`], and the batch's
    /// [`ToolWaitRequirements`] mapping each provider-independent
    /// [`ToolCallId`](crate::conversation::ToolCallId) to the
    /// [`RequirementId`](crate::agent::RequirementId) the driver must fulfill —
    /// for a `spawn_agent` bridge that mapped requirement is a `NeedSubagent`
    /// rather than a `NeedTool`, so the whole mixed batch parks on one cursor and
    /// its pending requirement ids recover uniformly. That lets a driver rebuild
    /// its pending-requirement registry from a restored machine. The volatile
    /// per-call correlation (provider call id, bridge kind) and the results
    /// collected so far live only in the machine's non-serialized scratch, so a
    /// mid-turn restore cannot resume a partially answered batch — see the
    /// machine's tool-phase docs.
    ///
    /// [`ExternalSessionInput::RespondToolResults`]:
    /// crate::agent::external::ExternalSessionInput::RespondToolResults
    AwaitingTool {
        /// Runtime-assigned batch token echoed back through
        /// [`RespondToolResults`](crate::agent::external::ExternalSessionInput::RespondToolResults).
        batch_id: ExternalToolBatchId,
        /// Outstanding tool requirement addressing
        /// (`ToolCallId -> RequirementId`) for the awaited batch.
        requirements: ToolWaitRequirements,
    },
    /// A session paused for a subagent spawn; the machine emitted one
    /// `NeedSubagent` requirement and is waiting for the host to drive the child
    /// agent and return its output.
    ///
    /// The cursor persists the resumable addressing: the outstanding
    /// [`CursorRequirement`] the driver must fulfill, and the runtime-assigned
    /// [`ExternalSubagentRequestId`] echoed back through
    /// [`ExternalSessionInput::RespondSubagent`] once the child's output is fed
    /// back — so a restored machine can rebuild its pending-requirement registry
    /// and correlate the eventual result with the runtime's spawn request.
    ///
    /// [`ExternalSessionInput::RespondSubagent`]:
    /// crate::agent::external::ExternalSessionInput::RespondSubagent
    AwaitingSubagent {
        /// The outstanding subagent requirement being awaited.
        requirement: CursorRequirement,
        /// Identifier of the runtime's spawn request the resolved subagent
        /// output answers, fed back through
        /// [`RespondSubagent`](crate::agent::external::ExternalSessionInput::RespondSubagent).
        request_id: ExternalSubagentRequestId,
    },
    /// The session reached a normal terminal outcome.
    Done,
    /// The session ended with a classified failure recorded as a message.
    Error {
        /// Stable, human-readable failure description.
        message: String,
    },
}

impl ExternalAgentCursor {
    /// Returns `true` when the cursor is the [`Idle`](Self::Idle) resting state.
    #[must_use]
    pub const fn is_idle(&self) -> bool {
        matches!(self, Self::Idle)
    }

    /// Returns `true` when the cursor has reached a terminal outcome.
    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        matches!(self, Self::Done | Self::Error { .. })
    }

    /// Returns the requirement address the cursor is stuck on, if any.
    ///
    /// Only the single-requirement awaiting variants
    /// ([`AwaitingSession`](Self::AwaitingSession) /
    /// [`AwaitingInteraction`](Self::AwaitingInteraction) /
    /// [`AwaitingSubagent`](Self::AwaitingSubagent)) carry one
    /// [`CursorRequirement`]. The [`AwaitingTool`](Self::AwaitingTool) batch
    /// awaits many requirements at once — read those through
    /// [`requirements`](Self::requirements).
    #[must_use]
    pub const fn requirement(&self) -> Option<&CursorRequirement> {
        match self {
            Self::AwaitingSession { requirement }
            | Self::AwaitingInteraction { requirement, .. }
            | Self::AwaitingSubagent { requirement, .. } => Some(requirement),
            Self::Idle | Self::AwaitingTool { .. } | Self::Done | Self::Error { .. } => None,
        }
    }

    /// Returns the outstanding tool-batch requirement addressing, if the cursor
    /// is parked on [`AwaitingTool`](Self::AwaitingTool).
    ///
    /// The returned [`ToolWaitRequirements`] maps every awaited
    /// [`ToolCallId`](crate::conversation::ToolCallId) to the
    /// [`RequirementId`](crate::agent::RequirementId) the driver must fulfill,
    /// so a restored machine can rebuild its pending-requirement registry from
    /// the persisted cursor alone.
    #[must_use]
    pub const fn requirements(&self) -> Option<&ToolWaitRequirements> {
        match self {
            Self::AwaitingTool { requirements, .. } => Some(requirements),
            Self::Idle
            | Self::AwaitingSession { .. }
            | Self::AwaitingInteraction { .. }
            | Self::AwaitingSubagent { .. }
            | Self::Done
            | Self::Error { .. } => None,
        }
    }

    /// Returns `true` when the cursor is parked on an outstanding host
    /// requirement — a session step, an interaction, a tool batch, or a subagent
    /// spawn — that a live runtime session may still back.
    ///
    /// Unlike [`requirement`](Self::requirement) (which only reports the
    /// single-requirement variants), this also covers the
    /// [`AwaitingTool`](Self::AwaitingTool) batch, so a never-resume abandon can
    /// flag every awaiting state for the handle layer to force-close (design
    /// §6.4).
    #[must_use]
    pub const fn has_outstanding_requirement(&self) -> bool {
        matches!(
            self,
            Self::AwaitingSession { .. }
                | Self::AwaitingInteraction { .. }
                | Self::AwaitingTool { .. }
                | Self::AwaitingSubagent { .. }
        )
    }
}

/// Data half of a running external-agent machine.
///
/// The state owns one active [`Conversation`] and records only resumable facts:
/// the static [`ExternalAgentSpec`], the [`ExternalSessionRef`] needed to realign
/// with the runtime across restarts, the active tool declarations, the recovery
/// [`ExternalAgentCursor`], the [`ExternalArtifactRef`] list a completed session
/// reported, and a pending-cleanup flag a never-resume abandon raises so the
/// handle layer knows it still owes an orphaned session a force-close (design
/// §6.4). Serialization crosses the Conversation persistence boundary via
/// [`Conversation::snapshot`]; deserialization rebuilds the live Conversation via
/// [`Conversation::restore`]. Runtime handles never appear in this shape.
#[derive(Debug)]
pub struct ExternalAgentState {
    spec: ExternalAgentSpec,
    conversation: Conversation,
    session: Option<ExternalSessionRef>,
    cursor: ExternalAgentCursor,
    active_tools: ToolSetRef,
    artifacts: Vec<ExternalArtifactRef>,
    cleanup_required: bool,
    decision_loops: u32,
}

impl ExternalAgentState {
    /// Creates external-agent state from a static spec and one active
    /// Conversation.
    ///
    /// The active tool set is seeded from the spec's initial tools, no session
    /// exists yet, and the cursor starts [`Idle`](ExternalAgentCursor::Idle).
    #[must_use]
    pub fn new(spec: ExternalAgentSpec, conversation: Conversation) -> Self {
        let active_tools = spec.initial_tools().clone();
        Self {
            spec,
            conversation,
            session: None,
            cursor: ExternalAgentCursor::Idle,
            active_tools,
            artifacts: Vec::new(),
            cleanup_required: false,
            decision_loops: 0,
        }
    }

    /// Returns the static external-agent specification.
    #[must_use]
    pub const fn spec(&self) -> &ExternalAgentSpec {
        &self.spec
    }

    /// Returns the unique active Conversation through a read-only view.
    #[must_use]
    pub const fn conversation(&self) -> &Conversation {
        &self.conversation
    }

    /// Returns the unique active Conversation for the machine's checked folds.
    ///
    /// The [`ExternalAgentMachine`](super::ExternalAgentMachine) uses this to
    /// open a turn on a fresh user message and to fold the runtime's terminal
    /// output back into committed history at a completed decision point. It stays
    /// crate-visible so only the machine's checked transitions mutate the
    /// Conversation, mirroring [`AgentState`](crate::agent::AgentState).
    pub(crate) const fn conversation_mut(&mut self) -> &mut Conversation {
        &mut self.conversation
    }

    /// Returns the resumable session facts, if a session has been established.
    #[must_use]
    pub const fn session(&self) -> Option<&ExternalSessionRef> {
        self.session.as_ref()
    }

    /// Returns the data-only recovery cursor.
    #[must_use]
    pub const fn cursor(&self) -> &ExternalAgentCursor {
        &self.cursor
    }

    /// Returns the currently active tool declarations.
    #[must_use]
    pub const fn active_tools(&self) -> &ToolSetRef {
        &self.active_tools
    }

    /// Replaces the recovery cursor.
    pub fn set_cursor(&mut self, cursor: ExternalAgentCursor) {
        self.cursor = cursor;
    }

    /// Records the latest resumable session facts.
    pub fn set_session(&mut self, session: Option<ExternalSessionRef>) {
        self.session = session;
    }

    /// Replaces the active tool declarations.
    pub fn set_active_tools(&mut self, active_tools: ToolSetRef) {
        self.active_tools = active_tools;
    }

    /// Returns the artifact references recorded from completed sessions, in the
    /// order they were reported.
    ///
    /// Each entry is only a redacted [`ExternalArtifactRef`] — a kind, an
    /// untrusted summary, and opaque path/reference handles — never the artifact
    /// content itself (full diff, test log, file blob), keeping large or
    /// sensitive payloads out of the persisted state (design §11, §12).
    #[must_use]
    pub fn artifacts(&self) -> &[ExternalArtifactRef] {
        &self.artifacts
    }

    /// Appends artifact references a completed session reported, in order.
    ///
    /// The [`ExternalAgentMachine`](super::ExternalAgentMachine) calls this when a
    /// session reaches [`Completed`](super::ExternalSessionResult::Completed) to
    /// fold [`ExternalAgentOutput::artifacts`](super::ExternalAgentOutput::artifacts)
    /// into the retained trace. Only the references are stored — never the
    /// underlying content — so the persisted state stays redaction-safe (design
    /// §12).
    pub fn record_artifacts<I>(&mut self, artifacts: I)
    where
        I: IntoIterator<Item = ExternalArtifactRef>,
    {
        self.artifacts.extend(artifacts);
    }

    /// Returns `true` when a never-resume abandon left an external session the
    /// handle layer still owes a force-close.
    ///
    /// A cancel abandons the machine's continuation while a runtime session may
    /// still be live (design §6.4). The machine cannot close the process itself
    /// (it is sans-io), so it flags the orphan here for the handle layer /
    /// session registry to sweep. The resumable [`session`](Self::session) facts
    /// are retained alongside the flag so the runtime can still be resumed if it
    /// supports it.
    #[must_use]
    pub const fn cleanup_required(&self) -> bool {
        self.cleanup_required
    }

    /// Flags that an external session needs a handle-layer force-close after a
    /// never-resume abandon (design §6.4).
    ///
    /// This never closes anything itself; it only records that the handle layer
    /// owes a sweep. The abandoning machine calls it instead of emitting a
    /// `Shutdown` effect, because a cancelled continuation is never stepped
    /// again.
    pub fn mark_cleanup_required(&mut self) {
        self.cleanup_required = true;
    }

    /// Clears the pending-cleanup flag once the handle layer has swept the
    /// session (or once a fresh session supersedes the orphaned one).
    pub fn clear_cleanup_required(&mut self) {
        self.cleanup_required = false;
    }

    /// Returns how many runtime decision loops (session round-trips) the machine
    /// has opened over its lifetime.
    ///
    /// Every time the machine hands control back to the runtime — the initial
    /// `Start`/`Continue`, and each `RespondToolResults` / `RespondInteraction` /
    /// `RespondSubagent` that reparks on a fresh `NeedExternalSession` — counts
    /// as one loop. The count is persisted so a restored machine keeps enforcing
    /// [`ExternalAgentMachineConfig::max_decision_loops`](super::ExternalAgentMachineConfig::max_decision_loops)
    /// across process boundaries (design §6.3).
    #[must_use]
    pub const fn decision_loops(&self) -> u32 {
        self.decision_loops
    }

    /// Records that the machine opened another runtime decision loop and returns
    /// the new running total.
    ///
    /// The counter saturates at [`u32::MAX`] rather than wrapping, so a machine
    /// that somehow reaches the ceiling keeps reporting an over-limit count
    /// instead of silently resetting.
    pub fn record_decision_loop(&mut self) -> u32 {
        self.decision_loops = self.decision_loops.saturating_add(1);
        self.decision_loops
    }

    fn from_record(record: ExternalAgentStateRecord) -> Result<Self, ConversationError> {
        let conversation = Conversation::restore(record.conversation)?;
        Ok(Self {
            spec: record.spec,
            conversation,
            session: record.session,
            cursor: record.cursor,
            active_tools: record.active_tools,
            artifacts: record.artifacts,
            cleanup_required: record.cleanup_required,
            decision_loops: record.decision_loops,
        })
    }
}

impl Serialize for ExternalAgentState {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let conversation = self.conversation.snapshot().map_err(ser::Error::custom)?;
        ExternalAgentStateRecord {
            spec: self.spec.clone(),
            conversation,
            session: self.session.clone(),
            cursor: self.cursor.clone(),
            active_tools: self.active_tools.clone(),
            artifacts: self.artifacts.clone(),
            cleanup_required: self.cleanup_required,
            decision_loops: self.decision_loops,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ExternalAgentState {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let record = ExternalAgentStateRecord::deserialize(deserializer)?;
        Self::from_record(record).map_err(de::Error::custom)
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExternalAgentStateRecord {
    spec: ExternalAgentSpec,
    conversation: ConversationSnapshot,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    session: Option<ExternalSessionRef>,
    #[serde(default)]
    cursor: ExternalAgentCursor,
    active_tools: ToolSetRef,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    artifacts: Vec<ExternalArtifactRef>,
    #[serde(default, skip_serializing_if = "is_false")]
    cleanup_required: bool,
    #[serde(default, skip_serializing_if = "is_zero")]
    decision_loops: u32,
}

/// Serde predicate: skips the pending-cleanup flag when it is not set, keeping
/// the common clean-state shape byte-for-byte compatible with the pre-M3-4
/// snapshot.
fn is_false(value: &bool) -> bool {
    !*value
}

/// Serde predicate: skips the decision-loop counter when it is still zero,
/// keeping a fresh clean-state snapshot byte-for-byte compatible with the
/// pre-M4-3 shape.
fn is_zero(value: &u32) -> bool {
    *value == 0
}

#[cfg(test)]
mod tests {
    use super::{ExternalAgentCursor, ExternalAgentState};
    use crate::{
        agent::{
            AgentId, AgentPath, AgentSlot, CursorRequirement, Interaction, RequirementId, StepId,
            ToolSetId, ToolWaitRequirements,
            external::{
                ExternalAgentSpec, ExternalArtifactKind, ExternalArtifactRef,
                ExternalPermissionMode, ExternalRuntimeKind, ExternalSessionPolicy,
                ExternalSessionRef, ExternalStreamPolicy, ExternalSubagentRequestId,
                ExternalToolBatchId, WorkerProfileRef, WorktreeIsolation,
            },
            spec::{ToolSetRef, WorktreeRef},
        },
        conversation::{
            AssistantFinish, Conversation, ConversationConfig, ConversationId, MessageId,
            ToolCallId, TurnId, TurnMeta,
        },
        model::{
            content::ContentBlock,
            message::{Message, Role},
            normalized::StopReason,
            tool::Tool,
            usage::Usage,
        },
    };
    use serde::{Serialize, de::DeserializeOwned};
    use serde_json::{Map, json};
    use std::collections::BTreeMap;
    use std::fmt::Debug;

    fn agent_id() -> AgentId {
        "018f0d9c-7b6a-7c12-8f31-1234567890f0"
            .parse()
            .expect("agent id")
    }

    fn tool_set_id() -> ToolSetId {
        "018f0d9c-7b6a-7c12-8f31-1234567890f1"
            .parse()
            .expect("tool set id")
    }

    fn requirement_id() -> RequirementId {
        "018f0d9c-7b6a-7c12-8f31-1234567890f2"
            .parse()
            .expect("requirement id")
    }

    fn tool_call_id() -> ToolCallId {
        "018f0d9c-7b6a-7c12-8f31-1234567890f3"
            .parse()
            .expect("tool call id")
    }

    fn message_id(offset: u8) -> MessageId {
        format!("018f0d9c-7b6a-7c12-8f31-1234567890a{offset}")
            .parse()
            .expect("message id")
    }

    fn paused_step_id() -> StepId {
        "018f0d9c-7b6a-7c12-8f31-1234567890e1"
            .parse()
            .expect("paused step id")
    }

    fn interaction() -> Interaction {
        Interaction::question(paused_step_id(), "Run `cargo test`?".to_owned())
    }

    fn tool(name: &str) -> Tool {
        Tool {
            name: name.to_owned(),
            description: format!("Tool {name}."),
            input_schema: json!({ "type": "object" }),
        }
    }

    fn spec() -> ExternalAgentSpec {
        ExternalAgentSpec::new(
            agent_id(),
            ExternalRuntimeKind::ClaudeCode,
            WorktreeRef::new("/repo/agent-lib"),
            Some(WorkerProfileRef::new("cheap-worker")),
            ToolSetRef::new(tool_set_id(), vec![tool("apply_patch")]),
            ExternalSessionPolicy {
                permission_mode: ExternalPermissionMode::AcceptEdits,
                isolation: WorktreeIsolation::EphemeralGitWorktree,
                max_turns: Some(12),
                stream_events: ExternalStreamPolicy::Buffered,
            },
        )
    }

    fn session_ref() -> ExternalSessionRef {
        ExternalSessionRef {
            runtime: ExternalRuntimeKind::ClaudeCode,
            session_id: Some("sess-77".to_owned()),
            transcript_ref: Some("transcript://77".to_owned()),
            resume_token: Some("resume-77".to_owned()),
            last_event_seq: Some(4),
        }
    }

    fn user_message(text: &str) -> Message {
        Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: text.to_owned(),
                extra: Map::new(),
            }],
        }
    }

    fn assistant_response(text: &str) -> crate::client::Response {
        crate::client::Response {
            message: Message {
                role: Role::Assistant,
                content: vec![ContentBlock::Text {
                    text: text.to_owned(),
                    extra: Map::new(),
                }],
            },
            usage: Usage::default(),
            stop_reason: StopReason::normalize("end_turn"),
            extra: Map::new(),
        }
    }

    fn committed_conversation() -> Conversation {
        let conversation_id: ConversationId = "018f0d9c-7b6a-7c12-8f31-1234567890fa"
            .parse()
            .expect("conversation id");
        let turn_id: TurnId = "018f0d9c-7b6a-7c12-8f31-1234567890fb"
            .parse()
            .expect("turn id");
        let mut conversation = Conversation::new(
            conversation_id,
            ConversationConfig::new(Some("Drive the external agent.".to_owned())),
        );
        conversation
            .begin_turn(turn_id, message_id(0), user_message("refactor the parser"))
            .expect("begin turn");
        conversation
            .start_assistant_response(assistant_response("on it"))
            .expect("assistant response");
        let finish = conversation
            .finish_assistant(message_id(1))
            .expect("finish assistant");
        assert_eq!(finish, AssistantFinish::ReadyToCommit);
        conversation
            .commit_pending(TurnMeta::default())
            .expect("commit pending");
        conversation
    }

    fn assert_json_round_trip<T>(value: &T)
    where
        T: Debug + PartialEq + Serialize + DeserializeOwned,
    {
        let encoded = serde_json::to_value(value).expect("serialize");
        let decoded: T = serde_json::from_value(encoded).expect("deserialize");
        assert_eq!(&decoded, value);
    }

    #[test]
    fn external_agent_state_serde_round_trips_through_conversation_snapshot() {
        let mut state = ExternalAgentState::new(spec(), committed_conversation());
        state.set_session(Some(session_ref()));
        state.set_active_tools(ToolSetRef::new(
            tool_set_id(),
            vec![tool("apply_patch"), tool("run_tests")],
        ));
        state.set_cursor(ExternalAgentCursor::AwaitingSession {
            requirement: CursorRequirement::root(requirement_id()),
        });

        let encoded = serde_json::to_value(&state).expect("serialize external agent state");
        assert_eq!(encoded["spec"]["id"], json!(agent_id().to_string()));
        assert_eq!(encoded["spec"]["runtime"], json!("claude_code"));
        assert_eq!(
            encoded["conversation"]["id"],
            json!("018f0d9c-7b6a-7c12-8f31-1234567890fa")
        );
        assert_eq!(
            encoded["conversation"]["history"]["raw_turns"]
                .as_array()
                .expect("raw turns array")
                .len(),
            1
        );
        assert_eq!(encoded["session"]["session_id"], json!("sess-77"));
        assert_eq!(encoded["cursor"]["state"], json!("awaiting_session"));
        assert_eq!(
            encoded["active_tools"]["tools"]
                .as_array()
                .expect("tools array")
                .len(),
            2
        );

        let object = encoded.as_object().expect("state object");
        for forbidden in [
            "runtime_handles",
            "interaction",
            "tool_registry",
            "session_tasks",
            "process",
            "task",
            "watcher",
        ] {
            assert!(
                !object.contains_key(forbidden),
                "runtime handle key must not be serialized: {forbidden}"
            );
        }

        let decoded: ExternalAgentState =
            serde_json::from_value(encoded).expect("deserialize external agent state");
        assert_eq!(decoded.spec().id(), agent_id());
        assert_eq!(decoded.spec().runtime(), &ExternalRuntimeKind::ClaudeCode);
        assert_eq!(
            decoded.spec().profile(),
            Some(&WorkerProfileRef::new("cheap-worker"))
        );
        assert_eq!(
            decoded.conversation().id().to_string(),
            "018f0d9c-7b6a-7c12-8f31-1234567890fa"
        );
        assert_eq!(decoded.conversation().turns().len(), 1);
        assert_eq!(decoded.session(), Some(&session_ref()));
        assert_eq!(decoded.active_tools().tools().len(), 2);
        assert_eq!(
            decoded.cursor(),
            &ExternalAgentCursor::AwaitingSession {
                requirement: CursorRequirement::root(requirement_id()),
            }
        );
    }

    #[test]
    fn external_agent_state_defaults_to_idle_without_session() {
        let state = ExternalAgentState::new(spec(), committed_conversation());
        assert!(state.cursor().is_idle());
        assert!(state.session().is_none());
        assert_eq!(state.active_tools().tools().len(), 1);

        let encoded = serde_json::to_value(&state).expect("serialize external agent state");
        assert!(
            encoded
                .as_object()
                .expect("object")
                .get("session")
                .is_none(),
            "absent session must be skipped"
        );
        assert_eq!(encoded["cursor"]["state"], json!("idle"));

        let decoded: ExternalAgentState =
            serde_json::from_value(encoded).expect("deserialize external agent state");
        assert!(decoded.cursor().is_idle());
        assert!(decoded.session().is_none());
    }

    #[test]
    fn external_agent_state_decision_loops_persist_and_skip_when_zero() {
        // A fresh state has opened no decision loops, and the zero counter is
        // omitted from the snapshot so the clean-state shape stays compatible.
        let mut state = ExternalAgentState::new(spec(), committed_conversation());
        assert_eq!(state.decision_loops(), 0);
        let encoded = serde_json::to_value(&state).expect("serialize external agent state");
        assert!(
            encoded
                .as_object()
                .expect("object")
                .get("decision_loops")
                .is_none(),
            "a zero decision-loop counter must be skipped"
        );

        // Recording loops advances the running total, which then rides the
        // snapshot and survives a round-trip so a restored machine keeps
        // enforcing the loop bound.
        assert_eq!(state.record_decision_loop(), 1);
        assert_eq!(state.record_decision_loop(), 2);
        let encoded = serde_json::to_value(&state).expect("serialize external agent state");
        assert_eq!(encoded["decision_loops"], json!(2));
        let decoded: ExternalAgentState =
            serde_json::from_value(encoded).expect("deserialize external agent state");
        assert_eq!(decoded.decision_loops(), 2);
    }

    #[test]
    fn external_agent_state_cursor_variants_round_trip() {
        let origin = AgentPath::from_slots(vec![AgentSlot::new(1), AgentSlot::new(3)]);
        let tool_requirements = ToolWaitRequirements::new(origin.clone(), {
            let mut ids = BTreeMap::new();
            ids.insert(tool_call_id(), requirement_id());
            ids
        });
        for cursor in [
            ExternalAgentCursor::Idle,
            ExternalAgentCursor::AwaitingSession {
                requirement: CursorRequirement::root(requirement_id()),
            },
            ExternalAgentCursor::AwaitingInteraction {
                requirement: CursorRequirement::new(requirement_id(), origin.clone()),
                pending_action: "act-9".to_owned(),
                interaction: interaction(),
            },
            ExternalAgentCursor::AwaitingTool {
                batch_id: ExternalToolBatchId::new("batch-7"),
                requirements: tool_requirements.clone(),
            },
            ExternalAgentCursor::AwaitingSubagent {
                requirement: CursorRequirement::root(requirement_id()),
                request_id: ExternalSubagentRequestId::new("spawn-3"),
            },
            ExternalAgentCursor::Done,
            ExternalAgentCursor::Error {
                message: "runtime crashed".to_owned(),
            },
        ] {
            assert_json_round_trip(&cursor);
        }

        assert!(ExternalAgentCursor::Idle.is_idle());
        assert!(ExternalAgentCursor::Done.is_terminal());
        assert!(
            ExternalAgentCursor::Error {
                message: "x".to_owned()
            }
            .is_terminal()
        );
        assert_eq!(
            ExternalAgentCursor::AwaitingInteraction {
                requirement: CursorRequirement::root(requirement_id()),
                pending_action: "act-1".to_owned(),
                interaction: interaction(),
            }
            .requirement()
            .map(CursorRequirement::id),
            Some(requirement_id())
        );

        // The tool-batch cursor awaits many requirements, so the single-value
        // `requirement()` accessor reports none while `requirements()` surfaces
        // the whole `ToolCallId -> RequirementId` binding. A restored driver
        // rebuilds its pending registry from that binding alone.
        let awaiting_tool = ExternalAgentCursor::AwaitingTool {
            batch_id: ExternalToolBatchId::new("batch-7"),
            requirements: tool_requirements.clone(),
        };
        assert!(!awaiting_tool.is_terminal());
        assert!(awaiting_tool.has_outstanding_requirement());
        assert!(awaiting_tool.requirement().is_none());
        assert_eq!(awaiting_tool.requirements(), Some(&tool_requirements));

        // The single-requirement awaiting variants and the resting states agree
        // with `has_outstanding_requirement()`.
        assert!(
            ExternalAgentCursor::AwaitingSession {
                requirement: CursorRequirement::root(requirement_id()),
            }
            .has_outstanding_requirement()
        );

        // The subagent cursor carries the single outstanding requirement plus
        // the runtime request id echoed back on `RespondSubagent`, so it reports
        // through `requirement()` and counts as an outstanding requirement.
        let awaiting_subagent = ExternalAgentCursor::AwaitingSubagent {
            requirement: CursorRequirement::root(requirement_id()),
            request_id: ExternalSubagentRequestId::new("spawn-3"),
        };
        assert!(!awaiting_subagent.is_terminal());
        assert!(awaiting_subagent.has_outstanding_requirement());
        assert_eq!(
            awaiting_subagent.requirement().map(CursorRequirement::id),
            Some(requirement_id())
        );
        assert!(awaiting_subagent.requirements().is_none());

        assert!(!ExternalAgentCursor::Idle.has_outstanding_requirement());
        assert!(!ExternalAgentCursor::Done.has_outstanding_requirement());
    }

    #[test]
    fn cleanup_required_flag_round_trips_and_is_skipped_when_clear() {
        let mut state = ExternalAgentState::new(spec(), committed_conversation());
        assert!(!state.cleanup_required());

        // A clean state omits the flag entirely, keeping the pre-M3-4 shape.
        let clean = serde_json::to_value(&state).expect("serialize clean state");
        assert!(
            clean
                .as_object()
                .expect("object")
                .get("cleanup_required")
                .is_none(),
            "clear cleanup flag must be skipped"
        );

        state.mark_cleanup_required();
        assert!(state.cleanup_required());

        let flagged = serde_json::to_value(&state).expect("serialize flagged state");
        assert_eq!(flagged["cleanup_required"], json!(true));

        let decoded: ExternalAgentState =
            serde_json::from_value(flagged).expect("deserialize flagged state");
        assert!(decoded.cleanup_required());

        let mut swept = decoded;
        swept.clear_cleanup_required();
        assert!(!swept.cleanup_required());
    }

    #[test]
    fn recorded_artifacts_accumulate_and_round_trip_and_skip_when_empty() {
        let mut state = ExternalAgentState::new(spec(), committed_conversation());
        assert!(state.artifacts().is_empty());

        // An empty artifact list is skipped in the snapshot, preserving the
        // pre-M5-3 shape.
        let clean = serde_json::to_value(&state).expect("serialize clean state");
        assert!(
            clean
                .as_object()
                .expect("object")
                .get("artifacts")
                .is_none(),
            "an empty artifact list must be skipped"
        );

        let patch = ExternalArtifactRef {
            kind: ExternalArtifactKind::Patch,
            summary: "refactor".to_owned(),
            path: Some("src/parser.rs".to_owned()),
            reference: Some("blob://diff-1".to_owned()),
        };
        let test_result = ExternalArtifactRef {
            kind: ExternalArtifactKind::TestResult,
            summary: "12 passed".to_owned(),
            path: None,
            reference: Some("blob://test-1".to_owned()),
        };

        // Recording accumulates across calls, preserving order.
        state.record_artifacts([patch.clone()]);
        state.record_artifacts([test_result.clone()]);
        assert_eq!(state.artifacts(), [patch.clone(), test_result.clone()]);

        let encoded = serde_json::to_value(&state).expect("serialize state");
        let decoded: ExternalAgentState =
            serde_json::from_value(encoded).expect("deserialize state");
        assert_eq!(decoded.artifacts(), [patch, test_result]);
    }
}
