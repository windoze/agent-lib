//! Data-only queued Agent boundary work.

use crate::{
    agent::{AgentId, LoopPolicy, ModelRef, SkillId, ToolSetId, ToolSetRef},
    conversation::{MessageId, MessageMeta},
    model::{
        message::{Message, Role},
        tool::Tool,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::Map;

use super::{AgentStateError, ensure_unique_skill_ids, ensure_unique_tool_names};

/// User-role message queued for a future step boundary.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueuedPivot {
    message_id: MessageId,
    message: Message,
    source: PivotSource,
}

impl QueuedPivot {
    /// Creates a queued pivot from caller-supplied message identity and payload.
    ///
    /// # Errors
    ///
    /// Returns [`AgentStateError::InvalidPivotRole`] when the payload role is
    /// not [`Role::User`].
    pub fn new(
        message_id: MessageId,
        message: Message,
        source: PivotSource,
    ) -> Result<Self, AgentStateError> {
        let pivot = Self {
            message_id,
            message,
            source,
        };
        pivot.validate()?;
        Ok(pivot)
    }

    /// Returns the Conversation message identity to use at injection time.
    #[must_use]
    pub const fn message_id(&self) -> MessageId {
        self.message_id
    }

    /// Returns the complete user message payload.
    #[must_use]
    pub const fn message(&self) -> &Message {
        &self.message
    }

    /// Returns the source metadata for this pivot.
    #[must_use]
    pub const fn source(&self) -> &PivotSource {
        &self.source
    }

    /// Builds the [`MessageMeta`] used when this pivot is injected at a checked
    /// pending step boundary.
    ///
    /// The meta records the pivot source both as a stable diagnostic label
    /// (via [`PivotSource::label`]) and as structured `pivot_source` extra data.
    #[must_use]
    pub fn message_meta(&self) -> MessageMeta {
        let mut extra = Map::new();
        extra.insert(
            "pivot_source".to_owned(),
            serde_json::to_value(&self.source).expect("pivot source serializes"),
        );
        MessageMeta::new(Some(self.source.label()), extra)
    }

    pub(crate) fn validate(&self) -> Result<(), AgentStateError> {
        if self.message.role != Role::User {
            return Err(AgentStateError::InvalidPivotRole {
                actual: self.message.role,
            });
        }
        Ok(())
    }
}

/// Data-only source metadata for a queued pivot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", content = "data", rename_all = "snake_case")]
pub enum PivotSource {
    /// A human user supplied the pivot.
    Human,
    /// A coordinator Agent supplied the pivot.
    Coordinator {
        /// Coordinator Agent identity.
        agent_id: AgentId,
    },
    /// An active skill supplied the pivot.
    Skill {
        /// Skill identity that produced the pivot.
        skill_id: SkillId,
    },
    /// Host application code supplied the pivot.
    Host {
        /// Stable host-side label for diagnostics.
        label: String,
    },
}

impl PivotSource {
    /// Returns a stable, human-readable diagnostic label for this source.
    ///
    /// The label is recorded on the injected message meta so tracing can
    /// attribute a pivot to the human, coordinator, skill, or host that
    /// produced it.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::Human => "pivot:human".to_owned(),
            Self::Coordinator { agent_id } => format!("pivot:coordinator:{agent_id}"),
            Self::Skill { skill_id } => format!("pivot:skill:{skill_id}"),
            Self::Host { label } => format!("pivot:host:{label}"),
        }
    }
}

/// FIFO queue of turn-boundary reconfiguration requests.
///
/// The queue is a data-only shape. Applying it is a separate checked
/// transaction on [`super::AgentState`] so validation can happen before any
/// active runtime registry is replaced.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ReconfigQueue {
    requests: Vec<ReconfigRequest>,
}

impl ReconfigQueue {
    /// Creates an empty reconfiguration queue.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            requests: Vec::new(),
        }
    }

    /// Returns queued requests in FIFO order.
    #[must_use]
    pub fn as_slice(&self) -> &[ReconfigRequest] {
        &self.requests
    }

    /// Returns whether no requests are queued.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.requests.is_empty()
    }

    /// Returns the number of queued requests.
    #[must_use]
    pub fn len(&self) -> usize {
        self.requests.len()
    }

    pub(super) fn with_pushed(&self, request: ReconfigRequest) -> Self {
        let mut requests = self.requests.clone();
        requests.push(request);
        Self { requests }
    }

    pub(super) fn clear(&mut self) {
        self.requests.clear();
    }
}

/// Turn-boundary reconfiguration request.
///
/// Reconfiguration data records future changes to skills, system prompt
/// overlays, model settings, loop policy, or tool declarations. It
/// intentionally stores declarations and version tokens, not a live tool
/// registry.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum ReconfigRequest {
    /// Activate one skill at the next turn boundary.
    ActivateSkill {
        /// Skill identity to activate.
        skill_id: SkillId,
    },
    /// Deactivate one skill at the next turn boundary.
    DeactivateSkill {
        /// Skill identity to deactivate.
        skill_id: SkillId,
    },
    /// Replace the complete active skill list at the next turn boundary.
    ReplaceActiveSkills {
        /// Replacement skill list in caller-controlled order.
        skill_ids: Vec<SkillId>,
    },
    /// Set or clear a future system-prompt overlay.
    SetSystemPromptOverlay {
        /// Overlay text, or `None` to clear the overlay.
        system_prompt: Option<String>,
        /// Caller-observed overlay version that must still be current.
        expected_version: u64,
    },
    /// Replace future tool declarations without storing a runtime registry.
    ReplaceToolSet {
        /// New static tool declarations.
        tool_set: ToolSetRef,
    },
    /// Patch future tool declarations against an expected current tool set.
    PatchToolSet {
        /// Declarative patch to apply to the current tool set.
        patch: ToolSetPatch,
    },
    /// Replace future model request settings.
    SetModel {
        /// New model request settings for future LLM calls.
        model: ModelRef,
    },
    /// Replace future loop policy knobs.
    SetLoopPolicy {
        /// New loop policy for future feed segments.
        loop_policy: LoopPolicy,
    },
}

/// Backwards-compatible name for a queued reconfiguration request.
pub type QueuedReconfig = ReconfigRequest;

impl ReconfigRequest {
    /// Creates a checked active-skill replacement intent.
    ///
    /// # Errors
    ///
    /// Returns [`AgentStateError::DuplicateSkill`] when `skill_ids` repeats an
    /// identity.
    pub fn replace_active_skills(skill_ids: Vec<SkillId>) -> Result<Self, AgentStateError> {
        let reconfig = Self::ReplaceActiveSkills { skill_ids };
        reconfig.validate()?;
        Ok(reconfig)
    }

    /// Creates a checked system overlay update with optimistic versioning.
    #[must_use]
    pub fn set_system_prompt_overlay(system_prompt: Option<String>, expected_version: u64) -> Self {
        Self::SetSystemPromptOverlay {
            system_prompt,
            expected_version,
        }
    }

    pub(super) fn validate(&self) -> Result<(), AgentStateError> {
        match self {
            Self::ReplaceActiveSkills { skill_ids } => ensure_unique_skill_ids(skill_ids)?,
            Self::ReplaceToolSet { tool_set } => ensure_unique_tool_names(tool_set.tools())?,
            Self::PatchToolSet { patch } => patch.validate()?,
            Self::ActivateSkill { .. }
            | Self::DeactivateSkill { .. }
            | Self::SetSystemPromptOverlay { .. }
            | Self::SetModel { .. }
            | Self::SetLoopPolicy { .. } => {}
        }
        Ok(())
    }
}

/// Declarative tool-set patch applied at a turn boundary.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolSetPatch {
    expected_tool_set_id: ToolSetId,
    resulting_tool_set_id: ToolSetId,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    remove: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    add_or_replace: Vec<Tool>,
}

impl ToolSetPatch {
    /// Creates a tool-set patch from caller-supplied identities and tool edits.
    ///
    /// # Errors
    ///
    /// Returns [`AgentStateError::DuplicateToolName`] when the patch repeats a
    /// tool name in either list.
    pub fn new(
        expected_tool_set_id: ToolSetId,
        resulting_tool_set_id: ToolSetId,
        remove: Vec<String>,
        add_or_replace: Vec<Tool>,
    ) -> Result<Self, AgentStateError> {
        let patch = Self {
            expected_tool_set_id,
            resulting_tool_set_id,
            remove,
            add_or_replace,
        };
        patch.validate()?;
        Ok(patch)
    }

    /// Returns the tool-set identity that must still be current.
    #[must_use]
    pub const fn expected_tool_set_id(&self) -> ToolSetId {
        self.expected_tool_set_id
    }

    /// Returns the identity assigned to the patched tool set.
    #[must_use]
    pub const fn resulting_tool_set_id(&self) -> ToolSetId {
        self.resulting_tool_set_id
    }

    /// Returns tool names to remove.
    #[must_use]
    pub fn remove(&self) -> &[String] {
        &self.remove
    }

    /// Returns tools to add or replace by name.
    #[must_use]
    pub fn add_or_replace(&self) -> &[Tool] {
        &self.add_or_replace
    }

    fn validate(&self) -> Result<(), AgentStateError> {
        ensure_unique_tool_name_strings(&self.remove)?;
        ensure_unique_tool_names(&self.add_or_replace)
    }
}

fn ensure_unique_tool_name_strings(names: &[String]) -> Result<(), AgentStateError> {
    let tools = names
        .iter()
        .map(|name| Tool {
            name: name.clone(),
            description: String::new(),
            input_schema: serde_json::Value::Null,
        })
        .collect::<Vec<_>>();
    ensure_unique_tool_names(&tools)
}
