//! DB-neutral row records for Conversation snapshots.
//!
//! The rows in this module are plain serde DTOs. They describe the same
//! committed consistency point as [`ConversationSnapshot`], but split it into
//! database-friendly immutable fact tables plus per-Conversation association
//! rows. They do not bind to SQL, a migration runner, or a concrete driver.
//!
//! Message fact rows carry the full [`ConversationMessage`] envelope — the
//! provider-neutral payload plus envelope-local [`MessageMeta`] — so a
//! `to_rows → into_snapshot` round trip preserves injected-message metadata
//! (for example a pivot source label) instead of silently dropping it.

use super::{
    CONVERSATION_SNAPSHOT_SCHEMA_VERSION, ConversationSnapshot, ConversationSnapshotHistory,
};
use crate::{
    conversation::{
        Artifact, ArtifactId, ArtifactProvenance, CheckedTurnRange, ConversationConfig,
        ConversationId, ConversationMessage, ForkOrigin, MessageId, MessageMeta, Projection,
        RowMappingError, Span, StrategyRef, TokenAccounting, ToolCallId, TurnId, TurnMeta,
        turn::{ToolPairingData, TurnCompletion, TurnData},
    },
    model::message::Message,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{
    collections::{HashMap, HashSet},
    hash::Hash,
};

/// Current DB-neutral row schema version.
///
/// The row schema evolves independently of the snapshot schema: version 2 adds
/// the `generation` column to the evolving row kinds (conversation, lineage
/// membership, projection spans) so a Conversation's evolution stays
/// insert-only. Rows are still reassembled into the current
/// [`ConversationSnapshot`] data shape before live restore validation runs.
///
/// Pre-1.0 there is no migration path: row sets exported with an older schema
/// version are rejected and must be re-exported with the current crate.
pub const CONVERSATION_ROW_SCHEMA_VERSION: u32 = 2;

/// A DB-neutral decomposition of one Conversation snapshot.
///
/// `turns`, `messages`, `tool_pairings`, and `artifacts` are immutable fact
/// rows keyed by stable ids. `raw_turns`, `lineage_turns`, and projection span
/// rows are per-Conversation associations and may reference immutable facts
/// already inserted for a shared fork ancestor.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConversationRows {
    /// Conversation-level metadata and consistency-point counters.
    pub conversation: ConversationRecord,
    /// Per-Conversation retained raw Turn membership in deterministic order.
    pub raw_turns: Vec<ConversationTurnRecord>,
    /// Per-Conversation addressable lineage in deterministic order.
    pub lineage_turns: Vec<ConversationLineageTurnRecord>,
    /// Immutable Turn fact rows keyed by `turn_id`.
    pub turns: Vec<TurnRecord>,
    /// Immutable message fact rows keyed by `message_id`.
    pub messages: Vec<MessageRecord>,
    /// Immutable tool-pairing fact rows keyed by framework `call_id`.
    pub tool_pairings: Vec<ToolPairingRecord>,
    /// Projection header for the Conversation.
    pub projection: ProjectionRecord,
    /// Ordered projection span rows.
    pub projection_spans: Vec<ProjectionSpanRecord>,
    /// Retained artifact fact rows.
    pub artifacts: Vec<ArtifactRecord>,
}

/// Conversation metadata row.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConversationRecord {
    /// Row schema version understood by this crate.
    pub schema_version: u32,
    /// Conversation primary key.
    pub conversation_id: ConversationId,
    /// Conversation-level system/config data kept outside messages.
    pub config: ConversationConfig,
    /// Structural version captured at the committed consistency point.
    pub structural_version: u64,
    /// Generation key for insert-only evolution.
    ///
    /// Always equals `structural_version`: every structural change (commit,
    /// revert, compaction) mints a new generation, so a later export of the
    /// same Conversation inserts a new row instead of updating this one.
    pub generation: u64,
    /// Optional fork provenance for child Conversations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<ForkOrigin>,
    /// Number of lineage Turns visible at the logical head.
    pub head_turn_count: u64,
    /// Largest addressable lineage position for this Conversation.
    pub fork_ceiling_turn_count: u64,
}

/// Per-Conversation retained raw Turn association.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConversationTurnRecord {
    /// Owning Conversation.
    pub conversation_id: ConversationId,
    /// Dense raw membership sequence, starting at zero.
    pub raw_sequence: u64,
    /// Retained raw Turn fact referenced by this Conversation.
    pub turn_id: TurnId,
}

/// Per-Conversation addressable lineage association.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConversationLineageTurnRecord {
    /// Owning Conversation.
    pub conversation_id: ConversationId,
    /// Generation this membership row was exported at (the owning
    /// Conversation's `structural_version` at that consistency point).
    pub generation: u64,
    /// Dense lineage sequence, starting at zero.
    pub lineage_sequence: u64,
    /// Turn fact at this lineage position.
    pub turn_id: TurnId,
}

/// Immutable Turn fact row.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TurnRecord {
    /// Turn primary key.
    pub turn_id: TurnId,
    /// Parent Turn pointer in the retained raw tree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_turn_id: Option<TurnId>,
    /// Caller/client metadata attached to the closed Turn.
    pub meta: TurnMeta,
}

/// Immutable message fact row.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MessageRecord {
    /// Message primary key.
    pub message_id: MessageId,
    /// Owning Turn foreign key.
    pub turn_id: TurnId,
    /// Dense message sequence within the Turn, starting at zero.
    pub message_sequence: u64,
    /// Provider-neutral complete Client message payload.
    pub payload: Message,
    /// Envelope-local metadata frozen with the message, when any.
    ///
    /// Rows exported before this column existed deserialize with `None`,
    /// matching the envelope's own absent-metadata representation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<MessageMeta>,
}

/// Immutable tool pairing fact row.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolPairingRecord {
    /// Framework tool-call primary key.
    pub call_id: ToolCallId,
    /// Owning Turn foreign key.
    pub turn_id: TurnId,
    /// Dense pairing sequence within the Turn, starting at zero.
    pub pairing_sequence: u64,
    /// Provider call id when the provider supplied one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_call_id: Option<String>,
    /// Message containing the tool-use block.
    pub call_message_id: MessageId,
    /// Message containing the corresponding tool-result block.
    pub result_message_id: MessageId,
}

/// Projection header row.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectionRecord {
    /// Owning Conversation.
    pub conversation_id: ConversationId,
    /// Row schema version understood by this crate.
    pub schema_version: u32,
}

/// Projection span category stored in [`ProjectionSpanRecord`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectionSpanKind {
    /// Render covered Turns from raw history.
    Raw,
    /// Render covered Turns from a retained artifact.
    Compacted,
}

/// Ordered projection span row.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectionSpanRecord {
    /// Owning Conversation.
    pub conversation_id: ConversationId,
    /// Generation this span row was exported at (the owning Conversation's
    /// `structural_version` at that consistency point).
    pub generation: u64,
    /// Dense projection span sequence, starting at zero.
    pub span_sequence: u64,
    /// Span rendering mode.
    pub kind: ProjectionSpanKind,
    /// Start boundary turn count.
    pub start_turn_count: u64,
    /// Turn immediately before the start boundary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_after_turn: Option<TurnId>,
    /// End boundary turn count.
    pub end_turn_count: u64,
    /// Turn immediately before the end boundary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_after_turn: Option<TurnId>,
    /// Artifact referenced by compacted spans.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_id: Option<ArtifactId>,
    /// Strategy reference recorded by compacted spans.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub produced_by: Option<StrategyRef>,
}

/// Retained projection artifact row.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactRecord {
    /// Owning Conversation projection.
    pub conversation_id: ConversationId,
    /// Dense retained artifact sequence, starting at zero.
    pub artifact_sequence: u64,
    /// Artifact primary key.
    pub artifact_id: ArtifactId,
    /// Complete Client messages rendered for this artifact.
    pub messages: Vec<Message>,
    /// Start boundary turn count for provenance.
    pub input_start_turn_count: u64,
    /// Turn immediately before the provenance start boundary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_start_after_turn: Option<TurnId>,
    /// End boundary turn count for provenance.
    pub input_end_turn_count: u64,
    /// Turn immediately before the provenance end boundary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_end_after_turn: Option<TurnId>,
    /// Strategy reference that produced the artifact.
    pub produced_by: StrategyRef,
    /// Token accounting for the compaction artifact.
    pub tokens: TokenAccounting,
    /// Extensible artifact provenance fields.
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub extra: Map<String, Value>,
}

/// Insert-only mutation set derived from comparing two row sets.
///
/// The set only contains rows that are absent from the existing set. If the
/// same primary key exists with different immutable facts, construction fails
/// with [`RowMappingError::InsertConflict`] instead of describing an update.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConversationRowInsertSet {
    /// Conversation rows to insert.
    pub conversations: Vec<ConversationRecord>,
    /// Raw membership rows to insert.
    pub raw_turns: Vec<ConversationTurnRecord>,
    /// Lineage rows to insert.
    pub lineage_turns: Vec<ConversationLineageTurnRecord>,
    /// Immutable Turn facts to insert.
    pub turns: Vec<TurnRecord>,
    /// Immutable message facts to insert.
    pub messages: Vec<MessageRecord>,
    /// Immutable tool pairing facts to insert.
    pub tool_pairings: Vec<ToolPairingRecord>,
    /// Projection header rows to insert.
    pub projections: Vec<ProjectionRecord>,
    /// Projection span rows to insert.
    pub projection_spans: Vec<ProjectionSpanRecord>,
    /// Artifact rows to insert.
    pub artifacts: Vec<ArtifactRecord>,
}

impl ConversationRowInsertSet {
    /// Reports whether the insert set contains no rows.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.conversations.is_empty()
            && self.raw_turns.is_empty()
            && self.lineage_turns.is_empty()
            && self.turns.is_empty()
            && self.messages.is_empty()
            && self.tool_pairings.is_empty()
            && self.projections.is_empty()
            && self.projection_spans.is_empty()
            && self.artifacts.is_empty()
    }
}

impl ConversationSnapshot {
    /// Splits this versioned snapshot into DB-neutral rows.
    ///
    /// The decomposition is deterministic and does not validate live
    /// Conversation semantics. Use [`ConversationRows::into_snapshot`] followed
    /// by [`Conversation::restore`](crate::conversation::Conversation::restore)
    /// to re-enter runtime state.
    ///
    /// # Errors
    ///
    /// Returns [`RowMappingError`] if the snapshot contains a closed-row shape
    /// that cannot be represented by insert-only row facts.
    pub fn to_rows(&self) -> Result<ConversationRows, RowMappingError> {
        ConversationRows::from_snapshot(self)
    }

    /// Reassembles a versioned snapshot from DB-neutral rows.
    ///
    /// This performs row-level PK/FK/sequence checks and projection data-shape
    /// checks. It deliberately returns a data snapshot, not a live
    /// Conversation; callers must still run normal restore validation.
    ///
    /// # Errors
    ///
    /// Returns [`RowMappingError`] for duplicate primary keys, missing
    /// referenced facts, sequence gaps, owner mismatches, or invalid projection
    /// row shapes.
    pub fn from_rows(rows: ConversationRows) -> Result<Self, RowMappingError> {
        rows.into_snapshot()
    }
}

impl ConversationRows {
    /// Splits a snapshot into deterministic DB-neutral rows.
    ///
    /// Shared fork ancestors appear once as immutable Turn/message facts. A
    /// child Conversation records its own raw/lineage association rows that
    /// reference those stable ids.
    ///
    /// # Errors
    ///
    /// Returns [`RowMappingError`] when a snapshot fact cannot be represented
    /// as a closed row, such as a dangling tool pairing.
    pub fn from_snapshot(snapshot: &ConversationSnapshot) -> Result<Self, RowMappingError> {
        let conversation_id = snapshot.id();
        let history = snapshot.history();
        // The export consistency point's structural version is the generation
        // of every evolving row produced here.
        let generation = snapshot.structural_version();

        let conversation = ConversationRecord {
            schema_version: CONVERSATION_ROW_SCHEMA_VERSION,
            conversation_id,
            config: snapshot.config().clone(),
            structural_version: generation,
            generation,
            origin: snapshot.origin(),
            head_turn_count: history.head_turn_count(),
            fork_ceiling_turn_count: history.fork_ceiling_turn_count(),
        };

        let mut raw_turns = Vec::new();
        let mut turns = Vec::new();
        let mut messages = Vec::new();
        let mut tool_pairings = Vec::new();

        for (raw_index, turn) in history.raw_turns().iter().enumerate() {
            raw_turns.push(ConversationTurnRecord {
                conversation_id,
                raw_sequence: usize_to_u64(raw_index),
                turn_id: turn.id,
            });
            turns.push(TurnRecord {
                turn_id: turn.id,
                parent_turn_id: turn.parent,
                meta: turn.meta.clone(),
            });
            for (message_index, message) in turn.messages.iter().enumerate() {
                messages.push(MessageRecord {
                    message_id: message.id(),
                    turn_id: turn.id,
                    message_sequence: usize_to_u64(message_index),
                    payload: message.payload().clone(),
                    meta: message.meta().cloned(),
                });
            }
            for (pairing_index, pairing) in turn.pairings.iter().enumerate() {
                let result_message_id = pairing.result_msg.ok_or_else(|| {
                    RowMappingError::InvalidRow {
                        path: format!(
                            "$.history.raw_turns[{raw_index}].pairings[{pairing_index}].result_msg"
                        ),
                        table: "tool_pairing_records",
                        reason: "closed pairing result message is missing",
                    }
                })?;
                tool_pairings.push(ToolPairingRecord {
                    call_id: pairing.call_id,
                    turn_id: turn.id,
                    pairing_sequence: usize_to_u64(pairing_index),
                    provider_call_id: pairing.provider_call_id.clone(),
                    call_message_id: pairing.call_msg,
                    result_message_id,
                });
            }
        }

        let lineage_turns = history
            .lineage_turn_ids()
            .iter()
            .copied()
            .enumerate()
            .map(|(lineage_index, turn_id)| ConversationLineageTurnRecord {
                conversation_id,
                generation,
                lineage_sequence: usize_to_u64(lineage_index),
                turn_id,
            })
            .collect();

        let projection = ProjectionRecord {
            conversation_id,
            schema_version: CONVERSATION_ROW_SCHEMA_VERSION,
        };
        let projection_spans = snapshot
            .projection()
            .spans()
            .iter()
            .enumerate()
            .map(|(span_index, span)| {
                ProjectionSpanRecord::from_span(conversation_id, generation, span_index, span)
            })
            .collect();
        let artifacts = snapshot
            .projection()
            .artifacts()
            .iter()
            .enumerate()
            .map(|(artifact_index, artifact)| {
                ArtifactRecord::from_artifact(conversation_id, artifact_index, artifact)
            })
            .collect();

        Ok(Self {
            conversation,
            raw_turns,
            lineage_turns,
            turns,
            messages,
            tool_pairings,
            projection,
            projection_spans,
            artifacts,
        })
    }

    /// Reassembles this row set into a versioned snapshot.
    ///
    /// Row order is irrelevant. The method sorts by explicit sequence fields,
    /// checks dense ordering and FK reachability, and then builds the same data
    /// snapshot shape that normal restore validates.
    ///
    /// # Errors
    ///
    /// Returns [`RowMappingError`] for duplicate PKs, wrong owner rows, missing
    /// referenced immutable facts, sequence gaps, invalid projection rows, or
    /// orphan facts that are not reachable from the Conversation associations.
    pub fn into_snapshot(self) -> Result<ConversationSnapshot, RowMappingError> {
        let owner = self.conversation.conversation_id;
        self.validate_schema_versions()?;
        self.validate_generations()?;
        self.validate_row_owners(owner)?;

        let raw_members = sorted_conversation_turns(&self.raw_turns)?;
        let lineage_members = sorted_lineage_turns(&self.lineage_turns)?;
        let turn_records = unique_by_key(&self.turns, "turn_records", "$.turns", |turn| {
            turn.turn_id.to_string()
        })?;
        let retained_turns = retained_turn_ids(&raw_members)?;
        reject_orphan_turn_records(&self.turns, &retained_turns)?;

        let messages_by_turn = group_messages(&self.messages, &turn_records, &retained_turns)?;
        let pairings_by_turn =
            group_tool_pairings(&self.tool_pairings, &turn_records, &retained_turns)?;

        let mut raw_turns = Vec::with_capacity(raw_members.len());
        for (raw_index, membership) in raw_members.iter().enumerate() {
            let turn = turn_records
                .get(&membership.turn_id.to_string())
                .ok_or_else(|| RowMappingError::MissingTurnRow {
                    path: format!("$.raw_turns[{raw_index}].turn_id"),
                    turn_id: membership.turn_id,
                })?;
            let messages = messages_for_turn(
                membership.turn_id,
                raw_index,
                messages_by_turn.get(&membership.turn_id),
            )?;
            let pairings = pairings_for_turn(pairings_by_turn.get(&membership.turn_id))?;
            raw_turns.push(TurnData {
                id: membership.turn_id,
                messages,
                pairings,
                parent: turn.parent_turn_id,
                meta: turn.meta.clone(),
                completion: TurnCompletion::Complete,
            });
        }

        let lineage_turns = lineage_members
            .iter()
            .enumerate()
            .map(|(lineage_index, membership)| {
                if !retained_turns.contains(&membership.turn_id) {
                    return Err(RowMappingError::MissingTurnRow {
                        path: format!("$.lineage_turns[{lineage_index}].turn_id"),
                        turn_id: membership.turn_id,
                    });
                }
                Ok(membership.turn_id)
            })
            .collect::<Result<Vec<_>, _>>()?;

        let projection = rows_to_projection(
            owner,
            &self.projection_spans,
            &self.artifacts,
            "$.projection_spans",
        )?;
        let history = ConversationSnapshotHistory::from_parts(
            raw_turns,
            lineage_turns,
            self.conversation.head_turn_count,
            self.conversation.fork_ceiling_turn_count,
        );
        // Row schema versions evolve independently of the snapshot schema;
        // validated current rows always describe the current snapshot shape.
        Ok(ConversationSnapshot::from_parts(
            CONVERSATION_SNAPSHOT_SCHEMA_VERSION,
            owner,
            self.conversation.config,
            self.conversation.structural_version,
            self.conversation.origin,
            history,
            projection,
        ))
    }

    /// Computes rows that can be inserted without updating existing facts.
    ///
    /// This is useful when exporting a fork child after a parent prefix has
    /// already been stored. Shared ancestor Turn/message/pairing rows are
    /// omitted when their immutable facts are identical; child association rows
    /// still appear because they belong to the child Conversation.
    ///
    /// # Errors
    ///
    /// Returns [`RowMappingError::InsertConflict`] if a row with the same
    /// primary key already exists but contains different immutable data.
    pub fn insert_set_against(
        &self,
        existing: &ConversationRows,
    ) -> Result<ConversationRowInsertSet, RowMappingError> {
        self.clone().into_snapshot()?;
        existing.clone().into_snapshot()?;

        let conversations = diff_single_conversation(&self.conversation, &existing.conversation)?;
        Ok(ConversationRowInsertSet {
            conversations,
            raw_turns: diff_rows(
                "conversation_turn_records",
                "$.raw_turns",
                &self.raw_turns,
                &existing.raw_turns,
                |row| format!("{}#{}", row.conversation_id, row.raw_sequence),
            )?,
            lineage_turns: diff_rows(
                "conversation_lineage_turn_records",
                "$.lineage_turns",
                &self.lineage_turns,
                &existing.lineage_turns,
                |row| format!("{}#{}", row.conversation_id, row.lineage_sequence),
            )?,
            turns: diff_rows(
                "turn_records",
                "$.turns",
                &self.turns,
                &existing.turns,
                |row| row.turn_id.to_string(),
            )?,
            messages: diff_rows(
                "message_records",
                "$.messages",
                &self.messages,
                &existing.messages,
                |row| row.message_id.to_string(),
            )?,
            tool_pairings: diff_rows(
                "tool_pairing_records",
                "$.tool_pairings",
                &self.tool_pairings,
                &existing.tool_pairings,
                |row| row.call_id.to_string(),
            )?,
            projections: diff_rows(
                "projection_records",
                "$.projection",
                std::slice::from_ref(&self.projection),
                std::slice::from_ref(&existing.projection),
                |row| row.conversation_id.to_string(),
            )?,
            projection_spans: diff_rows(
                "projection_span_records",
                "$.projection_spans",
                &self.projection_spans,
                &existing.projection_spans,
                |row| format!("{}#{}", row.conversation_id, row.span_sequence),
            )?,
            artifacts: diff_rows(
                "artifact_records",
                "$.artifacts",
                &self.artifacts,
                &existing.artifacts,
                |row| row.artifact_id.to_string(),
            )?,
        })
    }

    /// Ensures row schema versions are supported.
    ///
    /// Pre-1.0 there is no migration path: older row sets are rejected
    /// outright and must be re-exported with the current crate.
    fn validate_schema_versions(&self) -> Result<(), RowMappingError> {
        if self.conversation.schema_version != CONVERSATION_ROW_SCHEMA_VERSION {
            return Err(RowMappingError::InvalidRow {
                path: "$.conversation.schema_version".to_owned(),
                table: "conversation_records",
                reason: "unsupported row schema version (no migration path pre-1.0; re-export rows with the current crate)",
            });
        }
        if self.projection.schema_version != CONVERSATION_ROW_SCHEMA_VERSION {
            return Err(RowMappingError::InvalidRow {
                path: "$.projection.schema_version".to_owned(),
                table: "projection_records",
                reason: "unsupported row schema version (no migration path pre-1.0; re-export rows with the current crate)",
            });
        }
        Ok(())
    }

    /// Ensures the generation columns agree with the conversation row.
    ///
    /// The in-memory row set holds exactly one conversation row, so every
    /// evolving association row must belong to that row's generation; mixed
    /// generations can only enter through a tampered or hand-built set.
    fn validate_generations(&self) -> Result<(), RowMappingError> {
        let generation = self.conversation.structural_version;
        if self.conversation.generation != generation {
            return Err(RowMappingError::InvalidRow {
                path: "$.conversation.generation".to_owned(),
                table: "conversation_records",
                reason: "generation must equal structural_version",
            });
        }
        for (index, row) in self.lineage_turns.iter().enumerate() {
            if row.generation != generation {
                return Err(RowMappingError::InvalidRow {
                    path: format!("$.lineage_turns[{index}].generation"),
                    table: "conversation_lineage_turn_records",
                    reason: "generation does not match the conversation row generation",
                });
            }
        }
        for (index, row) in self.projection_spans.iter().enumerate() {
            if row.generation != generation {
                return Err(RowMappingError::InvalidRow {
                    path: format!("$.projection_spans[{index}].generation"),
                    table: "projection_span_records",
                    reason: "generation does not match the conversation row generation",
                });
            }
        }
        Ok(())
    }

    /// Ensures every owner-scoped row belongs to the same Conversation.
    fn validate_row_owners(&self, owner: ConversationId) -> Result<(), RowMappingError> {
        ensure_owner(
            "$.projection.conversation_id",
            owner,
            self.projection.conversation_id,
        )?;
        for (index, row) in self.raw_turns.iter().enumerate() {
            ensure_owner(
                &format!("$.raw_turns[{index}].conversation_id"),
                owner,
                row.conversation_id,
            )?;
        }
        for (index, row) in self.lineage_turns.iter().enumerate() {
            ensure_owner(
                &format!("$.lineage_turns[{index}].conversation_id"),
                owner,
                row.conversation_id,
            )?;
        }
        for (index, row) in self.projection_spans.iter().enumerate() {
            ensure_owner(
                &format!("$.projection_spans[{index}].conversation_id"),
                owner,
                row.conversation_id,
            )?;
        }
        for (index, row) in self.artifacts.iter().enumerate() {
            ensure_owner(
                &format!("$.artifacts[{index}].conversation_id"),
                owner,
                row.conversation_id,
            )?;
        }
        Ok(())
    }
}

impl ProjectionSpanRecord {
    /// Copies one projection span into a DB-neutral row.
    fn from_span(
        conversation_id: ConversationId,
        generation: u64,
        span_index: usize,
        span: &Span,
    ) -> Self {
        let range = span.range();
        match span {
            Span::Raw { .. } => Self {
                conversation_id,
                generation,
                span_sequence: usize_to_u64(span_index),
                kind: ProjectionSpanKind::Raw,
                start_turn_count: range.start_turn_count(),
                start_after_turn: range.start_after_turn(),
                end_turn_count: range.end_turn_count(),
                end_after_turn: range.end_after_turn(),
                artifact_id: None,
                produced_by: None,
            },
            Span::Compacted {
                artifact,
                produced_by,
                ..
            } => Self {
                conversation_id,
                generation,
                span_sequence: usize_to_u64(span_index),
                kind: ProjectionSpanKind::Compacted,
                start_turn_count: range.start_turn_count(),
                start_after_turn: range.start_after_turn(),
                end_turn_count: range.end_turn_count(),
                end_after_turn: range.end_after_turn(),
                artifact_id: Some(*artifact),
                produced_by: Some(produced_by.clone()),
            },
        }
    }

    /// Rebuilds the stored range claim for this span.
    fn range(&self, owner: ConversationId) -> CheckedTurnRange {
        CheckedTurnRange::from_persisted_parts(
            owner,
            self.start_turn_count,
            self.start_after_turn,
            self.end_turn_count,
            self.end_after_turn,
        )
    }
}

impl ArtifactRecord {
    /// Copies one retained artifact into a DB-neutral row.
    fn from_artifact(
        conversation_id: ConversationId,
        artifact_index: usize,
        artifact: &Artifact,
    ) -> Self {
        let provenance = artifact.provenance();
        let input_range = provenance.input_range();
        Self {
            conversation_id,
            artifact_sequence: usize_to_u64(artifact_index),
            artifact_id: artifact.id(),
            messages: artifact.messages().to_vec(),
            input_start_turn_count: input_range.start_turn_count(),
            input_start_after_turn: input_range.start_after_turn(),
            input_end_turn_count: input_range.end_turn_count(),
            input_end_after_turn: input_range.end_after_turn(),
            produced_by: provenance.produced_by().clone(),
            tokens: provenance.tokens().clone(),
            extra: provenance.extra().clone(),
        }
    }

    /// Rebuilds one retained artifact from row facts.
    fn to_artifact(&self, owner: ConversationId) -> Result<Artifact, RowMappingError> {
        let range = CheckedTurnRange::from_persisted_parts(
            owner,
            self.input_start_turn_count,
            self.input_start_after_turn,
            self.input_end_turn_count,
            self.input_end_after_turn,
        );
        Artifact::new(
            self.artifact_id,
            self.messages.clone(),
            ArtifactProvenance::new(
                range,
                self.produced_by.clone(),
                self.tokens.clone(),
                self.extra.clone(),
            ),
        )
        .map_err(|source| RowMappingError::InvalidProjectionRows {
            path: "$.artifacts".to_owned(),
            source,
        })
    }
}

/// Ensures one owner-scoped row belongs to the row-set owner.
fn ensure_owner(
    path: &str,
    expected: ConversationId,
    actual: ConversationId,
) -> Result<(), RowMappingError> {
    if actual != expected {
        return Err(RowMappingError::ConversationMismatch {
            path: path.to_owned(),
            expected,
            actual,
        });
    }
    Ok(())
}

/// Returns raw membership sorted by dense sequence.
fn sorted_conversation_turns(
    rows: &[ConversationTurnRecord],
) -> Result<Vec<&ConversationTurnRecord>, RowMappingError> {
    let mut seen_turns = HashSet::new();
    for (index, row) in rows.iter().enumerate() {
        if !seen_turns.insert(row.turn_id) {
            return Err(RowMappingError::DuplicatePrimaryKey {
                path: format!("$.raw_turns[{index}].turn_id"),
                table: "conversation_turn_records",
                key: row.turn_id.to_string(),
            });
        }
    }
    sorted_by_dense_sequence(rows, "conversation_turn_records", "$.raw_turns", |row| {
        row.raw_sequence
    })
}

/// Returns lineage membership sorted by dense sequence.
fn sorted_lineage_turns(
    rows: &[ConversationLineageTurnRecord],
) -> Result<Vec<&ConversationLineageTurnRecord>, RowMappingError> {
    sorted_by_dense_sequence(
        rows,
        "conversation_lineage_turn_records",
        "$.lineage_turns",
        |row| row.lineage_sequence,
    )
}

/// Sorts rows by a sequence column and requires 0..N with no gaps.
fn sorted_by_dense_sequence<'a, T, F>(
    rows: &'a [T],
    table: &'static str,
    path: &'static str,
    sequence: F,
) -> Result<Vec<&'a T>, RowMappingError>
where
    F: Fn(&T) -> u64,
{
    let mut seen = HashSet::new();
    for (index, row) in rows.iter().enumerate() {
        let value = sequence(row);
        if !seen.insert(value) {
            return Err(RowMappingError::DuplicateSequence {
                path: format!("{path}[{index}]"),
                table,
                sequence: value,
            });
        }
    }

    let mut sorted = rows.iter().collect::<Vec<_>>();
    sorted.sort_by_key(|row| sequence(row));
    for (expected, row) in sorted.iter().enumerate() {
        let actual = sequence(row);
        let expected = usize_to_u64(expected);
        if actual != expected {
            return Err(RowMappingError::SequenceGap {
                path: path.to_owned(),
                table,
                expected,
                actual,
            });
        }
    }
    Ok(sorted)
}

/// Indexes rows by stable primary key and rejects duplicates.
fn unique_by_key<'a, T, K, F>(
    rows: &'a [T],
    table: &'static str,
    path: &'static str,
    key: F,
) -> Result<HashMap<String, &'a T>, RowMappingError>
where
    F: Fn(&T) -> K,
    K: ToString + Eq + Hash,
{
    let mut index = HashMap::new();
    for (row_index, row) in rows.iter().enumerate() {
        let key = key(row).to_string();
        if index.insert(key.clone(), row).is_some() {
            return Err(RowMappingError::DuplicatePrimaryKey {
                path: format!("{path}[{row_index}]"),
                table,
                key,
            });
        }
    }
    Ok(index)
}

/// Builds the retained raw Turn id set from raw membership rows.
fn retained_turn_ids(
    raw_members: &[&ConversationTurnRecord],
) -> Result<HashSet<TurnId>, RowMappingError> {
    let mut retained = HashSet::new();
    for membership in raw_members {
        if !retained.insert(membership.turn_id) {
            return Err(RowMappingError::DuplicatePrimaryKey {
                path: "$.raw_turns".to_owned(),
                table: "conversation_turn_records",
                key: membership.turn_id.to_string(),
            });
        }
    }
    Ok(retained)
}

/// Rejects Turn fact rows not named by the Conversation raw membership.
fn reject_orphan_turn_records(
    turns: &[TurnRecord],
    retained: &HashSet<TurnId>,
) -> Result<(), RowMappingError> {
    for (index, turn) in turns.iter().enumerate() {
        if !retained.contains(&turn.turn_id) {
            return Err(RowMappingError::OrphanRow {
                path: format!("$.turns[{index}].turn_id"),
                table: "turn_records",
                key: turn.turn_id.to_string(),
            });
        }
    }
    Ok(())
}

/// Groups message rows by owning Turn after checking message PKs and FKs.
fn group_messages<'a>(
    rows: &'a [MessageRecord],
    turns: &HashMap<String, &'a TurnRecord>,
    retained: &HashSet<TurnId>,
) -> Result<HashMap<TurnId, Vec<&'a MessageRecord>>, RowMappingError> {
    unique_by_key(rows, "message_records", "$.messages", |row| {
        row.message_id.to_string()
    })?;
    let mut grouped: HashMap<TurnId, Vec<&MessageRecord>> = HashMap::new();
    for (index, row) in rows.iter().enumerate() {
        if !turns.contains_key(&row.turn_id.to_string()) {
            return Err(RowMappingError::MissingTurnRow {
                path: format!("$.messages[{index}].turn_id"),
                turn_id: row.turn_id,
            });
        }
        if !retained.contains(&row.turn_id) {
            return Err(RowMappingError::OrphanRow {
                path: format!("$.messages[{index}].turn_id"),
                table: "message_records",
                key: row.message_id.to_string(),
            });
        }
        grouped.entry(row.turn_id).or_default().push(row);
    }
    Ok(grouped)
}

/// Groups tool pairing rows by owning Turn after checking pairing PKs and FKs.
fn group_tool_pairings<'a>(
    rows: &'a [ToolPairingRecord],
    turns: &HashMap<String, &'a TurnRecord>,
    retained: &HashSet<TurnId>,
) -> Result<HashMap<TurnId, Vec<&'a ToolPairingRecord>>, RowMappingError> {
    unique_by_key(rows, "tool_pairing_records", "$.tool_pairings", |row| {
        row.call_id.to_string()
    })?;
    let mut grouped: HashMap<TurnId, Vec<&ToolPairingRecord>> = HashMap::new();
    for (index, row) in rows.iter().enumerate() {
        if !turns.contains_key(&row.turn_id.to_string()) {
            return Err(RowMappingError::MissingTurnRow {
                path: format!("$.tool_pairings[{index}].turn_id"),
                turn_id: row.turn_id,
            });
        }
        if !retained.contains(&row.turn_id) {
            return Err(RowMappingError::OrphanRow {
                path: format!("$.tool_pairings[{index}].turn_id"),
                table: "tool_pairing_records",
                key: row.call_id.to_string(),
            });
        }
        grouped.entry(row.turn_id).or_default().push(row);
    }
    Ok(grouped)
}

/// Rebuilds ordered immutable messages for one Turn.
fn messages_for_turn(
    turn_id: TurnId,
    raw_index: usize,
    rows: Option<&Vec<&MessageRecord>>,
) -> Result<Vec<ConversationMessage>, RowMappingError> {
    let rows = rows.ok_or_else(|| RowMappingError::MissingMessageRows {
        path: format!("$.raw_turns[{raw_index}]"),
        turn_id,
    })?;
    let sorted =
        sorted_by_dense_sequence(rows.as_slice(), "message_records", "$.messages", |row| {
            row.message_sequence
        })?;
    if sorted.is_empty() {
        return Err(RowMappingError::MissingMessageRows {
            path: format!("$.raw_turns[{raw_index}]"),
            turn_id,
        });
    }
    Ok(sorted
        .into_iter()
        .map(|row| match &row.meta {
            Some(meta) => ConversationMessage::new_with_meta(
                row.message_id,
                row.payload.clone(),
                meta.clone(),
            ),
            None => ConversationMessage::new(row.message_id, row.payload.clone()),
        })
        .collect())
}

/// Rebuilds ordered tool pairing rows for one Turn.
fn pairings_for_turn(
    rows: Option<&Vec<&ToolPairingRecord>>,
) -> Result<Vec<ToolPairingData>, RowMappingError> {
    let Some(rows) = rows else {
        return Ok(Vec::new());
    };
    let sorted = sorted_by_dense_sequence(
        rows.as_slice(),
        "tool_pairing_records",
        "$.tool_pairings",
        |row| row.pairing_sequence,
    )?;
    Ok(sorted
        .into_iter()
        .map(|row| ToolPairingData {
            call_id: row.call_id,
            provider_call_id: row.provider_call_id.clone(),
            call_msg: row.call_message_id,
            result_msg: Some(row.result_message_id),
        })
        .collect())
}

/// Rebuilds Projection data from span and artifact rows.
fn rows_to_projection(
    owner: ConversationId,
    span_rows: &[ProjectionSpanRecord],
    artifact_rows: &[ArtifactRecord],
    path: &'static str,
) -> Result<Projection, RowMappingError> {
    unique_by_key(artifact_rows, "artifact_records", "$.artifacts", |row| {
        row.artifact_id.to_string()
    })?;
    let artifact_rows =
        sorted_by_dense_sequence(artifact_rows, "artifact_records", "$.artifacts", |row| {
            row.artifact_sequence
        })?;
    let artifacts = artifact_rows
        .into_iter()
        .map(|row| row.to_artifact(owner))
        .collect::<Result<Vec<_>, _>>()?;

    let span_rows = sorted_by_dense_sequence(span_rows, "projection_span_records", path, |row| {
        row.span_sequence
    })?;
    let spans = span_rows
        .into_iter()
        .enumerate()
        .map(|(index, row)| row_to_span(owner, index, row))
        .collect::<Result<Vec<_>, _>>()?;

    Projection::from_persisted_parts(spans, artifacts).map_err(|source| {
        RowMappingError::InvalidProjectionRows {
            path: "$.projection".to_owned(),
            source,
        }
    })
}

/// Rebuilds one projection span from a row.
fn row_to_span(
    owner: ConversationId,
    index: usize,
    row: &ProjectionSpanRecord,
) -> Result<Span, RowMappingError> {
    let range = row.range(owner);
    match row.kind {
        ProjectionSpanKind::Raw => {
            if row.artifact_id.is_some() || row.produced_by.is_some() {
                return Err(RowMappingError::InvalidRow {
                    path: format!("$.projection_spans[{index}]"),
                    table: "projection_span_records",
                    reason: "raw span cannot reference an artifact or strategy",
                });
            }
            Ok(Span::raw(range))
        }
        ProjectionSpanKind::Compacted => {
            let artifact = row.artifact_id.ok_or_else(|| RowMappingError::InvalidRow {
                path: format!("$.projection_spans[{index}].artifact_id"),
                table: "projection_span_records",
                reason: "compacted span is missing artifact_id",
            })?;
            let produced_by =
                row.produced_by
                    .clone()
                    .ok_or_else(|| RowMappingError::InvalidRow {
                        path: format!("$.projection_spans[{index}].produced_by"),
                        table: "projection_span_records",
                        reason: "compacted span is missing produced_by",
                    })?;
            Ok(Span::compacted(range, artifact, produced_by))
        }
    }
}

/// Computes the Conversation row diff.
fn diff_single_conversation(
    current: &ConversationRecord,
    existing: &ConversationRecord,
) -> Result<Vec<ConversationRecord>, RowMappingError> {
    if current.conversation_id == existing.conversation_id {
        if current == existing {
            return Ok(Vec::new());
        }
        return Err(RowMappingError::InsertConflict {
            path: "$.conversation".to_owned(),
            table: "conversation_records",
            key: current.conversation_id.to_string(),
        });
    }
    Ok(vec![current.clone()])
}

/// Computes insert-only rows for one logical table.
fn diff_rows<T, F>(
    table: &'static str,
    path: &'static str,
    current: &[T],
    existing: &[T],
    key: F,
) -> Result<Vec<T>, RowMappingError>
where
    T: Clone + Eq,
    F: Fn(&T) -> String,
{
    let mut existing_index: HashMap<String, &T> = HashMap::new();
    for (index, row) in existing.iter().enumerate() {
        let row_key = key(row);
        if existing_index.insert(row_key.clone(), row).is_some() {
            return Err(RowMappingError::DuplicatePrimaryKey {
                path: format!("$.existing.{table}[{index}]"),
                table,
                key: row_key,
            });
        }
    }

    let mut inserts = Vec::new();
    for row in current {
        let row_key = key(row);
        match existing_index.get(&row_key) {
            Some(existing_row) if *existing_row == row => {}
            Some(_) => {
                return Err(RowMappingError::InsertConflict {
                    path: path.to_owned(),
                    table,
                    key: row_key,
                });
            }
            None => inserts.push(row.clone()),
        }
    }
    Ok(inserts)
}

/// Converts an in-memory index to the stable row integer width.
fn usize_to_u64(value: usize) -> u64 {
    u64::try_from(value).expect("an in-memory row count cannot exceed u64")
}
