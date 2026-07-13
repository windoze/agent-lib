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
    agent::{AgentId, AgentSpec, SkillId},
    conversation::{Conversation, ConversationError, ConversationSnapshot, ToolCallId},
    model::message::Role,
};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de, ser};
use std::collections::BTreeSet;
use thiserror::Error;

pub use cursor::{
    ApprovalCursor, CancelRecoveryCursor, CancelRecoveryReason, DoneCursor, ErrorCursor,
    LoopCursor, LoopCursorKind, LoopDoneReason, StepCursor, ToolWaitCursor,
};
pub use queue::{PivotSource, QueuedPivot, QueuedReconfig};
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
    queued_pivots: Vec<QueuedPivot>,
    queued_reconfigs: Vec<QueuedReconfig>,
    loop_cursor: LoopCursor,
}

impl AgentState {
    /// Creates Agent state from a static spec and one active Conversation.
    #[must_use]
    pub fn new(spec: AgentSpec, conversation: Conversation) -> Self {
        Self {
            spec,
            conversation,
            active_skills: Vec::new(),
            queued_pivots: Vec::new(),
            queued_reconfigs: Vec::new(),
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

    /// Returns queued pivot messages waiting for a future step boundary.
    #[must_use]
    pub fn queued_pivots(&self) -> &[QueuedPivot] {
        &self.queued_pivots
    }

    /// Returns queued reconfiguration intents waiting for a turn boundary.
    #[must_use]
    pub fn queued_reconfigs(&self) -> &[QueuedReconfig] {
        &self.queued_reconfigs
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

    /// Queues a pivot message for the next valid step boundary.
    ///
    /// # Errors
    ///
    /// Returns [`AgentStateError::InvalidPivotRole`] if the pivot message is
    /// not a user-authored message.
    pub fn queue_pivot(&mut self, pivot: QueuedPivot) -> Result<(), AgentStateError> {
        pivot.validate()?;
        self.queued_pivots.push(pivot);
        Ok(())
    }

    /// Queues a turn-boundary reconfiguration intent.
    ///
    /// # Errors
    ///
    /// Returns [`AgentStateError::DuplicateSkill`] when a replacement active
    /// skill list repeats a skill id.
    pub fn queue_reconfig(&mut self, reconfig: QueuedReconfig) -> Result<(), AgentStateError> {
        reconfig.validate()?;
        self.queued_reconfigs.push(reconfig);
        Ok(())
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

    fn from_record(record: AgentStateRecord) -> Result<Self, AgentStateError> {
        ensure_unique_skill_ids(&record.active_skills)?;
        for pivot in &record.queued_pivots {
            pivot.validate()?;
        }
        for reconfig in &record.queued_reconfigs {
            reconfig.validate()?;
        }
        record.loop_cursor.validate()?;

        let conversation = Conversation::restore(record.conversation)?;
        Ok(Self {
            spec: record.spec,
            conversation,
            active_skills: record.active_skills,
            queued_pivots: record.queued_pivots,
            queued_reconfigs: record.queued_reconfigs,
            loop_cursor: record.loop_cursor,
        })
    }
}

impl Serialize for AgentState {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let conversation = self.conversation.snapshot().map_err(ser::Error::custom)?;
        AgentStateRecord {
            spec: self.spec.clone(),
            conversation,
            active_skills: self.active_skills.clone(),
            queued_pivots: self.queued_pivots.clone(),
            queued_reconfigs: self.queued_reconfigs.clone(),
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    queued_pivots: Vec<QueuedPivot>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    queued_reconfigs: Vec<QueuedReconfig>,
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
    /// A tool-wait cursor had no tool calls to wait for.
    #[error("awaiting-tool cursor must contain at least one tool call")]
    EmptyToolWait,
    /// A tool-wait cursor repeated a framework tool-call identity.
    #[error("tool call id {call_id} appears more than once in the cursor")]
    DuplicateToolCall {
        /// Repeated tool-call identity.
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
}

fn ensure_unique_skill_ids(skill_ids: &[SkillId]) -> Result<(), AgentStateError> {
    if let Some(skill_id) = first_duplicate(skill_ids) {
        Err(AgentStateError::DuplicateSkill { skill_id })
    } else {
        Ok(())
    }
}

fn first_duplicate<T>(items: &[T]) -> Option<T>
where
    T: Copy + Ord,
{
    let mut seen = BTreeSet::new();
    items.iter().copied().find(|item| !seen.insert(*item))
}
