//! Data-only queued Agent boundary work.

use crate::{
    agent::{AgentId, SkillId, ToolSetRef},
    conversation::MessageId,
    model::message::{Message, Role},
};
use serde::{Deserialize, Serialize};

use super::{AgentStateError, ensure_unique_skill_ids};

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

    pub(super) fn validate(&self) -> Result<(), AgentStateError> {
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

/// Turn-boundary reconfiguration intent.
///
/// Reconfiguration data records future changes to skills, system prompt
/// overlays, or tool declarations. It intentionally stores declarations, not a
/// live tool registry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum QueuedReconfig {
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
    },
    /// Replace future tool declarations without storing a runtime registry.
    ReplaceToolSet {
        /// New static tool declarations.
        tool_set: ToolSetRef,
    },
}

impl QueuedReconfig {
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

    pub(super) fn validate(&self) -> Result<(), AgentStateError> {
        if let Self::ReplaceActiveSkills { skill_ids } = self {
            ensure_unique_skill_ids(skill_ids)?;
        }
        Ok(())
    }
}
