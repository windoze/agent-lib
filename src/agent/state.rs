//! Serializable Agent runtime state and loop cursor data.
//!
//! [`AgentState`] is the data half of a running Agent. It owns exactly one live
//! [`Conversation`], records active skills and queued boundary work, and keeps a
//! data-only [`LoopCursor`] for pause/restore. Runtime handles such as clients,
//! registries, responders, sessions, and task handles live in
//! [`AgentRuntimeHandles`] instead of this serde shape.

mod cursor;
mod queue;
mod runtime;

#[cfg(test)]
mod tests;

use crate::{
    agent::{AgentId, AgentPath, AgentSpec, LoopPolicy, ModelRef, SkillId, ToolSetId, ToolSetRef},
    conversation::{Conversation, ConversationError, ConversationSnapshot, ToolCallId},
    model::{extras::ProviderExtras, message::Role, tool::Tool},
};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de, ser};
use std::collections::BTreeSet;
use thiserror::Error;

pub use cursor::{
    ApprovalCursor, CancelRecoveryCursor, CancelRecoveryReason, CursorRequirement, DoneCursor,
    ErrorCursor, ErrorCursorKind, LoopCursor, LoopCursorKind, LoopDoneReason, ReconfigCursor,
    StepCursor, ToolWaitCursor, ToolWaitRequirements,
};
pub use queue::{
    PivotSource, QueuedPivot, QueuedReconfig, ReconfigQueue, ReconfigRequest, ToolSetPatch,
};
pub(crate) use queue::{reconfig_boundary_metadata, reconfig_boundary_records};
pub use runtime::AgentRuntimeHandles;

/// Data half of a running Agent.
///
/// The state owns one active [`Conversation`] and never exposes a public setter
/// that can swap it out. Serialization crosses the Conversation persistence
/// boundary by calling [`Conversation::snapshot`]; deserialization rebuilds the
/// live Conversation by calling [`Conversation::restore`].
#[derive(Debug)]
pub struct AgentState {
    spec: AgentSpec,
    conversation: Conversation,
    active_skills: Vec<SkillId>,
    queued_reconfigs: ReconfigQueue,
    system_prompt_overlay: Option<String>,
    system_prompt_overlay_version: u64,
    current_tool_set: ToolSetRef,
    current_model: ModelRef,
    current_loop_policy: LoopPolicy,
    loop_cursor: LoopCursor,
}

impl AgentState {
    /// Creates Agent state from a static spec and one active Conversation.
    #[must_use]
    pub fn new(spec: AgentSpec, conversation: Conversation) -> Self {
        let current_tool_set = spec.initial_tools().clone();
        let current_model = spec.model().clone();
        let current_loop_policy = *spec.loop_policy();
        Self {
            spec,
            conversation,
            active_skills: Vec::new(),
            queued_reconfigs: ReconfigQueue::new(),
            system_prompt_overlay: None,
            system_prompt_overlay_version: 0,
            current_tool_set,
            current_model,
            current_loop_policy,
            loop_cursor: LoopCursor::Idle,
        }
    }

    /// Returns the static Agent specification stored with this state.
    #[must_use]
    pub const fn spec(&self) -> &AgentSpec {
        &self.spec
    }

    /// Returns the Agent identity embedded in the static specification.
    #[must_use]
    pub const fn spec_id(&self) -> AgentId {
        self.spec.id()
    }

    /// Returns the unique active Conversation through a read-only view.
    #[must_use]
    pub const fn conversation(&self) -> &Conversation {
        &self.conversation
    }

    /// Returns the unique active Conversation to crate-internal checked drivers.
    pub(crate) const fn conversation_mut(&mut self) -> &mut Conversation {
        &mut self.conversation
    }

    /// Returns active skill identities in their caller-controlled order.
    #[must_use]
    pub fn active_skills(&self) -> &[SkillId] {
        &self.active_skills
    }

    /// Returns queued reconfiguration intents waiting for a turn boundary.
    #[must_use]
    pub fn queued_reconfigs(&self) -> &[QueuedReconfig] {
        self.queued_reconfigs.as_slice()
    }

    /// Returns the currently effective system-prompt overlay, if any.
    #[must_use]
    pub fn system_prompt_overlay(&self) -> Option<&str> {
        self.system_prompt_overlay.as_deref()
    }

    /// Returns the optimistic version for the system-prompt overlay.
    #[must_use]
    pub const fn system_prompt_overlay_version(&self) -> u64 {
        self.system_prompt_overlay_version
    }

    /// Returns the currently effective tool declarations.
    #[must_use]
    pub const fn current_tool_set(&self) -> &ToolSetRef {
        &self.current_tool_set
    }

    /// Returns the currently effective model request settings.
    #[must_use]
    pub const fn current_model(&self) -> &ModelRef {
        &self.current_model
    }

    /// Overrides provider-specific request extras on the effective model.
    ///
    /// Restore builders use this to re-bind a data-only snapshot to runtime
    /// provider settings without changing the restored conversation or tools.
    pub(crate) fn override_current_provider_extras(&mut self, provider_extras: ProviderExtras) {
        self.current_model = self
            .current_model
            .clone()
            .with_provider_extras(provider_extras);
    }

    /// Returns the currently effective loop policy.
    #[must_use]
    pub const fn current_loop_policy(&self) -> &LoopPolicy {
        &self.current_loop_policy
    }

    /// Returns the data-only loop recovery cursor.
    #[must_use]
    pub const fn loop_cursor(&self) -> &LoopCursor {
        &self.loop_cursor
    }

    /// Replaces the active skill list after checking for duplicate identities.
    ///
    /// # Errors
    ///
    /// Returns [`AgentStateError::DuplicateSkill`] when the same skill id
    /// appears more than once.
    pub fn replace_active_skills(
        &mut self,
        active_skills: Vec<SkillId>,
    ) -> Result<(), AgentStateError> {
        ensure_unique_skill_ids(&active_skills)?;
        self.active_skills = active_skills;
        Ok(())
    }

    /// Queues a turn-boundary reconfiguration intent.
    ///
    /// # Errors
    ///
    /// Returns [`AgentStateError::ReconfigWhileAwaitingRegistry`] when the
    /// cursor is [`LoopCursor::AwaitingReconfig`] (a previous reconfiguration
    /// is parked behind the registry effect; the caller may retry once that
    /// requirement resolves). Returns a classified [`AgentStateError`] when
    /// the request or the full pending queue would conflict with the current
    /// Agent state.
    pub fn queue_reconfig(&mut self, reconfig: QueuedReconfig) -> Result<(), AgentStateError> {
        self.ensure_reconfig_admission()?;
        let queue = self.queued_reconfigs.with_pushed(reconfig);
        let _application = self.plan_reconfig_requests(queue.as_slice())?;
        self.queued_reconfigs = queue;
        Ok(())
    }

    /// Gates reconfiguration admission while a previous one is parked.
    ///
    /// While the cursor is [`LoopCursor::AwaitingReconfig`], the resume path
    /// applies the application planned at park time and clears the queue, so
    /// any request admitted during the park would be silently dropped without
    /// ever being applied. Rejecting at admission keeps the queue exactly the
    /// set the parked application was planned from; the caller retries once
    /// the outstanding reconfig requirement resolves (H-STATE-5 / M4-2).
    pub(crate) fn ensure_reconfig_admission(&self) -> Result<(), AgentStateError> {
        if matches!(self.loop_cursor, LoopCursor::AwaitingReconfig(_)) {
            return Err(AgentStateError::ReconfigWhileAwaitingRegistry);
        }
        Ok(())
    }

    pub(crate) fn plan_reconfig_with(
        &self,
        reconfig: &ReconfigRequest,
    ) -> Result<ReconfigApplication, AgentStateError> {
        let queue = self.queued_reconfigs.with_pushed(reconfig.clone());
        self.plan_reconfig_requests(queue.as_slice())
    }

    pub(crate) fn queue_prevalidated_reconfig(&mut self, reconfig: ReconfigRequest) {
        let queue = self.queued_reconfigs.with_pushed(reconfig);
        self.queued_reconfigs = queue;
    }

    pub(crate) fn queued_reconfig_application(
        &self,
    ) -> Result<Option<ReconfigApplication>, AgentStateError> {
        if self.queued_reconfigs.is_empty() {
            Ok(None)
        } else {
            self.plan_reconfig_requests(self.queued_reconfigs.as_slice())
                .map(Some)
        }
    }

    pub(crate) fn apply_reconfig_application(&mut self, application: ReconfigApplication) {
        self.active_skills = application.active_skills;
        self.system_prompt_overlay = application.system_prompt_overlay;
        self.system_prompt_overlay_version = application.system_prompt_overlay_version;
        self.current_tool_set = application.current_tool_set;
        self.current_model = application.current_model;
        self.current_loop_policy = application.current_loop_policy;
        self.queued_reconfigs.clear();
    }

    fn plan_reconfig_requests(
        &self,
        requests: &[ReconfigRequest],
    ) -> Result<ReconfigApplication, AgentStateError> {
        let mut application = ReconfigApplication {
            requests: requests.to_vec(),
            active_skills: self.active_skills.clone(),
            system_prompt_overlay: self.system_prompt_overlay.clone(),
            system_prompt_overlay_version: self.system_prompt_overlay_version,
            current_tool_set: self.current_tool_set.clone(),
            current_model: self.current_model.clone(),
            current_loop_policy: self.current_loop_policy,
        };

        for request in requests {
            request.validate()?;
            apply_reconfig_request(&mut application, request)?;
        }

        ensure_unique_skill_ids(&application.active_skills)?;
        ensure_unique_tool_names(application.current_tool_set.tools())?;
        Ok(application)
    }

    /// Advances the loop cursor through a checked state transition.
    ///
    /// # Errors
    ///
    /// Returns [`AgentStateError::InvalidCursorTransition`] when the requested
    /// transition skips a required recovery point or attempts to leave a
    /// terminal cursor. Returns other [`AgentStateError`] variants if the target
    /// cursor data is invalid.
    pub fn transition_cursor(&mut self, next: LoopCursor) -> Result<(), AgentStateError> {
        next.validate()?;
        if !self.loop_cursor.can_transition_to(&next) {
            return Err(AgentStateError::InvalidCursorTransition {
                from: self.loop_cursor.kind(),
                to: next.kind(),
            });
        }
        self.loop_cursor = next;
        Ok(())
    }

    /// Re-stamps the loop cursor's requirement binding origin to `base`.
    ///
    /// Used by a nested machine to record the real [`AgentPath`] of the node
    /// that owns this state: the sans-io machine always stamps its cursor
    /// binding at the root, so the enclosing node re-bases it to the node's
    /// absolute path (migration doc §7.1). This only rewrites addressing
    /// metadata, so it does not go through transition validation.
    pub(crate) fn rebase_cursor_origin(&mut self, base: &AgentPath) {
        self.loop_cursor.rebase_origin(base);
    }

    fn from_record(record: AgentStateRecord) -> Result<Self, AgentStateError> {
        ensure_unique_skill_ids(&record.active_skills)?;
        let current_tool_set = record
            .current_tool_set
            .unwrap_or_else(|| record.spec.initial_tools().clone());
        ensure_unique_tool_names(current_tool_set.tools())?;
        let current_model = record
            .current_model
            .unwrap_or_else(|| record.spec.model().clone());
        let current_loop_policy = record
            .current_loop_policy
            .unwrap_or(*record.spec.loop_policy());
        let queued_reconfigs = record.queued_reconfigs;
        for reconfig in queued_reconfigs.as_slice() {
            reconfig.validate()?;
        }
        record.loop_cursor.validate()?;

        let conversation = Conversation::restore(record.conversation)?;
        let state = Self {
            spec: record.spec,
            conversation,
            active_skills: record.active_skills,
            queued_reconfigs,
            system_prompt_overlay: record.system_prompt_overlay,
            system_prompt_overlay_version: record.system_prompt_overlay_version.unwrap_or(0),
            current_tool_set,
            current_model,
            current_loop_policy,
            loop_cursor: record.loop_cursor,
        };
        let _application = state
            .queued_reconfig_application()?
            .map(|application| application.requests().len());
        Ok(state)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ReconfigApplication {
    requests: Vec<ReconfigRequest>,
    active_skills: Vec<SkillId>,
    system_prompt_overlay: Option<String>,
    system_prompt_overlay_version: u64,
    current_tool_set: ToolSetRef,
    current_model: ModelRef,
    current_loop_policy: LoopPolicy,
}

impl ReconfigApplication {
    pub(crate) fn requests(&self) -> &[ReconfigRequest] {
        &self.requests
    }

    pub(crate) const fn current_tool_set(&self) -> &ToolSetRef {
        &self.current_tool_set
    }
}

fn apply_reconfig_request(
    application: &mut ReconfigApplication,
    request: &ReconfigRequest,
) -> Result<(), AgentStateError> {
    match request {
        ReconfigRequest::ActivateSkill { skill_id } => {
            if application.active_skills.contains(skill_id) {
                return Err(AgentStateError::SkillAlreadyActive {
                    skill_id: *skill_id,
                });
            }
            application.active_skills.push(*skill_id);
        }
        ReconfigRequest::DeactivateSkill { skill_id } => {
            let Some(index) = application
                .active_skills
                .iter()
                .position(|active| active == skill_id)
            else {
                return Err(AgentStateError::SkillNotActive {
                    skill_id: *skill_id,
                });
            };
            application.active_skills.remove(index);
        }
        ReconfigRequest::ReplaceActiveSkills { skill_ids } => {
            ensure_unique_skill_ids(skill_ids)?;
            application.active_skills = skill_ids.clone();
        }
        ReconfigRequest::SetSystemPromptOverlay {
            system_prompt,
            expected_version,
        } => {
            if *expected_version != application.system_prompt_overlay_version {
                return Err(AgentStateError::SystemOverlayVersionConflict {
                    expected: *expected_version,
                    actual: application.system_prompt_overlay_version,
                });
            }
            application.system_prompt_overlay = system_prompt.clone();
            application.system_prompt_overlay_version = application
                .system_prompt_overlay_version
                .checked_add(1)
                .ok_or(AgentStateError::SystemOverlayVersionOverflow)?;
        }
        ReconfigRequest::ReplaceToolSet { tool_set } => {
            ensure_unique_tool_names(tool_set.tools())?;
            application.current_tool_set = tool_set.clone();
        }
        ReconfigRequest::PatchToolSet { patch } => {
            application.current_tool_set =
                apply_tool_set_patch(&application.current_tool_set, patch)?;
        }
        ReconfigRequest::SetModel { model } => {
            application.current_model = model.clone();
        }
        ReconfigRequest::SetLoopPolicy { loop_policy } => {
            application.current_loop_policy = *loop_policy;
        }
    }
    Ok(())
}

fn apply_tool_set_patch(
    current: &ToolSetRef,
    patch: &ToolSetPatch,
) -> Result<ToolSetRef, AgentStateError> {
    if patch.expected_tool_set_id() != current.id() {
        return Err(AgentStateError::ToolSetVersionConflict {
            expected: patch.expected_tool_set_id(),
            actual: current.id(),
        });
    }

    let mut tools = current.tools().to_vec();
    for name in patch.remove() {
        let Some(index) = tools.iter().position(|tool| &tool.name == name) else {
            return Err(AgentStateError::UnknownToolName { name: name.clone() });
        };
        tools.remove(index);
    }

    for tool in patch.add_or_replace() {
        if let Some(existing) = tools.iter_mut().find(|existing| existing.name == tool.name) {
            *existing = tool.clone();
        } else {
            tools.push(tool.clone());
        }
    }

    ensure_unique_tool_names(&tools)?;
    Ok(ToolSetRef::new(patch.resulting_tool_set_id(), tools))
}

impl Serialize for AgentState {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let conversation = self.conversation.snapshot().map_err(ser::Error::custom)?;
        let current_tool_set = (&self.current_tool_set != self.spec.initial_tools())
            .then(|| self.current_tool_set.clone());
        let current_model =
            (&self.current_model != self.spec.model()).then(|| self.current_model.clone());
        let current_loop_policy = (&self.current_loop_policy != self.spec.loop_policy())
            .then_some(self.current_loop_policy);
        let system_prompt_overlay_version =
            (self.system_prompt_overlay_version != 0).then_some(self.system_prompt_overlay_version);
        AgentStateRecord {
            spec: self.spec.clone(),
            conversation,
            active_skills: self.active_skills.clone(),
            queued_reconfigs: self.queued_reconfigs.clone(),
            system_prompt_overlay: self.system_prompt_overlay.clone(),
            system_prompt_overlay_version,
            current_tool_set,
            current_model,
            current_loop_policy,
            loop_cursor: self.loop_cursor.clone(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for AgentState {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let record = AgentStateRecord::deserialize(deserializer)?;
        Self::from_record(record).map_err(de::Error::custom)
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentStateRecord {
    spec: AgentSpec,
    conversation: ConversationSnapshot,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    active_skills: Vec<SkillId>,
    #[serde(default, skip_serializing_if = "ReconfigQueue::is_empty")]
    queued_reconfigs: ReconfigQueue,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    system_prompt_overlay: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    system_prompt_overlay_version: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    current_tool_set: Option<ToolSetRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    current_model: Option<ModelRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    current_loop_policy: Option<LoopPolicy>,
    #[serde(default)]
    loop_cursor: LoopCursor,
}

/// Agent state validation and transition failures.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum AgentStateError {
    /// A Conversation snapshot or restore operation failed.
    #[error(transparent)]
    Conversation(#[from] ConversationError),
    /// An active skill list repeated the same skill identity.
    #[error("skill id {skill_id} appears more than once")]
    DuplicateSkill {
        /// Repeated skill identity.
        skill_id: SkillId,
    },
    /// A reconfiguration attempted to activate an already-active skill.
    #[error("skill id {skill_id} is already active")]
    SkillAlreadyActive {
        /// Already-active skill identity.
        skill_id: SkillId,
    },
    /// A reconfiguration attempted to deactivate an inactive skill.
    #[error("skill id {skill_id} is not active")]
    SkillNotActive {
        /// Inactive skill identity.
        skill_id: SkillId,
    },
    /// A tool declaration list repeated a tool name.
    #[error("tool name `{name}` appears more than once")]
    DuplicateToolName {
        /// Repeated tool name.
        name: String,
    },
    /// A tool-set patch targeted a stale current tool-set identity.
    #[error("tool set version conflict: expected {expected}, found {actual}")]
    ToolSetVersionConflict {
        /// Caller-observed tool-set identity.
        expected: ToolSetId,
        /// Current tool-set identity.
        actual: ToolSetId,
    },
    /// A tool-set patch attempted to remove a tool that is not present.
    #[error("tool `{name}` is not present in the current tool set")]
    UnknownToolName {
        /// Missing tool name.
        name: String,
    },
    /// A system overlay request used a stale overlay version.
    #[error("system overlay version conflict: expected {expected}, found {actual}")]
    SystemOverlayVersionConflict {
        /// Caller-observed overlay version.
        expected: u64,
        /// Current overlay version.
        actual: u64,
    },
    /// A system overlay version counter overflowed.
    #[error("system overlay version overflow")]
    SystemOverlayVersionOverflow,
    /// A tool-wait cursor had no tool calls to wait for.
    #[error("awaiting-tool cursor must contain at least one tool call")]
    EmptyToolWait,
    /// A tool-wait cursor repeated a framework tool-call identity.
    #[error("tool call id {call_id} appears more than once in the cursor")]
    DuplicateToolCall {
        /// Repeated tool-call identity.
        call_id: ToolCallId,
    },
    /// A tool-wait cursor's requirement binding did not cover the tool-call set exactly.
    #[error("tool call id {call_id} is not consistently bound to a requirement in the cursor")]
    ToolRequirementMismatch {
        /// Tool-call identity that is unbound or bound without an awaited call.
        call_id: ToolCallId,
    },
    /// A pivot attempted to inject a non-user message.
    #[error("queued pivot messages must use Role::User, found {actual:?}")]
    InvalidPivotRole {
        /// Role carried by the invalid pivot payload.
        actual: Role,
    },
    /// A cursor transition skipped a valid recovery state or left a terminal state.
    #[error("illegal LoopCursor transition from {from:?} to {to:?}")]
    InvalidCursorTransition {
        /// Current cursor kind.
        from: LoopCursorKind,
        /// Requested cursor kind.
        to: LoopCursorKind,
    },
    /// An error cursor did not carry stable diagnostic text.
    #[error("error cursor message must not be empty")]
    EmptyCursorError,
    /// A reconfiguration was requested while a previous one is parked awaiting
    /// registry resolution.
    #[error(
        "cannot queue a reconfiguration while the cursor is `AwaitingReconfig`; \
         retry after the outstanding reconfig requirement resolves"
    )]
    ReconfigWhileAwaitingRegistry,
}

fn ensure_unique_skill_ids(skill_ids: &[SkillId]) -> Result<(), AgentStateError> {
    if let Some(skill_id) = first_duplicate(skill_ids) {
        Err(AgentStateError::DuplicateSkill { skill_id })
    } else {
        Ok(())
    }
}

fn ensure_unique_tool_names(tools: &[Tool]) -> Result<(), AgentStateError> {
    let mut seen = BTreeSet::new();
    for tool in tools {
        if !seen.insert(tool.name.clone()) {
            return Err(AgentStateError::DuplicateToolName {
                name: tool.name.clone(),
            });
        }
    }
    Ok(())
}

fn first_duplicate<T>(items: &[T]) -> Option<T>
where
    T: Copy + Ord,
{
    let mut seen = BTreeSet::new();
    items.iter().copied().find(|item| !seen.insert(*item))
}
