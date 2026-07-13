//! Non-destructive projection overlay for committed Conversation history.
//!
//! A projection describes complete Turn ranges as either raw history or
//! compacted artifacts. It never edits raw [`Turn`](crate::conversation::Turn)
//! or [`ConversationMessage`](crate::conversation::ConversationMessage) data.

mod artifact;
mod compaction;
mod strategy;

pub use artifact::{Artifact, ArtifactProvenance, StrategyRef, TokenAccounting};
pub use compaction::{CompactionPlan, CompactionStep, CompactionTarget};
pub use strategy::{
    ArtifactDraft, CompactCtx, CompactionInput, CompactionStrategy, CompactionStrategyResolver,
    CompactionTrigger, CompactionTriggerOutcome, DeferredUntilBoundary,
    materialize_compaction_plan, run_compaction_strategy,
};

use super::{
    ArtifactId, Boundary, Conversation, ConversationError, ConversationId, ProjectionError, TurnId,
};
use crate::model::message::Message;
use serde::{Deserialize, Deserializer, Serialize, de::Error as DeError};
use std::{
    collections::{HashMap, HashSet},
    ops::Range,
};

/// Client-ready committed view rendered from system configuration plus projection.
///
/// The view owns cloned provider-neutral [`Message`] payloads so callers can
/// move it directly into a Client request without gaining access to
/// Conversation message identities or mutable raw history.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EffectiveView {
    system: Option<String>,
    messages: Vec<Message>,
}

impl EffectiveView {
    /// Creates a view from already-rendered Client payloads.
    fn new(system: Option<String>, messages: Vec<Message>) -> Self {
        Self { system, messages }
    }

    /// Returns the system instructions kept outside committed message history.
    #[must_use]
    pub fn system(&self) -> Option<&str> {
        self.system.as_deref()
    }

    /// Returns the projected complete Client messages in request order.
    #[must_use]
    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    /// Reports whether no committed messages are visible.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    /// Returns the number of projected complete Client messages.
    #[must_use]
    pub fn len(&self) -> usize {
        self.messages.len()
    }

    /// Consumes the view into Client-request fields.
    #[must_use]
    pub fn into_parts(self) -> (Option<String>, Vec<Message>) {
        (self.system, self.messages)
    }
}

/// Frozen pending messages that can be appended to an effective view explicitly.
///
/// Active stream/non-stream partials remain hidden inside
/// [`PendingTurn`](crate::conversation::PendingTurn); this context only owns
/// cloned payloads that have already crossed a complete freeze boundary.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PendingContext {
    messages: Vec<Message>,
}

impl PendingContext {
    /// Creates a context from frozen pending payloads.
    fn new(messages: Vec<Message>) -> Self {
        Self { messages }
    }

    /// Returns the complete frozen pending Client messages in turn order.
    #[must_use]
    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    /// Reports whether the pending turn has no frozen messages.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    /// Returns the number of frozen pending Client messages.
    #[must_use]
    pub fn len(&self) -> usize {
        self.messages.len()
    }

    /// Consumes the context into owned Client messages.
    #[must_use]
    pub fn into_messages(self) -> Vec<Message> {
        self.messages
    }
}

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

    /// Rebuilds a persisted range claim from row fields.
    pub(crate) const fn from_persisted_parts(
        conversation_id: ConversationId,
        start_turn_count: u64,
        start_after_turn: Option<TurnId>,
        end_turn_count: u64,
        end_after_turn: Option<TurnId>,
    ) -> Self {
        Self {
            conversation_id,
            start: RangeEndpoint::new(start_turn_count, start_after_turn),
            end: RangeEndpoint::new(end_turn_count, end_after_turn),
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

    /// Rebuilds projection data from persistence rows before restore-time validation.
    pub(crate) fn from_persisted_parts(
        spans: Vec<Span>,
        artifacts: Vec<Artifact>,
    ) -> Result<Self, ProjectionError> {
        validate_projection_shape(&spans, &artifacts)?;
        Ok(Self { spans, artifacts })
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

    /// Preserves the current active overlay and appends one newly committed raw Turn.
    ///
    /// When a commit happens after a revert into a compacted cover, the
    /// visible prefix of that cover is materialized as raw spans for the new
    /// branch. Artifacts whose provenance still matches the retained prefix are
    /// kept as audit data; raw history itself remains untouched.
    pub(crate) fn extend_after_commit(
        self,
        conversation_id: ConversationId,
        turns: &[super::Turn],
        previous_active_len: usize,
    ) -> Self {
        debug_assert_eq!(turns.len(), previous_active_len + 1);

        let mut spans = active_projection_spans(conversation_id, &self, turns, previous_active_len);
        push_raw_span(
            &mut spans,
            conversation_id,
            turns,
            previous_active_len,
            previous_active_len + 1,
        );

        let artifacts = self
            .artifacts
            .into_iter()
            .filter(|artifact| {
                range_matches_turns(
                    artifact.provenance().input_range(),
                    conversation_id,
                    turns,
                    previous_active_len,
                )
            })
            .collect();

        Self { spans, artifacts }
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

    /// Renders committed history into Client-ready system and message fields.
    ///
    /// Rendering is capped at the current logical head. Raw spans clone the
    /// underlying Client payloads from immutable Turns; complete compacted
    /// spans clone their artifact messages. If the head falls inside a
    /// compacted cover, that visible prefix is rendered as raw Turns instead,
    /// so a summary that also covers future Turns is never exposed early.
    #[must_use]
    pub fn effective_view(&self) -> EffectiveView {
        let system = self.config.system().map(ToOwned::to_owned);
        let mut messages = Vec::new();
        let active_len = self.history.active_len();
        let lineage_len = self.history.lineage_len();
        let lineage_turns = self.history.lineage_turns();

        for span in self.projection.spans() {
            let (start, end) = span_bounds(span.range(), lineage_len);
            if start >= active_len {
                break;
            }

            let visible_end = end.min(active_len);
            match span {
                Span::Raw { .. } => {
                    extend_raw_messages(&mut messages, &lineage_turns[start..visible_end]);
                }
                Span::Compacted { artifact, .. } if end <= active_len => {
                    let artifact = self
                        .projection
                        .artifact(*artifact)
                        .expect("validated projection compacted span references an artifact");
                    messages.extend(artifact.messages().iter().cloned());
                }
                Span::Compacted { .. } => {
                    extend_raw_messages(&mut messages, &lineage_turns[start..visible_end]);
                }
            }
        }

        EffectiveView::new(system, messages)
    }

    /// Returns frozen pending messages without exposing the active partial.
    ///
    /// The committed [`effective_view`](Self::effective_view) intentionally
    /// excludes pending state. Callers that need to build an in-flight context
    /// can append this context explicitly; it contains only complete frozen
    /// payloads from the current pending turn, never the active accumulator.
    #[must_use]
    pub fn pending_context(&self) -> Option<PendingContext> {
        self.pending.as_ref().map(|pending| {
            PendingContext::new(
                pending
                    .messages()
                    .iter()
                    .map(|message| message.payload().clone())
                    .collect(),
            )
        })
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

    /// Revalidates a snapshot-restored projection against the addressable lineage.
    ///
    /// Normal projection construction checks complete coverage of the current
    /// logical head. A valid snapshot may have been taken after a revert, where
    /// the stored overlay still covers redo suffix turns beyond the head and
    /// `effective_view` clips rendering. Restore therefore validates owner,
    /// anchors, ordering, artifacts, and complete coverage of the restored
    /// lineage ceiling.
    pub(crate) fn validate_restored_projection(
        &self,
        projection: &Projection,
    ) -> Result<(), ProjectionError> {
        let artifact_index = validate_artifacts_for_lineage(self, projection.artifacts())?;
        validate_spans_for_lineage(self, projection.spans(), &artifact_index)
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

/// Builds the projection spans that are valid for the supplied active prefix.
fn active_projection_spans(
    conversation_id: ConversationId,
    projection: &Projection,
    turns: &[super::Turn],
    active_len: usize,
) -> Vec<Span> {
    debug_assert!(active_len <= turns.len());

    let mut spans = Vec::new();
    for span in projection.spans() {
        let (start, end) = checked_range_positions(span.range());
        if start >= active_len {
            break;
        }

        let visible_end = end.min(active_len);
        if start >= visible_end {
            continue;
        }

        match span {
            Span::Raw { .. } => {
                push_raw_span(&mut spans, conversation_id, turns, start, visible_end);
            }
            Span::Compacted { covers, .. }
                if end <= active_len
                    && range_matches_turns(covers, conversation_id, turns, active_len) =>
            {
                spans.push(span.clone());
            }
            Span::Compacted { .. } => {
                push_raw_span(&mut spans, conversation_id, turns, start, visible_end);
            }
        }
    }

    spans
}

/// Appends a raw span and merges it with a preceding adjacent raw span.
fn push_raw_span(
    spans: &mut Vec<Span>,
    conversation_id: ConversationId,
    turns: &[super::Turn],
    start: usize,
    end: usize,
) {
    if start == end {
        return;
    }

    if let Some(Span::Raw { turns: previous }) = spans.last_mut() {
        let previous_end = usize::try_from(previous.end_turn_count())
            .expect("checked projection end fits in memory");
        if previous_end == start {
            let previous_start = usize::try_from(previous.start_turn_count())
                .expect("checked projection start fits in memory");
            *previous =
                CheckedTurnRange::from_positions(conversation_id, turns, previous_start, end);
            return;
        }
    }

    spans.push(Span::raw(CheckedTurnRange::from_positions(
        conversation_id,
        turns,
        start,
        end,
    )));
}

/// Checks whether a stored range still matches a lineage prefix by anchors.
fn range_matches_turns(
    range: &CheckedTurnRange,
    conversation_id: ConversationId,
    turns: &[super::Turn],
    head_len: usize,
) -> bool {
    if range.conversation_id != conversation_id {
        return false;
    }
    if range.start_turn_count() >= range.end_turn_count() {
        return false;
    }

    let Ok(start) = usize::try_from(range.start_turn_count()) else {
        return false;
    };
    let Ok(end) = usize::try_from(range.end_turn_count()) else {
        return false;
    };
    if end > head_len || end > turns.len() {
        return false;
    }

    endpoint_matches(turns, start, range.start_after_turn())
        && endpoint_matches(turns, end, range.end_after_turn())
}

/// Checks one endpoint anchor against a materialized lineage.
fn endpoint_matches(turns: &[super::Turn], position: usize, after_turn: Option<TurnId>) -> bool {
    let expected = position
        .checked_sub(1)
        .and_then(|index| turns.get(index))
        .map(super::Turn::id);
    after_turn == expected
}

/// Converts a checked range to in-memory positions.
fn checked_range_positions(range: &CheckedTurnRange) -> (usize, usize) {
    let start =
        usize::try_from(range.start_turn_count()).expect("checked projection start fits in memory");
    let end =
        usize::try_from(range.end_turn_count()).expect("checked projection end fits in memory");
    debug_assert!(start <= end);
    (start, end)
}

/// Resolves stored span endpoints against an addressable lineage length.
fn span_bounds(range: &CheckedTurnRange, lineage_len: usize) -> (usize, usize) {
    let (start, end) = checked_range_positions(range);
    debug_assert!(end <= lineage_len);
    (start, end)
}

/// Appends raw Turn payloads without exposing their Conversation identities.
fn extend_raw_messages(messages: &mut Vec<Message>, turns: &[super::Turn]) {
    messages.extend(
        turns
            .iter()
            .flat_map(super::Turn::messages)
            .map(|message| message.payload().clone()),
    );
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

/// Checks artifact facts against the complete addressable lineage during restore.
fn validate_artifacts_for_lineage<'a>(
    conversation: &Conversation,
    artifacts: &'a [Artifact],
) -> Result<HashMap<ArtifactId, &'a Artifact>, ProjectionError> {
    let mut seen = HashSet::new();
    let mut index = HashMap::new();

    for artifact in artifacts {
        artifact.validate_messages()?;
        if !seen.insert(artifact.id()) {
            return Err(ProjectionError::DuplicateArtifactId {
                artifact_id: artifact.id(),
            });
        }
        conversation.resolve_range_against_lineage(artifact.provenance().input_range())?;
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

/// Checks restored spans over the complete addressable lineage.
fn validate_spans_for_lineage(
    conversation: &Conversation,
    spans: &[Span],
    artifacts: &HashMap<ArtifactId, &Artifact>,
) -> Result<(), ProjectionError> {
    let mut expected_start = 0usize;

    for span in spans {
        let resolved = conversation.resolve_range_against_lineage(span.range())?;
        if resolved.start < expected_start {
            return Err(ProjectionError::SpanOverlap {
                expected_start: usize_to_u64(expected_start),
                actual_start: usize_to_u64(resolved.start),
            });
        }
        if resolved.start > expected_start {
            return Err(ProjectionError::SpanGap {
                expected_start: usize_to_u64(expected_start),
                actual_start: usize_to_u64(resolved.start),
            });
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
                });
            }
            if artifact_value.provenance().produced_by() != produced_by {
                return Err(ProjectionError::ArtifactStrategyMismatch {
                    artifact_id: *artifact,
                });
            }
        }

        expected_start = resolved.end;
    }

    let lineage_len = conversation.history.lineage_len();
    if expected_start != lineage_len {
        return Err(ProjectionError::IncompleteProjection {
            expected_end: usize_to_u64(lineage_len),
            actual_end: usize_to_u64(expected_start),
        });
    }

    Ok(())
}

impl Conversation {
    /// Resolves one checked range against the complete restored lineage ceiling.
    fn resolve_range_against_lineage(
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

        let start = self.resolve_lineage_endpoint(range.start)?;
        let end = self.resolve_lineage_endpoint(range.end)?;
        debug_assert!(start < end);
        Ok(start..end)
    }

    /// Resolves one stored endpoint against the full addressable lineage.
    fn resolve_lineage_endpoint(&self, endpoint: RangeEndpoint) -> Result<usize, ProjectionError> {
        let lineage_len = self.history.lineage_len();
        let lineage_len_u64 = usize_to_u64(lineage_len);
        if endpoint.turn_count > lineage_len_u64 {
            return Err(ProjectionError::RangePositionOutOfRange {
                turn_count: endpoint.turn_count,
                lineage_turns: lineage_len_u64,
            });
        }

        let position = usize::try_from(endpoint.turn_count).map_err(|_| {
            ProjectionError::RangePositionOutOfRange {
                turn_count: endpoint.turn_count,
                lineage_turns: lineage_len_u64,
            }
        })?;
        let expected = position
            .checked_sub(1)
            .and_then(|index| self.history.lineage_turns().get(index))
            .map(super::Turn::id);

        if endpoint.after_turn != expected {
            if let Some(turn_id) = endpoint.after_turn {
                if self.history.contains_turn_id(turn_id) {
                    let on_lineage = self
                        .history
                        .lineage_turns()
                        .iter()
                        .any(|turn| turn.id() == turn_id);
                    if !on_lineage {
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

#[cfg(test)]
mod tests;
