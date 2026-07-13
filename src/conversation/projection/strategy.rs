//! Runtime compaction strategy and trigger extension points.
//!
//! Strategies and triggers are runtime behavior. They are deliberately kept
//! out of Conversation snapshots; persisted state records only
//! [`StrategyRef`], [`CompactionPlan`], [`Artifact`], and artifact provenance.

use super::{
    Artifact, ArtifactProvenance, CompactionPlan, CompactionStep, CompactionTarget, EffectiveView,
    Span, StrategyRef, TokenAccounting,
};
use crate::{
    conversation::{
        ArtifactId, CompactionError, Conversation, ConversationError, ConversationId, TurnId,
    },
    model::{message::Message, usage::Usage},
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Read-only input supplied to a runtime compaction strategy.
///
/// This view contains the current projection spans and the Client-ready
/// committed context that the caller chose to compact against. It exposes no
/// mutable Conversation state and contains no pending partials.
#[derive(Clone, Copy, Debug)]
pub struct CompactionInput<'a> {
    spans: &'a [Span],
    effective_view: &'a EffectiveView,
}

impl<'a> CompactionInput<'a> {
    /// Creates read-only strategy input from caller-owned context values.
    #[must_use]
    pub const fn new(spans: &'a [Span], effective_view: &'a EffectiveView) -> Self {
        Self {
            spans,
            effective_view,
        }
    }

    /// Returns the projection spans visible to the runtime strategy.
    #[must_use]
    pub const fn spans(&self) -> &'a [Span] {
        self.spans
    }

    /// Returns the Client-ready committed context visible to the strategy.
    #[must_use]
    pub const fn effective_view(&self) -> &'a EffectiveView {
        self.effective_view
    }
}

/// Data context for one compaction strategy invocation.
///
/// The caller supplies the output artifact id and target strategy reference.
/// A strategy returns an [`ArtifactDraft`]; this context then binds the draft
/// to provenance without trusting the runtime object as persisted state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompactCtx {
    target: CompactionTarget,
    artifact: ArtifactId,
    produced_by: StrategyRef,
}

impl CompactCtx {
    /// Creates context for one strategy invocation.
    #[must_use]
    pub fn new(target: CompactionTarget, artifact: ArtifactId, produced_by: StrategyRef) -> Self {
        Self {
            target,
            artifact,
            produced_by,
        }
    }

    /// Creates invocation context from a data-only compaction step.
    #[must_use]
    pub fn from_step(step: &CompactionStep) -> Self {
        Self::new(
            step.target().clone(),
            step.artifact(),
            step.produced_by().clone(),
        )
    }

    /// Returns the target range and target kind for this invocation.
    #[must_use]
    pub const fn target(&self) -> &CompactionTarget {
        &self.target
    }

    /// Returns the caller-supplied artifact id to bind to the strategy output.
    #[must_use]
    pub const fn artifact(&self) -> ArtifactId {
        self.artifact
    }

    /// Returns the strategy reference requested by the plan step.
    #[must_use]
    pub const fn produced_by(&self) -> &StrategyRef {
        &self.produced_by
    }
}

/// Strategy-produced artifact data before provenance validation.
///
/// The draft intentionally does not contain an artifact id, covered range, or
/// strategy reference. Those facts come from [`CompactCtx`] so a runtime object
/// cannot silently rewrite the persisted provenance of the plan step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArtifactDraft {
    messages: Vec<Message>,
    tokens: TokenAccounting,
    extra: Map<String, Value>,
}

impl ArtifactDraft {
    /// Creates draft render messages plus token accounting.
    #[must_use]
    pub fn new(messages: Vec<Message>, tokens: TokenAccounting, extra: Map<String, Value>) -> Self {
        Self {
            messages,
            tokens,
            extra,
        }
    }

    /// Returns complete Client messages proposed for artifact rendering.
    #[must_use]
    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    /// Returns token accounting proposed by the strategy.
    #[must_use]
    pub const fn tokens(&self) -> &TokenAccounting {
        &self.tokens
    }

    /// Returns optional strategy-provided provenance metadata.
    #[must_use]
    pub const fn extra(&self) -> &Map<String, Value> {
        &self.extra
    }

    /// Binds this draft to checked target data and returns a persisted artifact.
    ///
    /// # Errors
    ///
    /// Returns a projection error when the draft does not contain at least one
    /// complete render message.
    pub fn into_artifact(self, ctx: &CompactCtx) -> Result<Artifact, ConversationError> {
        let provenance = ArtifactProvenance::new(
            ctx.target().range().clone(),
            ctx.produced_by().clone(),
            self.tokens,
            self.extra,
        );
        Artifact::new(ctx.artifact(), self.messages, provenance).map_err(Into::into)
    }
}

/// Dyn-safe asynchronous artifact producer for one compaction strategy.
///
/// Implementations may call external model clients or local summarizers, but
/// those runtime handles remain outside Conversation data and serde.
#[async_trait]
pub trait CompactionStrategy: Send + Sync {
    /// Returns the serializable identity used to resolve this runtime instance.
    fn strategy_ref(&self) -> &StrategyRef;

    /// Produces an unpersisted artifact draft for one compaction step.
    async fn compact(
        &self,
        input: CompactionInput<'_>,
        ctx: &CompactCtx,
    ) -> Result<ArtifactDraft, CompactionError>;
}

/// Read-only runtime resolver for compaction strategies.
///
/// The resolver itself is behavior, not data. It is never stored on
/// [`Conversation`]; callers pass it explicitly at strategy execution time.
pub trait CompactionStrategyResolver: Send + Sync {
    /// Resolves a strategy reference to a runtime strategy instance.
    fn resolve(&self, strategy: &StrategyRef) -> Option<&(dyn CompactionStrategy + Send + Sync)>;
}

/// Resolves and runs one strategy invocation, then binds the draft to an artifact.
///
/// # Errors
///
/// Returns [`CompactionError::UnresolvedStrategy`] when no resolver or matching
/// runtime instance is supplied, [`CompactionError::StrategyReferenceMismatch`]
/// when a resolver returns the wrong instance, strategy errors from the
/// runtime implementation, or projection errors for invalid artifact drafts.
pub async fn run_compaction_strategy(
    resolver: Option<&dyn CompactionStrategyResolver>,
    input: CompactionInput<'_>,
    ctx: &CompactCtx,
) -> Result<Artifact, ConversationError> {
    let Some(resolver) = resolver else {
        return Err(CompactionError::UnresolvedStrategy {
            strategy: ctx.produced_by().clone(),
        }
        .into());
    };
    let Some(strategy) = resolver.resolve(ctx.produced_by()) else {
        return Err(CompactionError::UnresolvedStrategy {
            strategy: ctx.produced_by().clone(),
        }
        .into());
    };

    let actual = strategy.strategy_ref().clone();
    if &actual != ctx.produced_by() {
        return Err(CompactionError::StrategyReferenceMismatch {
            expected: ctx.produced_by().clone(),
            actual,
        }
        .into());
    }

    strategy.compact(input, ctx).await?.into_artifact(ctx)
}

/// Runs every strategy referenced by a plan and returns a materialized plan.
///
/// Existing plan artifacts are not used as a fallback. The returned plan keeps
/// the same owner, version, head and steps, but replaces artifacts with drafts
/// produced through the supplied resolver.
pub async fn materialize_compaction_plan(
    resolver: Option<&dyn CompactionStrategyResolver>,
    input: CompactionInput<'_>,
    plan: &CompactionPlan,
) -> Result<CompactionPlan, ConversationError> {
    let mut artifacts = Vec::with_capacity(plan.steps().len());
    for step in plan.steps() {
        let ctx = CompactCtx::from_step(step);
        artifacts.push(run_compaction_strategy(resolver, input, &ctx).await?);
    }
    Ok(plan.with_artifacts(artifacts))
}

/// Data-only marker that a trigger wants compaction retried at a later boundary.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeferredUntilBoundary {
    conversation_id: ConversationId,
    version: u64,
    pending_turn: Option<TurnId>,
    reason: String,
}

impl DeferredUntilBoundary {
    /// Creates a deferred marker from caller-owned facts.
    #[must_use]
    pub fn new(
        conversation_id: ConversationId,
        version: u64,
        pending_turn: Option<TurnId>,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            conversation_id,
            version,
            pending_turn,
            reason: reason.into(),
        }
    }

    /// Creates a standard marker for an active pending turn.
    #[must_use]
    pub fn pending(conversation: &Conversation, pending_turn: TurnId) -> Self {
        Self::new(
            conversation.id(),
            conversation.version(),
            Some(pending_turn),
            "pending turn is active",
        )
    }

    /// Returns the Conversation identity observed by the trigger check.
    #[must_use]
    pub const fn conversation_id(&self) -> ConversationId {
        self.conversation_id
    }

    /// Returns the structural version observed by the trigger check.
    #[must_use]
    pub const fn version(&self) -> u64 {
        self.version
    }

    /// Returns the pending Turn that prevented boundary execution, if any.
    #[must_use]
    pub const fn pending_turn(&self) -> Option<TurnId> {
        self.pending_turn
    }

    /// Returns the stable deferred reason.
    #[must_use]
    pub fn reason(&self) -> &str {
        &self.reason
    }
}

/// Data-only trigger result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CompactionTriggerOutcome {
    /// A trigger produced a data-only compaction plan intent.
    Plan {
        /// Plan returned by the trigger.
        plan: CompactionPlan,
    },

    /// The trigger check should be repeated after the next complete boundary.
    DeferredUntilBoundary {
        /// Deferred marker explaining why no plan was produced.
        deferred: DeferredUntilBoundary,
    },
}

impl CompactionTriggerOutcome {
    /// Creates a plan outcome.
    #[must_use]
    pub const fn plan(plan: CompactionPlan) -> Self {
        Self::Plan { plan }
    }

    /// Creates a deferred outcome.
    #[must_use]
    pub const fn deferred_until_boundary(deferred: DeferredUntilBoundary) -> Self {
        Self::DeferredUntilBoundary { deferred }
    }

    /// Returns the plan if this is a plan outcome.
    #[must_use]
    pub const fn as_plan(&self) -> Option<&CompactionPlan> {
        match self {
            Self::Plan { plan } => Some(plan),
            Self::DeferredUntilBoundary { .. } => None,
        }
    }

    /// Returns the deferred marker if this is a deferred outcome.
    #[must_use]
    pub const fn as_deferred(&self) -> Option<&DeferredUntilBoundary> {
        match self {
            Self::Plan { .. } => None,
            Self::DeferredUntilBoundary { deferred } => Some(deferred),
        }
    }
}

/// Synchronous boundary observer that decides whether to compact.
///
/// Implementations receive immutable Conversation state and aggregate usage
/// facts. They may return a data-only plan intent or a deferred marker, but
/// cannot directly mutate projection or raw history through this trait.
pub trait CompactionTrigger: Send + Sync {
    /// Evaluates the trigger at a committed Turn boundary.
    fn evaluate(
        &self,
        conversation: &Conversation,
        usage: &Usage,
    ) -> Result<CompactionTriggerOutcome, ConversationError>;
}

impl Conversation {
    /// Evaluates a compaction trigger without letting it mutate the Conversation.
    ///
    /// When a pending turn is active this method does not call the trigger and
    /// instead returns [`DeferredUntilBoundary`]. At a committed boundary it
    /// passes immutable Conversation state plus usage to the trigger.
    pub fn evaluate_compaction_trigger(
        &self,
        trigger: &dyn CompactionTrigger,
        usage: &Usage,
    ) -> Result<CompactionTriggerOutcome, ConversationError> {
        if let Some(pending) = self.pending() {
            return Ok(CompactionTriggerOutcome::deferred_until_boundary(
                DeferredUntilBoundary::pending(self, pending.id()),
            ));
        }
        trigger.evaluate(self, usage)
    }
}
