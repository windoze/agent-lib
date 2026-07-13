//! Non-destructive projection overlay for committed Conversation history.
//!
//! A projection describes complete Turn ranges as either raw history or
//! compacted artifacts. It never edits raw [`Turn`](crate::conversation::Turn)
//! or [`ConversationMessage`](crate::conversation::ConversationMessage) data.

mod artifact;

pub use artifact::{Artifact, ArtifactProvenance, StrategyRef, TokenAccounting};

use super::{
    ArtifactId, Boundary, Conversation, ConversationError, ConversationId, ProjectionError, TurnId,
};
use serde::{Deserialize, Deserializer, Serialize, de::Error as DeError};
use std::{
    collections::{HashMap, HashSet},
    ops::Range,
};

/// One stable Turn-boundary endpoint without a structural version.
///
/// Versioned [`Boundary`] tokens are consumed only when a range is first
/// checked. The stored endpoint keeps the stable position and Turn anchor so a
/// later Conversation version can revalidate the range against the current
/// lineage instead of trusting an old token.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RangeEndpoint {
    turn_count: u64,
    after_turn: Option<TurnId>,
}

impl RangeEndpoint {
    /// Creates an endpoint from already-owned lineage facts.
    const fn new(turn_count: u64, after_turn: Option<TurnId>) -> Self {
        Self {
            turn_count,
            after_turn,
        }
    }
}

/// A checked, non-empty range of complete Turns on one Conversation lineage.
///
/// Values are normally created with [`Conversation::checked_turn_range`] from
/// two current [`Boundary`] tokens. Serde restores the stable range claim, but
/// callers must validate it with
/// [`Conversation::validate_checked_turn_range`] before treating it as current
/// Conversation fact.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckedTurnRange {
    conversation_id: ConversationId,
    start: RangeEndpoint,
    end: RangeEndpoint,
}

impl CheckedTurnRange {
    /// Checks two current Conversation boundaries and stores stable anchors.
    ///
    /// # Errors
    ///
    /// Returns a classified boundary or projection error when either token is
    /// stale/foreign/pending, the range is reversed or empty, or the end lies
    /// beyond the current logical head.
    pub fn new(
        conversation: &Conversation,
        start: Boundary,
        end: Boundary,
    ) -> Result<Self, ConversationError> {
        conversation.checked_turn_range(start, end)
    }

    /// Returns the Conversation identity this range belongs to.
    #[must_use]
    pub const fn conversation_id(&self) -> ConversationId {
        self.conversation_id
    }

    /// Returns the number of complete Turns before the start boundary.
    #[must_use]
    pub const fn start_turn_count(&self) -> u64 {
        self.start.turn_count
    }

    /// Returns the Turn immediately before the start boundary, if any.
    #[must_use]
    pub const fn start_after_turn(&self) -> Option<TurnId> {
        self.start.after_turn
    }

    /// Returns the number of complete Turns before the end boundary.
    #[must_use]
    pub const fn end_turn_count(&self) -> u64 {
        self.end.turn_count
    }

    /// Returns the Turn immediately before the end boundary.
    #[must_use]
    pub const fn end_after_turn(&self) -> Option<TurnId> {
        self.end.after_turn
    }

    /// Returns the number of complete Turns covered by this range.
    #[must_use]
    pub const fn len(&self) -> u64 {
        self.end.turn_count - self.start.turn_count
    }

    /// Reports whether the range covers no Turns.
    ///
    /// Public constructors currently reject empty ranges; this method exists
    /// so future zero-length APIs can make that choice explicit.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.start.turn_count == self.end.turn_count
    }

    /// Creates a range from already-checked active lineage positions.
    fn from_positions(
        conversation_id: ConversationId,
        turns: &[super::Turn],
        start: usize,
        end: usize,
    ) -> Self {
        debug_assert!(start <= end);
        debug_assert!(end <= turns.len());
        Self {
            conversation_id,
            start: endpoint_from_turns(turns, start),
            end: endpoint_from_turns(turns, end),
        }
    }
}

/// One ordered projection span over a complete raw Turn range.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Span {
    /// Raw Turns should be rendered unchanged.
    Raw {
        /// Complete Turn range to pass through.
        turns: CheckedTurnRange,
    },

    /// Covered raw Turns should be rendered through a compacted artifact.
    Compacted {
        /// Complete Turn range summarized by the artifact.
        covers: CheckedTurnRange,
        /// Artifact used for rendering.
        artifact: ArtifactId,
        /// Serializable strategy reference recorded on the span.
        produced_by: StrategyRef,
    },
}

impl Span {
    /// Creates a raw pass-through span over a checked Turn range.
    #[must_use]
    pub fn raw(turns: CheckedTurnRange) -> Self {
        Self::Raw { turns }
    }

    /// Creates a compacted span over a checked Turn range.
    #[must_use]
    pub fn compacted(
        covers: CheckedTurnRange,
        artifact: ArtifactId,
        produced_by: StrategyRef,
    ) -> Self {
        Self::Compacted {
            covers,
            artifact,
            produced_by,
        }
    }

    /// Returns the raw Turn range described by this span.
    #[must_use]
    pub const fn range(&self) -> &CheckedTurnRange {
        match self {
            Self::Raw { turns } => turns,
            Self::Compacted { covers, .. } => covers,
        }
    }

    /// Returns the artifact identity for compacted spans.
    #[must_use]
    pub const fn artifact_id(&self) -> Option<ArtifactId> {
        match self {
            Self::Raw { .. } => None,
            Self::Compacted { artifact, .. } => Some(*artifact),
        }
    }

    /// Returns the strategy reference for compacted spans.
    #[must_use]
    pub const fn produced_by(&self) -> Option<&StrategyRef> {
        match self {
            Self::Raw { .. } => None,
            Self::Compacted { produced_by, .. } => Some(produced_by),
        }
    }
}

/// Serializable projection overlay plus the artifacts it references.
///
/// A valid projection describes the complete current head range with ordered,
/// non-overlapping spans at construction time. Later head movement can make a
/// stored span extend beyond the current head; effective-view rendering is
/// responsible for clipping without mutating raw history.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Projection {
    spans: Vec<Span>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    artifacts: Vec<Artifact>,
}

impl<'de> Deserialize<'de> for Projection {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct ProjectionData {
            spans: Vec<Span>,
            #[serde(default)]
            artifacts: Vec<Artifact>,
        }

        let data = ProjectionData::deserialize(deserializer)?;
        validate_projection_shape(&data.spans, &data.artifacts).map_err(D::Error::custom)?;
        Ok(Self {
            spans: data.spans,
            artifacts: data.artifacts,
        })
    }
}

impl Projection {
    /// Checks a complete projection over the Conversation's current head.
    ///
    /// # Errors
    ///
    /// Returns a classified projection error for invalid ranges, gaps,
    /// overlaps, missing artifacts, duplicated artifact ids, or provenance
    /// mismatches. Boundary-shaped range claims are revalidated against the
    /// current lineage and pending state.
    pub fn new(
        conversation: &Conversation,
        spans: Vec<Span>,
        artifacts: Vec<Artifact>,
    ) -> Result<Self, ConversationError> {
        let artifact_index = validate_artifacts(conversation, &artifacts)?;
        validate_spans(conversation, &spans, &artifact_index)?;
        Ok(Self { spans, artifacts })
    }

    /// Returns spans in their checked projection order.
    #[must_use]
    pub fn spans(&self) -> &[Span] {
        &self.spans
    }

    /// Returns retained artifacts in deterministic caller-supplied order.
    #[must_use]
    pub fn artifacts(&self) -> &[Artifact] {
        &self.artifacts
    }

    /// Finds an artifact by identity.
    #[must_use]
    pub fn artifact(&self, artifact_id: ArtifactId) -> Option<&Artifact> {
        self.artifacts
            .iter()
            .find(|artifact| artifact.id() == artifact_id)
    }

    /// Builds the default all-raw projection for the current effective Turns.
    pub(crate) fn raw_for_active_turns(
        conversation_id: ConversationId,
        turns: &[super::Turn],
    ) -> Self {
        if turns.is_empty() {
            return Self::default();
        }

        let range = CheckedTurnRange::from_positions(conversation_id, turns, 0, turns.len());
        Self {
            spans: vec![Span::raw(range)],
            artifacts: Vec::new(),
        }
    }
}

impl Conversation {
    /// Returns the current projection overlay.
    ///
    /// The projection is non-destructive metadata over raw history. It can
    /// contain spans that were valid before a later revert; later effective
    /// rendering must clip those spans to the current logical head.
    #[must_use]
    pub const fn projection(&self) -> &Projection {
        &self.projection
    }

    /// Creates a stable checked range from two current complete-Turn boundaries.
    ///
    /// The resulting value omits the boundary structural version and stores
    /// Turn anchors instead, so it can be revalidated after unrelated version
    /// changes.
    pub fn checked_turn_range(
        &self,
        start: Boundary,
        end: Boundary,
    ) -> Result<CheckedTurnRange, ConversationError> {
        let start_position = self.resolve_boundary(&start)?;
        let end_position = self.resolve_boundary(&end)?;
        self.checked_range_from_positions(start_position, end_position)
            .map_err(Into::into)
    }

    /// Revalidates a serde-restored or previously checked Turn range.
    ///
    /// Validation is read-only and rejects pending state, foreign owners,
    /// detached branches, reversed/empty ranges, and ranges beyond the current
    /// logical head.
    pub fn validate_checked_turn_range(
        &self,
        range: &CheckedTurnRange,
    ) -> Result<(), ConversationError> {
        self.resolve_checked_turn_range(range)?;
        Ok(())
    }

    /// Resolves a stored range to current active-lineage indices.
    pub(crate) fn resolve_checked_turn_range(
        &self,
        range: &CheckedTurnRange,
    ) -> Result<Range<usize>, ProjectionError> {
        if range.conversation_id != self.id {
            return Err(ProjectionError::RangeOwnerMismatch {
                expected: self.id,
                actual: range.conversation_id,
            });
        }
        if let Some(pending) = &self.pending {
            return Err(ProjectionError::PendingTurn {
                turn_id: pending.id(),
            });
        }
        if range.start.turn_count > range.end.turn_count {
            return Err(ProjectionError::ReversedRange {
                start: range.start.turn_count,
                end: range.end.turn_count,
            });
        }
        if range.start.turn_count == range.end.turn_count {
            return Err(ProjectionError::EmptyRange {
                turn_count: range.start.turn_count,
            });
        }

        let start = self.resolve_range_endpoint(range.start)?;
        let end = self.resolve_range_endpoint(range.end)?;
        debug_assert!(start < end);
        Ok(start..end)
    }

    /// Creates a range from already resolved boundary positions.
    fn checked_range_from_positions(
        &self,
        start: usize,
        end: usize,
    ) -> Result<CheckedTurnRange, ProjectionError> {
        if start > end {
            return Err(ProjectionError::ReversedRange {
                start: usize_to_u64(start),
                end: usize_to_u64(end),
            });
        }
        if start == end {
            return Err(ProjectionError::EmptyRange {
                turn_count: usize_to_u64(start),
            });
        }
        if end > self.history.active_len() {
            return Err(ProjectionError::RangeBeyondHead {
                end: usize_to_u64(end),
                head: usize_to_u64(self.history.active_len()),
            });
        }
        Ok(CheckedTurnRange::from_positions(
            self.id,
            self.history.turns(),
            start,
            end,
        ))
    }

    /// Resolves one stored endpoint against the current active lineage.
    fn resolve_range_endpoint(&self, endpoint: RangeEndpoint) -> Result<usize, ProjectionError> {
        let active_len = self.history.active_len();
        let active_len_u64 = usize_to_u64(active_len);
        if endpoint.turn_count > active_len_u64 {
            return Err(ProjectionError::RangeBeyondHead {
                end: endpoint.turn_count,
                head: active_len_u64,
            });
        }

        let position = usize::try_from(endpoint.turn_count).map_err(|_| {
            ProjectionError::RangePositionOutOfRange {
                turn_count: endpoint.turn_count,
                lineage_turns: usize_to_u64(self.history.lineage_len()),
            }
        })?;
        let expected = position
            .checked_sub(1)
            .and_then(|index| self.history.lineage_turns().get(index))
            .map(super::Turn::id);

        if endpoint.after_turn != expected {
            if let Some(turn_id) = endpoint.after_turn {
                if self.history.contains_turn_id(turn_id) {
                    let on_active_lineage =
                        self.history.turns().iter().any(|turn| turn.id() == turn_id);
                    if !on_active_lineage {
                        return Err(ProjectionError::DetachedTurn { turn_id });
                    }
                } else {
                    return Err(ProjectionError::UnknownTurn { turn_id });
                }
            }
            return Err(ProjectionError::RangeAnchorMismatch {
                turn_count: endpoint.turn_count,
                expected,
                actual: endpoint.after_turn,
            });
        }

        Ok(position)
    }
}

/// Builds an endpoint from a materialized active-lineage slice.
fn endpoint_from_turns(turns: &[super::Turn], position: usize) -> RangeEndpoint {
    debug_assert!(position <= turns.len());
    let turn_count = usize_to_u64(position);
    let after_turn = position
        .checked_sub(1)
        .and_then(|index| turns.get(index))
        .map(super::Turn::id);
    RangeEndpoint::new(turn_count, after_turn)
}

/// Converts an in-memory length to the stable serialized width.
fn usize_to_u64(value: usize) -> u64 {
    u64::try_from(value).expect("an in-memory turn count cannot exceed u64")
}

/// Checks artifact identities, render messages, and provenance ranges.
fn validate_artifacts<'a>(
    conversation: &Conversation,
    artifacts: &'a [Artifact],
) -> Result<HashMap<ArtifactId, &'a Artifact>, ConversationError> {
    let mut seen = HashSet::new();
    let mut index = HashMap::new();

    for artifact in artifacts {
        artifact.validate_messages()?;
        if !seen.insert(artifact.id()) {
            return Err(ProjectionError::DuplicateArtifactId {
                artifact_id: artifact.id(),
            }
            .into());
        }
        conversation.validate_checked_turn_range(artifact.provenance().input_range())?;
        index.insert(artifact.id(), artifact);
    }

    Ok(index)
}

/// Checks serde-restored projection facts that do not need a Conversation.
fn validate_projection_shape(
    spans: &[Span],
    artifacts: &[Artifact],
) -> Result<(), ProjectionError> {
    let mut seen = HashSet::new();
    let mut artifact_index = HashMap::new();
    for artifact in artifacts {
        artifact.validate_messages()?;
        if !seen.insert(artifact.id()) {
            return Err(ProjectionError::DuplicateArtifactId {
                artifact_id: artifact.id(),
            });
        }
        artifact_index.insert(artifact.id(), artifact);
    }

    let mut expected_start = 0;
    for span in spans {
        let range = span.range();
        let start = range.start_turn_count();
        let end = range.end_turn_count();
        if start > end {
            return Err(ProjectionError::ReversedRange { start, end });
        }
        if start == end {
            return Err(ProjectionError::EmptyRange { turn_count: start });
        }
        if start < expected_start {
            return Err(ProjectionError::SpanOverlap {
                expected_start,
                actual_start: start,
            });
        }
        if start > expected_start {
            return Err(ProjectionError::SpanGap {
                expected_start,
                actual_start: start,
            });
        }

        if let Span::Compacted {
            covers,
            artifact,
            produced_by,
        } = span
        {
            let artifact_value =
                artifact_index
                    .get(artifact)
                    .ok_or(ProjectionError::MissingArtifact {
                        artifact_id: *artifact,
                    })?;
            if artifact_value.provenance().input_range() != covers {
                return Err(ProjectionError::ArtifactRangeMismatch {
                    artifact_id: *artifact,
                });
            }
            if artifact_value.provenance().produced_by() != produced_by {
                return Err(ProjectionError::ArtifactStrategyMismatch {
                    artifact_id: *artifact,
                });
            }
        }

        expected_start = end;
    }

    Ok(())
}

/// Checks span ordering, coverage, artifact references, and provenance links.
fn validate_spans(
    conversation: &Conversation,
    spans: &[Span],
    artifacts: &HashMap<ArtifactId, &Artifact>,
) -> Result<(), ConversationError> {
    let mut expected_start = 0usize;

    for span in spans {
        let resolved = conversation.resolve_checked_turn_range(span.range())?;
        if resolved.start < expected_start {
            return Err(ProjectionError::SpanOverlap {
                expected_start: usize_to_u64(expected_start),
                actual_start: usize_to_u64(resolved.start),
            }
            .into());
        }
        if resolved.start > expected_start {
            return Err(ProjectionError::SpanGap {
                expected_start: usize_to_u64(expected_start),
                actual_start: usize_to_u64(resolved.start),
            }
            .into());
        }

        if let Span::Compacted {
            covers,
            artifact,
            produced_by,
        } = span
        {
            let artifact_value =
                artifacts
                    .get(artifact)
                    .ok_or(ProjectionError::MissingArtifact {
                        artifact_id: *artifact,
                    })?;
            if artifact_value.provenance().input_range() != covers {
                return Err(ProjectionError::ArtifactRangeMismatch {
                    artifact_id: *artifact,
                }
                .into());
            }
            if artifact_value.provenance().produced_by() != produced_by {
                return Err(ProjectionError::ArtifactStrategyMismatch {
                    artifact_id: *artifact,
                }
                .into());
            }
        }

        expected_start = resolved.end;
    }

    let head = conversation.history.active_len();
    if expected_start != head {
        return Err(ProjectionError::IncompleteProjection {
            expected_end: usize_to_u64(head),
            actual_end: usize_to_u64(expected_start),
        }
        .into());
    }

    Ok(())
}

#[cfg(test)]
mod tests;
