//! Serializable compaction plans and atomic projection replacement.
//!
//! A plan is pure data: it names checked Turn ranges, references a
//! [`StrategyRef`], and supplies externally produced [`Artifact`] values. The
//! Conversation applies the plan by validating every target against the current
//! committed projection, then replacing the overlay in one assignment.

use super::{
    Artifact, CheckedTurnRange, Projection, Span, StrategyRef, active_projection_spans,
    checked_range_positions, push_raw_span, range_matches_turns, usize_to_u64,
};
use crate::conversation::{
    ArtifactId, Conversation, ConversationError, ConversationId, ProjectionError,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    ops::Range,
};

/// Serializable target for one compaction replacement.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CompactionTarget {
    /// A range that must currently render through raw spans.
    Raw {
        /// Complete Turn range to replace with a compacted span.
        turns: CheckedTurnRange,
    },

    /// A range that must align with existing projection span boundaries.
    Spans {
        /// Complete projection-span range to replace with a compacted span.
        covers: CheckedTurnRange,
    },
}

impl CompactionTarget {
    /// Creates a target that may split raw spans on Turn boundaries.
    #[must_use]
    pub fn raw(turns: CheckedTurnRange) -> Self {
        Self::Raw { turns }
    }

    /// Creates a target that must start and end on current span boundaries.
    #[must_use]
    pub fn spans(covers: CheckedTurnRange) -> Self {
        Self::Spans { covers }
    }

    /// Returns the raw Turn range covered by this target.
    #[must_use]
    pub const fn range(&self) -> &CheckedTurnRange {
        match self {
            Self::Raw { turns } => turns,
            Self::Spans { covers } => covers,
        }
    }
}

/// One replacement step inside a [`CompactionPlan`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompactionStep {
    target: CompactionTarget,
    artifact: ArtifactId,
    produced_by: StrategyRef,
}

impl CompactionStep {
    /// Creates a raw-range replacement step.
    #[must_use]
    pub fn raw(turns: CheckedTurnRange, artifact: ArtifactId, produced_by: StrategyRef) -> Self {
        Self {
            target: CompactionTarget::raw(turns),
            artifact,
            produced_by,
        }
    }

    /// Creates an existing-span replacement step.
    #[must_use]
    pub fn spans(covers: CheckedTurnRange, artifact: ArtifactId, produced_by: StrategyRef) -> Self {
        Self {
            target: CompactionTarget::spans(covers),
            artifact,
            produced_by,
        }
    }

    /// Returns the checked target declaration.
    #[must_use]
    pub const fn target(&self) -> &CompactionTarget {
        &self.target
    }

    /// Returns the artifact that will render the replacement span.
    #[must_use]
    pub const fn artifact(&self) -> ArtifactId {
        self.artifact
    }

    /// Returns the strategy reference recorded on the replacement span.
    #[must_use]
    pub const fn produced_by(&self) -> &StrategyRef {
        &self.produced_by
    }
}

/// Serializable data-only compaction request.
///
/// The plan records the Conversation owner, structural version, and logical
/// head observed by the caller. It deliberately stores no strategy object,
/// model client, closure, or registry handle; runtime behavior is resolved
/// outside Conversation Core.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompactionPlan {
    conversation_id: ConversationId,
    version: u64,
    head_turn_count: u64,
    steps: Vec<CompactionStep>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    artifacts: Vec<Artifact>,
}

impl CompactionPlan {
    /// Creates a plan bound to the Conversation's current version and head.
    ///
    /// Construction is intentionally light-weight so a caller can persist a
    /// deferred intent. [`Conversation::apply_compaction`] revalidates every
    /// step and artifact against the then-current committed state.
    #[must_use]
    pub fn new(
        conversation: &Conversation,
        steps: Vec<CompactionStep>,
        artifacts: Vec<Artifact>,
    ) -> Self {
        Self {
            conversation_id: conversation.id(),
            version: conversation.version(),
            head_turn_count: usize_to_u64(conversation.history.active_len()),
            steps,
            artifacts,
        }
    }

    /// Returns the Conversation identity this plan was prepared for.
    #[must_use]
    pub const fn conversation_id(&self) -> ConversationId {
        self.conversation_id
    }

    /// Returns the structural version observed by the plan producer.
    #[must_use]
    pub const fn version(&self) -> u64 {
        self.version
    }

    /// Returns the logical head size observed by the plan producer.
    #[must_use]
    pub const fn head_turn_count(&self) -> u64 {
        self.head_turn_count
    }

    /// Returns replacement steps in the caller-declared order.
    #[must_use]
    pub fn steps(&self) -> &[CompactionStep] {
        &self.steps
    }

    /// Returns externally produced artifacts supplied by this plan.
    #[must_use]
    pub fn artifacts(&self) -> &[Artifact] {
        &self.artifacts
    }

    /// Returns the same data-only plan header and steps with replacement artifacts.
    ///
    /// This is useful when a synchronous trigger first emits a plan intent and
    /// an asynchronous runtime strategy later materializes the artifacts for
    /// those exact steps. Existing artifacts are replaced rather than used as a
    /// fallback.
    #[must_use]
    pub fn with_artifacts(&self, artifacts: Vec<Artifact>) -> Self {
        Self {
            conversation_id: self.conversation_id,
            version: self.version,
            head_turn_count: self.head_turn_count,
            steps: self.steps.clone(),
            artifacts,
        }
    }
}

impl Conversation {
    /// Applies a checked compaction plan by atomically replacing the projection.
    ///
    /// Raw history, Turn ids, message ids, payloads, and tool pairings are
    /// never modified. Replaced artifacts that still validate against the
    /// current head remain in the projection artifact set as provenance/audit
    /// data even when no span references them after consolidation.
    ///
    /// # Errors
    ///
    /// Returns classified projection errors for stale plans, pending state,
    /// a reverted head, invalid targets, artifact/provenance mismatches,
    /// overlapping steps, or final projection inconsistency. Every error
    /// leaves the previous projection, artifacts, history, head, index, and
    /// version unchanged.
    pub fn apply_compaction(&mut self, plan: &CompactionPlan) -> Result<(), ConversationError> {
        self.validate_compaction_plan_header(plan)?;
        let next_version =
            self.version
                .checked_add(1)
                .ok_or(ConversationError::NonAtomicProjectionUpdate {
                    current_version: self.version,
                })?;

        let projection = self.compacted_projection(plan)?;
        self.projection = projection;
        self.version = next_version;
        Ok(())
    }

    /// Checks plan-level owner, version, head, pending, tip, and non-empty state.
    fn validate_compaction_plan_header(
        &self,
        plan: &CompactionPlan,
    ) -> Result<(), ConversationError> {
        if plan.conversation_id != self.id {
            return Err(ProjectionError::CompactionOwnerMismatch {
                expected: self.id,
                actual: plan.conversation_id,
            }
            .into());
        }
        if plan.version != self.version {
            return Err(ProjectionError::StaleCompactionPlan {
                plan_version: plan.version,
                current_version: self.version,
            }
            .into());
        }
        if let Some(pending) = &self.pending {
            return Err(ProjectionError::PendingTurn {
                turn_id: pending.id(),
            }
            .into());
        }

        let current_head = usize_to_u64(self.history.active_len());
        if plan.head_turn_count != current_head {
            return Err(ProjectionError::CompactionHeadMismatch {
                plan_head: plan.head_turn_count,
                current_head,
            }
            .into());
        }
        // Compacting a reverted head would rebuild the projection over the
        // active prefix only; a later redo to the lineage tip would then
        // silently drop the tail turns from the effective view.
        let lineage_len = usize_to_u64(self.history.lineage_len());
        if current_head != lineage_len {
            return Err(ProjectionError::CompactionOnRevertedHead {
                head: current_head,
                lineage_len,
            }
            .into());
        }
        if plan.steps.is_empty() {
            return Err(ProjectionError::EmptyCompactionPlan.into());
        }
        Ok(())
    }

    /// Builds the next projection in temporary state.
    fn compacted_projection(&self, plan: &CompactionPlan) -> Result<Projection, ConversationError> {
        let source_spans = source_spans(self);
        let plan_artifacts = plan_artifact_index(self, plan)?;
        let steps = resolve_steps(self, plan, &plan_artifacts, &source_spans)?;
        let spans = build_replacement_spans(self, &source_spans, &steps)?;

        let mut artifacts = retained_current_artifacts(self);
        artifacts.extend(plan.artifacts().iter().cloned());

        Projection::new(self, spans, artifacts)
    }
}

/// One active source span with resolved Turn positions.
#[derive(Clone, Debug)]
struct SourceSpan {
    span: Span,
    range: Range<usize>,
}

/// One validated replacement step with resolved Turn positions.
#[derive(Clone, Debug)]
struct ResolvedStep {
    target: CompactionTarget,
    artifact: ArtifactId,
    produced_by: StrategyRef,
    range: Range<usize>,
}

/// Returns the current projection as active-head source spans.
fn source_spans(conversation: &Conversation) -> Vec<SourceSpan> {
    active_projection_spans(
        conversation.id,
        &conversation.projection,
        conversation.history.lineage_turns(),
        conversation.history.active_len(),
    )
    .into_iter()
    .map(|span| {
        let (start, end) = checked_range_positions(span.range());
        SourceSpan {
            span,
            range: start..end,
        }
    })
    .collect()
}

/// Builds an index of new artifacts supplied by the plan.
fn plan_artifact_index<'a>(
    conversation: &Conversation,
    plan: &'a CompactionPlan,
) -> Result<HashMap<ArtifactId, &'a Artifact>, ConversationError> {
    let mut seen = HashSet::new();
    let mut artifacts = HashMap::new();

    for artifact in plan.artifacts() {
        artifact.validate_messages()?;
        if !seen.insert(artifact.id()) {
            return Err(ProjectionError::DuplicateArtifactId {
                artifact_id: artifact.id(),
            }
            .into());
        }
        conversation.validate_checked_turn_range(artifact.provenance().input_range())?;
        artifacts.insert(artifact.id(), artifact);
    }

    Ok(artifacts)
}

/// Resolves and validates every replacement step before any mutation.
fn resolve_steps(
    conversation: &Conversation,
    plan: &CompactionPlan,
    plan_artifacts: &HashMap<ArtifactId, &Artifact>,
    source_spans: &[SourceSpan],
) -> Result<Vec<ResolvedStep>, ConversationError> {
    let mut previous_end = 0usize;
    let mut referenced_artifacts = HashSet::new();
    let mut resolved_steps = Vec::with_capacity(plan.steps().len());

    for step in plan.steps() {
        let range = conversation.resolve_checked_turn_range(step.target().range())?;
        if range.start < previous_end {
            return Err(ProjectionError::SpanOverlap {
                expected_start: usize_to_u64(previous_end),
                actual_start: usize_to_u64(range.start),
            }
            .into());
        }

        let artifact =
            plan_artifacts
                .get(&step.artifact())
                .ok_or(ProjectionError::MissingArtifact {
                    artifact_id: step.artifact(),
                })?;
        if artifact.provenance().input_range() != step.target().range() {
            return Err(ProjectionError::ArtifactRangeMismatch {
                artifact_id: artifact.id(),
            }
            .into());
        }
        if artifact.provenance().produced_by() != step.produced_by() {
            return Err(ProjectionError::ArtifactStrategyMismatch {
                artifact_id: artifact.id(),
            }
            .into());
        }

        validate_target(step.target(), &range, source_spans)?;
        previous_end = range.end;
        referenced_artifacts.insert(step.artifact());
        resolved_steps.push(ResolvedStep {
            target: step.target().clone(),
            artifact: step.artifact(),
            produced_by: step.produced_by().clone(),
            range,
        });
    }

    for artifact in plan.artifacts() {
        if !referenced_artifacts.contains(&artifact.id()) {
            return Err(ProjectionError::UnreferencedCompactionArtifact {
                artifact_id: artifact.id(),
            }
            .into());
        }
    }

    Ok(resolved_steps)
}

/// Checks that one target matches the current source projection.
fn validate_target(
    target: &CompactionTarget,
    range: &Range<usize>,
    source_spans: &[SourceSpan],
) -> Result<(), ProjectionError> {
    let overlaps = overlapping_spans(source_spans, range);
    match target {
        CompactionTarget::Raw { .. } => {
            if overlaps
                .iter()
                .any(|span| !matches!(span.span, Span::Raw { .. }))
            {
                return Err(ProjectionError::CompactionTargetNotRaw {
                    start: usize_to_u64(range.start),
                    end: usize_to_u64(range.end),
                });
            }
        }
        CompactionTarget::Spans { .. } => {
            let aligned = overlaps
                .first()
                .is_some_and(|first| first.range.start == range.start)
                && overlaps
                    .last()
                    .is_some_and(|last| last.range.end == range.end);
            if !aligned {
                return Err(ProjectionError::CompactionTargetNotSpanAligned {
                    start: usize_to_u64(range.start),
                    end: usize_to_u64(range.end),
                });
            }
        }
    }

    Ok(())
}

/// Returns source spans that overlap a resolved range.
fn overlapping_spans<'a>(
    source_spans: &'a [SourceSpan],
    range: &Range<usize>,
) -> Vec<&'a SourceSpan> {
    source_spans
        .iter()
        .filter(|span| span.range.start < range.end && span.range.end > range.start)
        .collect()
}

/// Builds the final span list from validated replacement steps.
fn build_replacement_spans(
    conversation: &Conversation,
    source_spans: &[SourceSpan],
    steps: &[ResolvedStep],
) -> Result<Vec<Span>, ConversationError> {
    let mut spans = Vec::new();
    let mut cursor = 0usize;

    for step in steps {
        append_source_segment(
            conversation,
            source_spans,
            cursor,
            step.range.start,
            &mut spans,
        )?;
        spans.push(Span::compacted(
            step.target.range().clone(),
            step.artifact,
            step.produced_by.clone(),
        ));
        cursor = step.range.end;
    }

    append_source_segment(
        conversation,
        source_spans,
        cursor,
        conversation.history.active_len(),
        &mut spans,
    )?;
    Ok(spans)
}

/// Copies a non-targeted source segment into the output spans.
fn append_source_segment(
    conversation: &Conversation,
    source_spans: &[SourceSpan],
    start: usize,
    end: usize,
    output: &mut Vec<Span>,
) -> Result<(), ConversationError> {
    if start == end {
        return Ok(());
    }

    for source in overlapping_spans(source_spans, &(start..end)) {
        let segment_start = source.range.start.max(start);
        let segment_end = source.range.end.min(end);
        if segment_start == segment_end {
            continue;
        }

        match &source.span {
            Span::Raw { .. } => push_raw_span(
                output,
                conversation.id,
                conversation.history.turns(),
                segment_start,
                segment_end,
            ),
            Span::Compacted { .. }
                if segment_start == source.range.start && segment_end == source.range.end =>
            {
                output.push(source.span.clone());
            }
            Span::Compacted { .. } => {
                return Err(ProjectionError::CompactionTargetNotSpanAligned {
                    start: usize_to_u64(segment_start),
                    end: usize_to_u64(segment_end),
                }
                .into());
            }
        }
    }

    Ok(())
}

/// Keeps current artifacts that still belong to the active head.
fn retained_current_artifacts(conversation: &Conversation) -> Vec<Artifact> {
    conversation
        .projection
        .artifacts()
        .iter()
        .filter(|artifact| {
            range_matches_turns(
                artifact.provenance().input_range(),
                conversation.id,
                conversation.history.lineage_turns(),
                conversation.history.active_len(),
            )
        })
        .cloned()
        .collect()
}
