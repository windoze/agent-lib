//! Compaction strategy and trigger extension-point tests.

use super::{
    artifact_id, begin_pending, commit_text_turn, conversation, message_labels, range,
    summary_message,
};
use crate::{
    conversation::{
        ArtifactDraft, CompactCtx, CompactionError, CompactionInput, CompactionPlan,
        CompactionStep, CompactionStrategy, CompactionStrategyResolver, CompactionTrigger,
        CompactionTriggerOutcome, Conversation, ConversationError, ProjectionError, StrategyRef,
        TokenAccounting, materialize_compaction_plan,
    },
    model::{message::Role, usage::Usage},
};
use async_trait::async_trait;
use serde_json::Map;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[derive(Debug)]
struct RuntimeHandle {
    marker: &'static str,
    calls: AtomicUsize,
}

impl RuntimeHandle {
    fn new(marker: &'static str) -> Arc<Self> {
        Arc::new(Self {
            marker,
            calls: AtomicUsize::new(0),
        })
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

struct MockStrategy {
    strategy_ref: StrategyRef,
    label: &'static str,
    handle: Arc<RuntimeHandle>,
    failure: Option<&'static str>,
}

impl MockStrategy {
    fn new(strategy_ref: StrategyRef, label: &'static str, handle: Arc<RuntimeHandle>) -> Self {
        Self {
            strategy_ref,
            label,
            handle,
            failure: None,
        }
    }

    fn failing(
        strategy_ref: StrategyRef,
        label: &'static str,
        handle: Arc<RuntimeHandle>,
        failure: &'static str,
    ) -> Self {
        Self {
            strategy_ref,
            label,
            handle,
            failure: Some(failure),
        }
    }
}

#[async_trait]
impl CompactionStrategy for MockStrategy {
    fn strategy_ref(&self) -> &StrategyRef {
        &self.strategy_ref
    }

    async fn compact(
        &self,
        input: CompactionInput<'_>,
        _ctx: &CompactCtx,
    ) -> Result<ArtifactDraft, CompactionError> {
        self.handle.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(message) = self.failure {
            return Err(CompactionError::StrategyFailed {
                strategy: self.strategy_ref.clone(),
                message: message.to_owned(),
            });
        }

        let label = format!(
            "{} summary spans={} messages={}",
            self.label,
            input.spans().len(),
            input.effective_view().len()
        );
        Ok(ArtifactDraft::new(
            vec![summary_message(label)],
            TokenAccounting::new(
                Usage {
                    input: 120,
                    ..Usage::default()
                },
                Usage {
                    input: 12,
                    ..Usage::default()
                },
            ),
            Map::new(),
        ))
    }
}

struct MockResolver {
    strategies: Vec<Box<dyn CompactionStrategy + Send + Sync>>,
}

impl MockResolver {
    fn new(strategies: Vec<Box<dyn CompactionStrategy + Send + Sync>>) -> Self {
        Self { strategies }
    }
}

impl CompactionStrategyResolver for MockResolver {
    fn resolve(&self, strategy: &StrategyRef) -> Option<&(dyn CompactionStrategy + Send + Sync)> {
        self.strategies
            .iter()
            .find(|candidate| candidate.strategy_ref() == strategy)
            .map(Box::as_ref)
    }
}

struct WrongResolver {
    strategy: MockStrategy,
}

impl CompactionStrategyResolver for WrongResolver {
    fn resolve(&self, _strategy: &StrategyRef) -> Option<&(dyn CompactionStrategy + Send + Sync)> {
        Some(&self.strategy)
    }
}

#[derive(Clone, Copy)]
enum PlanTarget {
    Raw,
    Spans,
}

struct PlanTrigger {
    target: PlanTarget,
    start: usize,
    end: usize,
    artifact_seed: u128,
    strategy: StrategyRef,
    handle: Arc<RuntimeHandle>,
}

impl PlanTrigger {
    fn raw(
        start: usize,
        end: usize,
        artifact_seed: u128,
        strategy: StrategyRef,
        handle: Arc<RuntimeHandle>,
    ) -> Self {
        Self {
            target: PlanTarget::Raw,
            start,
            end,
            artifact_seed,
            strategy,
            handle,
        }
    }

    fn spans(
        start: usize,
        end: usize,
        artifact_seed: u128,
        strategy: StrategyRef,
        handle: Arc<RuntimeHandle>,
    ) -> Self {
        Self {
            target: PlanTarget::Spans,
            start,
            end,
            artifact_seed,
            strategy,
            handle,
        }
    }
}

impl CompactionTrigger for PlanTrigger {
    fn evaluate(
        &self,
        conversation: &Conversation,
        _usage: &Usage,
    ) -> Result<CompactionTriggerOutcome, ConversationError> {
        self.handle.calls.fetch_add(1, Ordering::SeqCst);
        let target = range(conversation, self.start, self.end);
        let artifact = artifact_id(self.artifact_seed);
        let step = match self.target {
            PlanTarget::Raw => CompactionStep::raw(target, artifact, self.strategy.clone()),
            PlanTarget::Spans => CompactionStep::spans(target, artifact, self.strategy.clone()),
        };
        Ok(CompactionTriggerOutcome::plan(CompactionPlan::new(
            conversation,
            vec![step],
            Vec::new(),
        )))
    }
}

fn plan_from(outcome: CompactionTriggerOutcome) -> CompactionPlan {
    match outcome {
        CompactionTriggerOutcome::Plan { plan } => plan,
        CompactionTriggerOutcome::DeferredUntilBoundary { deferred } => {
            panic!("expected plan, got deferred: {deferred:?}")
        }
    }
}

async fn materialize(
    conversation: &Conversation,
    resolver: &dyn CompactionStrategyResolver,
    plan: &CompactionPlan,
) -> Result<CompactionPlan, ConversationError> {
    let view = conversation.effective_view();
    let input = CompactionInput::new(conversation.projection().spans(), &view);
    materialize_compaction_plan(Some(resolver), input, plan).await
}

#[tokio::test]
async fn strategy_trait_object_materializes_serde_plan_and_applies() {
    let mut conversation = conversation(210);
    commit_text_turn(&mut conversation, 211);
    commit_text_turn(&mut conversation, 212);
    commit_text_turn(&mut conversation, 213);
    commit_text_turn(&mut conversation, 214);

    let strategy_ref = StrategyRef::new("tiered", "v1");
    let trigger_handle = RuntimeHandle::new("mock-trigger-client");
    let trigger = PlanTrigger::raw(0, 2, 2100, strategy_ref.clone(), trigger_handle.clone());
    let plan = plan_from(
        conversation
            .evaluate_compaction_trigger(&trigger, &Usage::default())
            .expect("trigger evaluates at committed boundary"),
    );
    assert_eq!(trigger_handle.calls(), 1);
    assert!(plan.artifacts().is_empty());

    let encoded_plan = serde_json::to_string(&plan).expect("serialize trigger plan intent");
    assert!(encoded_plan.contains("\"tiered\""));
    assert!(!encoded_plan.contains(trigger_handle.marker));
    let decoded_plan: CompactionPlan =
        serde_json::from_str(&encoded_plan).expect("deserialize trigger plan intent");

    let strategy_handle = RuntimeHandle::new("mock-strategy-client");
    let resolver = MockResolver::new(vec![Box::new(MockStrategy::new(
        strategy_ref,
        "tiered",
        strategy_handle.clone(),
    ))]);
    let materialized = materialize(&conversation, &resolver, &decoded_plan)
        .await
        .expect("runtime strategy materializes artifacts");
    assert_eq!(strategy_handle.calls(), 1);
    assert_eq!(materialized.artifacts().len(), 1);

    let encoded_materialized =
        serde_json::to_string(&materialized).expect("serialize materialized data plan");
    assert!(!encoded_materialized.contains(strategy_handle.marker));
    conversation
        .apply_compaction(&materialized)
        .expect("materialized plan applies atomically");

    assert_eq!(
        message_labels(conversation.effective_view().messages()),
        vec![
            (
                Role::Assistant,
                "tiered summary spans=1 messages=8".to_owned()
            ),
            (Role::User, "question:213".to_owned()),
            (Role::Assistant, "answer:213".to_owned()),
            (Role::User, "question:214".to_owned()),
            (Role::Assistant, "answer:214".to_owned()),
        ]
    );
}

#[tokio::test]
async fn different_triggers_use_distinct_tiered_and_consolidated_strategies() {
    let mut conversation = conversation(220);
    commit_text_turn(&mut conversation, 221);
    commit_text_turn(&mut conversation, 222);
    commit_text_turn(&mut conversation, 223);
    commit_text_turn(&mut conversation, 224);

    let tiered_ref = StrategyRef::new("tiered", "v1");
    let consolidate_ref = StrategyRef::new("consolidate", "v2");
    let tiered_handle = RuntimeHandle::new("tiered-client");
    let consolidate_handle = RuntimeHandle::new("consolidate-client");
    let resolver = MockResolver::new(vec![
        Box::new(MockStrategy::new(
            tiered_ref.clone(),
            "tiered",
            tiered_handle.clone(),
        )),
        Box::new(MockStrategy::new(
            consolidate_ref.clone(),
            "consolidated",
            consolidate_handle.clone(),
        )),
    ]);

    let raw_trigger = PlanTrigger::raw(
        0,
        2,
        2200,
        tiered_ref,
        RuntimeHandle::new("raw-trigger-client"),
    );
    let raw_plan = plan_from(
        conversation
            .evaluate_compaction_trigger(&raw_trigger, &Usage::default())
            .expect("raw trigger evaluates"),
    );
    let raw_plan = materialize(&conversation, &resolver, &raw_plan)
        .await
        .expect("tiered strategy materializes raw target");
    conversation
        .apply_compaction(&raw_plan)
        .expect("tiered plan applies");
    assert_eq!(tiered_handle.calls(), 1);

    let span_trigger = PlanTrigger::spans(
        0,
        4,
        2201,
        consolidate_ref,
        RuntimeHandle::new("span-trigger-client"),
    );
    let span_plan = plan_from(
        conversation
            .evaluate_compaction_trigger(&span_trigger, &Usage::default())
            .expect("span trigger evaluates"),
    );
    let span_plan = materialize(&conversation, &resolver, &span_plan)
        .await
        .expect("consolidate strategy materializes span target");
    conversation
        .apply_compaction(&span_plan)
        .expect("consolidated plan applies");

    assert_eq!(consolidate_handle.calls(), 1);
    assert_eq!(conversation.projection().artifacts().len(), 2);
    assert_eq!(
        message_labels(conversation.effective_view().messages()),
        vec![(
            Role::Assistant,
            "consolidated summary spans=2 messages=5".to_owned()
        )]
    );
}

#[test]
fn pending_trigger_defers_without_invoking_runtime_trigger() {
    let mut conversation = conversation(230);
    commit_text_turn(&mut conversation, 231);
    begin_pending(&mut conversation, 232);

    let trigger_handle = RuntimeHandle::new("pending-trigger-client");
    let trigger = PlanTrigger::raw(
        0,
        1,
        2300,
        StrategyRef::new("pending", "v1"),
        trigger_handle.clone(),
    );
    let outcome = conversation
        .evaluate_compaction_trigger(&trigger, &Usage::default())
        .expect("pending trigger check returns deferred marker");

    let deferred = outcome.as_deferred().expect("pending defers");
    assert_eq!(deferred.conversation_id(), conversation.id());
    assert_eq!(deferred.version(), conversation.version());
    assert_eq!(
        deferred.pending_turn(),
        conversation
            .pending()
            .map(crate::conversation::PendingTurn::id)
    );
    assert_eq!(deferred.reason(), "pending turn is active");
    assert_eq!(trigger_handle.calls(), 0);
}

#[tokio::test]
async fn strategy_resolution_errors_are_classified_without_fallback() {
    let mut conversation = conversation(240);
    commit_text_turn(&mut conversation, 241);

    let requested = StrategyRef::new("requested", "v1");
    let plan = CompactionPlan::new(
        &conversation,
        vec![CompactionStep::raw(
            range(&conversation, 0, 1),
            artifact_id(2400),
            requested.clone(),
        )],
        Vec::new(),
    );
    let view = conversation.effective_view();
    let input = CompactionInput::new(conversation.projection().spans(), &view);

    let missing_registry = materialize_compaction_plan(None, input, &plan)
        .await
        .expect_err("no registry must not fallback");
    assert_eq!(
        missing_registry,
        ConversationError::Compaction(CompactionError::UnresolvedStrategy {
            strategy: requested.clone()
        })
    );

    let empty_resolver = MockResolver::new(Vec::new());
    let unresolved = materialize_compaction_plan(Some(&empty_resolver), input, &plan)
        .await
        .expect_err("missing strategy must not fallback");
    assert_eq!(
        unresolved,
        ConversationError::Compaction(CompactionError::UnresolvedStrategy {
            strategy: requested.clone()
        })
    );

    let wrong = StrategyRef::new("wrong", "v9");
    let wrong_resolver = WrongResolver {
        strategy: MockStrategy::new(wrong.clone(), "wrong", RuntimeHandle::new("wrong-client")),
    };
    let mismatch = materialize_compaction_plan(Some(&wrong_resolver), input, &plan)
        .await
        .expect_err("wrong strategy ref must be visible");
    assert_eq!(
        mismatch,
        ConversationError::Compaction(CompactionError::StrategyReferenceMismatch {
            expected: requested.clone(),
            actual: wrong,
        })
    );

    let failing_resolver = MockResolver::new(vec![Box::new(MockStrategy::failing(
        requested.clone(),
        "failing",
        RuntimeHandle::new("failing-client"),
        "summarizer rejected input",
    ))]);
    let failed = materialize_compaction_plan(Some(&failing_resolver), input, &plan)
        .await
        .expect_err("strategy failure must be classified");
    assert_eq!(
        failed,
        ConversationError::Compaction(CompactionError::StrategyFailed {
            strategy: requested,
            message: "summarizer rejected input".to_owned(),
        })
    );
}

#[test]
fn empty_strategy_draft_remains_projection_error_not_runtime_fallback() {
    let ctx = CompactCtx::new(
        crate::conversation::CompactionTarget::raw(range(
            &{
                let mut conversation = conversation(250);
                commit_text_turn(&mut conversation, 251);
                conversation
            },
            0,
            1,
        )),
        artifact_id(2500),
        StrategyRef::new("empty", "v1"),
    );
    let draft = ArtifactDraft::new(Vec::new(), TokenAccounting::default(), Map::new());
    let error = draft
        .into_artifact(&ctx)
        .expect_err("empty draft is rejected by artifact validation");
    assert_eq!(
        error,
        ConversationError::Projection(ProjectionError::EmptyArtifactMessages {
            artifact_id: artifact_id(2500),
        })
    );
}
