//! Deterministic id sources for tests.
//!
//! [`SeqIds`] is a clone-cheap, thread-safe id source that hands out globally
//! unique identities from a single shared monotonic counter. It implements the
//! `agent-lib` public [`RequirementIds`] and [`ToolExecutionIds`] contracts so
//! the driver can mint framework ids from it, and it also exposes inherent
//! helpers that fixtures use to fabricate typed ids ([`RunId`],
//! [`ConversationId`], and friends).
//!
//! # Uniqueness model
//!
//! Every generated UUID is `((base << 64) | seq)` where `seq` is drawn from a
//! single [`AtomicU64`] shared by every clone and every [`fork`](SeqIds::fork).
//! Because that counter is monotonic and shared, the low 64 bits never repeat,
//! so two ids can never collide regardless of their `base`. The `base` only
//! decorates the high bits so ids from distinct subtrees stay visually
//! distinguishable. The counter starts at `1`, so no id is ever the nil UUID.
//!
//! - [`clone`](Clone::clone) keeps the same `base` and shares the counter, so a
//!   parent scope and its clone never mint the same id.
//! - [`fork`](SeqIds::fork) allocates a fresh `base` (a new subtree) while still
//!   sharing the counter, so child/subagent ids stay globally unique and carry
//!   a readable nested label for trace nodes.
//! - [`named`](SeqIds::named) relabels without a new subtree.
//!
//! # Allocation log and failure mode
//!
//! Requirement-id allocations made through the [`RequirementIds`] contract are
//! recorded in order, keyed by [`RequirementKindTag`], and queryable via
//! [`requirement_log`](SeqIds::requirement_log) /
//! [`requirement_ids`](SeqIds::requirement_ids). An
//! [`exhausted`](SeqIds::exhausted) (or [`with_budget`](SeqIds::with_budget))
//! source drives the id-unavailable paths: its contract methods return
//! [`RequirementError::IdUnavailable`] and [`ToolRuntimeError::IdUnavailable`].

use std::{
    fmt,
    sync::{
        Arc, Mutex,
        atomic::{AtomicI64, AtomicU64, Ordering},
    },
};

use agent_lib::{
    agent::{
        AgentId, RequirementError, RequirementId, RequirementIds, RequirementKindTag, RunId,
        StepId, ToolExecutionIds, ToolRuntimeError, ToolSetId, TraceNodeId,
    },
    conversation::{ConversationId, MessageId, ToolCallId, TurnId},
    model::tool::ToolCall,
};
use uuid::Uuid;

/// Sentinel stored in [`Shared::remaining`] to mark an unlimited budget.
const UNLIMITED: i64 = -1;

/// One recorded requirement-id allocation made through [`RequirementIds`].
///
/// The log preserves allocation order so tests can assert the sequence of
/// requirement families a machine reified, and which id each one received.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RequirementAllocation {
    /// Requirement family the id was minted for.
    pub tag: RequirementKindTag,
    /// Framework id handed to that requirement.
    pub id: RequirementId,
}

/// State shared by every clone and fork of a [`SeqIds`] tree.
///
/// Sharing the counter is what makes ids globally unique across the whole tree;
/// sharing the log and budget makes assertions and the failure mode observable
/// from any handle into the tree.
struct Shared {
    /// Monotonic source of the low 64 bits of every generated UUID.
    counter: AtomicU64,
    /// Source of fresh `base` values handed to [`SeqIds::fork`].
    base_counter: AtomicU64,
    /// Ordered requirement-id allocations made through [`RequirementIds`].
    requirement_log: Mutex<Vec<RequirementAllocation>>,
    /// Remaining contract-method budget, or [`UNLIMITED`].
    remaining: AtomicI64,
}

impl fmt::Debug for Shared {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Shared")
            .field("counter", &self.counter.load(Ordering::SeqCst))
            .field("base_counter", &self.base_counter.load(Ordering::SeqCst))
            .field(
                "logged_requirements",
                &self
                    .requirement_log
                    .lock()
                    .map(|log| log.len())
                    .unwrap_or(0),
            )
            .field("remaining", &self.remaining.load(Ordering::SeqCst))
            .finish()
    }
}

/// Deterministic, clone-cheap id source for agent-layer tests.
///
/// See the [module docs](crate::ids) for the uniqueness model, allocation log,
/// and failure mode.
#[derive(Clone, Debug)]
pub struct SeqIds {
    /// Counter, log, and budget shared across the whole clone/fork tree.
    shared: Arc<Shared>,
    /// High 64 bits of ids minted by this handle; identifies its subtree.
    base: u64,
    /// Human-readable label woven into [`TraceNodeId`]s from this handle.
    label: Arc<str>,
}

impl Default for SeqIds {
    fn default() -> Self {
        Self::new()
    }
}

impl SeqIds {
    /// Creates a fresh root id source with an unlimited budget.
    #[must_use]
    pub fn new() -> Self {
        Self::with_shared(Shared {
            counter: AtomicU64::new(1),
            base_counter: AtomicU64::new(1),
            requirement_log: Mutex::new(Vec::new()),
            remaining: AtomicI64::new(UNLIMITED),
        })
    }

    /// Creates a root id source whose contract methods succeed exactly `budget`
    /// times before returning the id-unavailable errors.
    ///
    /// Inherent helpers ([`run_id`](Self::run_id) and friends) ignore the
    /// budget; only the [`RequirementIds`] / [`ToolExecutionIds`] contract
    /// methods consume it, so tests can drive the id-unavailable paths.
    #[must_use]
    pub fn with_budget(budget: u64) -> Self {
        let budget = i64::try_from(budget).unwrap_or(i64::MAX);
        Self::with_shared(Shared {
            counter: AtomicU64::new(1),
            base_counter: AtomicU64::new(1),
            requirement_log: Mutex::new(Vec::new()),
            remaining: AtomicI64::new(budget),
        })
    }

    /// Creates a root id source that is already exhausted.
    ///
    /// Every [`RequirementIds`] / [`ToolExecutionIds`] contract call returns the
    /// id-unavailable error, exercising the failure paths.
    #[must_use]
    pub fn exhausted() -> Self {
        Self::with_budget(0)
    }

    /// Builds a root handle over freshly constructed [`Shared`] state.
    fn with_shared(shared: Shared) -> Self {
        Self {
            shared: Arc::new(shared),
            base: 0,
            label: Arc::from("root"),
        }
    }

    /// Returns a child handle for a new subtree, sharing the counter, log, and
    /// budget while carrying a fresh `base` and a nested, readable `label`.
    ///
    /// Ids from the child never collide with the parent because the counter is
    /// shared; the distinct `base` only makes the two subtrees visually
    /// distinguishable and gives forked [`TraceNodeId`]s a nested label.
    #[must_use]
    pub fn fork(&self, label: &str) -> Self {
        let base = self.shared.base_counter.fetch_add(1, Ordering::SeqCst);
        let label: Arc<str> = Arc::from(format!("{}/{label}", self.label));
        Self {
            shared: Arc::clone(&self.shared),
            base,
            label,
        }
    }

    /// Returns a handle that shares this one's subtree but carries a new
    /// `label`, so its trace-node ids read differently without a new `base`.
    #[must_use]
    pub fn named(&self, label: &str) -> Self {
        Self {
            shared: Arc::clone(&self.shared),
            base: self.base,
            label: Arc::from(label),
        }
    }

    /// Returns the readable label woven into this handle's trace-node ids.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Draws the next globally unique 128-bit value from the shared counter.
    fn next_u128(&self) -> u128 {
        let seq = self.shared.counter.fetch_add(1, Ordering::SeqCst);
        (u128::from(self.base) << 64) | u128::from(seq)
    }

    /// Draws the next globally unique UUID from the shared counter.
    fn next_uuid(&self) -> Uuid {
        Uuid::from_u128(self.next_u128())
    }

    /// Consumes one unit of contract budget.
    ///
    /// Returns `true` when a contract method may proceed and `false` when the
    /// source is exhausted. An [`UNLIMITED`] budget always succeeds.
    fn take_budget(&self) -> bool {
        let mut current = self.shared.remaining.load(Ordering::SeqCst);
        loop {
            if current == UNLIMITED {
                return true;
            }
            if current == 0 {
                return false;
            }
            match self.shared.remaining.compare_exchange_weak(
                current,
                current - 1,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => return true,
                Err(actual) => current = actual,
            }
        }
    }

    /// Mints a fresh [`RequirementId`].
    #[must_use]
    pub fn requirement_id(&self) -> RequirementId {
        RequirementId::new(self.next_uuid())
    }

    /// Mints a fresh [`RunId`].
    #[must_use]
    pub fn run_id(&self) -> RunId {
        RunId::new(self.next_uuid())
    }

    /// Mints a fresh [`AgentId`].
    #[must_use]
    pub fn agent_id(&self) -> AgentId {
        AgentId::new(self.next_uuid())
    }

    /// Mints a fresh [`ToolSetId`].
    #[must_use]
    pub fn tool_set_id(&self) -> ToolSetId {
        ToolSetId::new(self.next_uuid())
    }

    /// Mints a fresh [`ConversationId`].
    #[must_use]
    pub fn conversation_id(&self) -> ConversationId {
        ConversationId::new(self.next_uuid())
    }

    /// Mints a fresh [`TurnId`].
    #[must_use]
    pub fn turn_id(&self) -> TurnId {
        TurnId::new(self.next_uuid())
    }

    /// Mints a fresh [`MessageId`].
    #[must_use]
    pub fn message_id(&self) -> MessageId {
        MessageId::new(self.next_uuid())
    }

    /// Mints a fresh [`ToolCallId`].
    ///
    /// This inherent helper takes no arguments and is distinct from the
    /// [`ToolExecutionIds::tool_call_id`] contract method (which derives an id
    /// from a [`ToolCall`]); both mint a fresh, globally unique id.
    #[must_use]
    pub fn tool_call_id(&self) -> ToolCallId {
        ToolCallId::new(self.next_uuid())
    }

    /// Mints a fresh [`StepId`].
    #[must_use]
    pub fn step_id(&self) -> StepId {
        StepId::new(self.next_uuid())
    }

    /// Mints a fresh, readable [`TraceNodeId`] of the form
    /// `"<label>:<node>#<seq>"`.
    ///
    /// The trailing sequence number is drawn from the shared counter, so the id
    /// stays globally unique even when two nodes share the same `node` label.
    #[must_use]
    pub fn trace_node(&self, node: &str) -> TraceNodeId {
        let seq = self.shared.counter.fetch_add(1, Ordering::SeqCst);
        TraceNodeId::new(format!("{}:{node}#{seq}", self.label))
    }

    /// Returns the ordered log of requirement ids minted through the
    /// [`RequirementIds`] contract across this whole clone/fork tree.
    #[must_use]
    pub fn requirement_log(&self) -> Vec<RequirementAllocation> {
        self.shared
            .requirement_log
            .lock()
            .expect("requirement log mutex poisoned")
            .clone()
    }

    /// Returns, in allocation order, the requirement ids minted for `tag`.
    #[must_use]
    pub fn requirement_ids(&self, tag: RequirementKindTag) -> Vec<RequirementId> {
        self.shared
            .requirement_log
            .lock()
            .expect("requirement log mutex poisoned")
            .iter()
            .filter(|entry| entry.tag == tag)
            .map(|entry| entry.id)
            .collect()
    }
}

impl RequirementIds for SeqIds {
    fn next_requirement_id(
        &self,
        kind_tag: RequirementKindTag,
    ) -> Result<RequirementId, RequirementError> {
        if !self.take_budget() {
            return Err(RequirementError::IdUnavailable { kind: kind_tag });
        }
        let id = RequirementId::new(self.next_uuid());
        self.shared
            .requirement_log
            .lock()
            .expect("requirement log mutex poisoned")
            .push(RequirementAllocation { tag: kind_tag, id });
        Ok(id)
    }
}

impl ToolExecutionIds for SeqIds {
    fn tool_call_id(&self, call: &ToolCall) -> Result<ToolCallId, ToolRuntimeError> {
        if !self.take_budget() {
            return Err(ToolRuntimeError::IdUnavailable {
                purpose: format!("tool call `{}`", call.id),
            });
        }
        Ok(ToolCallId::new(self.next_uuid()))
    }

    fn tool_result_message_id(
        &self,
        _call_id: ToolCallId,
        call: &ToolCall,
    ) -> Result<MessageId, ToolRuntimeError> {
        if !self.take_budget() {
            return Err(ToolRuntimeError::IdUnavailable {
                purpose: format!("tool result for `{}`", call.id),
            });
        }
        Ok(MessageId::new(self.next_uuid()))
    }

    fn next_assistant_message_id(&self) -> Result<MessageId, ToolRuntimeError> {
        if !self.take_budget() {
            return Err(ToolRuntimeError::IdUnavailable {
                purpose: "assistant continuation message".to_owned(),
            });
        }
        Ok(MessageId::new(self.next_uuid()))
    }

    fn next_step_id(&self) -> Result<StepId, ToolRuntimeError> {
        if !self.take_budget() {
            return Err(ToolRuntimeError::IdUnavailable {
                purpose: "assistant continuation step".to_owned(),
            });
        }
        Ok(StepId::new(self.next_uuid()))
    }
}

#[cfg(test)]
mod tests {
    use super::{RequirementAllocation, SeqIds};
    use agent_lib::{
        agent::{
            RequirementError, RequirementIds, RequirementKindTag, ToolExecutionIds,
            ToolRuntimeError,
        },
        model::tool::ToolCall,
    };
    use serde_json::json;
    use std::collections::HashSet;

    fn sample_call() -> ToolCall {
        ToolCall {
            id: "provider-1".to_owned(),
            name: "weather".to_owned(),
            input: json!({}),
        }
    }

    #[test]
    fn clones_share_the_counter_and_never_repeat() {
        let ids = SeqIds::new();
        let clone = ids.clone();

        let mut seen = HashSet::new();
        for _ in 0..64 {
            assert!(seen.insert(ids.run_id().into_uuid()));
            assert!(seen.insert(clone.message_id().into_uuid()));
        }
        assert_eq!(seen.len(), 128);
    }

    #[test]
    fn forks_stay_unique_and_carry_nested_labels() {
        let root = SeqIds::new();
        let child = root.fork("child");
        let grandchild = child.fork("leaf");

        assert_eq!(child.label(), "root/child");
        assert_eq!(grandchild.label(), "root/child/leaf");

        let mut seen = HashSet::new();
        for source in [&root, &child, &grandchild] {
            for _ in 0..16 {
                assert!(seen.insert(source.requirement_id().into_uuid()));
                assert!(seen.insert(source.tool_call_id().into_uuid()));
            }
        }
        assert_eq!(seen.len(), 3 * 16 * 2);
    }

    #[test]
    fn named_relabels_without_colliding() {
        let root = SeqIds::new();
        let aliased = root.named("audit");

        assert_eq!(aliased.label(), "audit");
        assert_ne!(
            root.trace_node("step").to_string(),
            aliased.trace_node("step").to_string()
        );
        assert!(
            aliased
                .trace_node("step")
                .as_str()
                .starts_with("audit:step#")
        );
    }

    #[test]
    fn minted_ids_round_trip_through_agent_lib_parsers() {
        let ids = SeqIds::new();

        let run = ids.run_id();
        let reparsed = run.to_string().parse().expect("run id parses");
        assert_eq!(run, reparsed);

        let requirement = ids
            .next_requirement_id(RequirementKindTag::Llm)
            .expect("requirement id available");
        let reparsed = requirement
            .to_string()
            .parse()
            .expect("requirement id parses");
        assert_eq!(requirement, reparsed);
    }

    #[test]
    fn requirement_log_is_ordered_and_queryable_by_tag() {
        let ids = SeqIds::new();

        let llm = ids
            .next_requirement_id(RequirementKindTag::Llm)
            .expect("llm id");
        let tool = ids
            .next_requirement_id(RequirementKindTag::Tool)
            .expect("tool id");
        let llm_2 = ids
            .next_requirement_id(RequirementKindTag::Llm)
            .expect("second llm id");

        assert_eq!(
            ids.requirement_log(),
            vec![
                RequirementAllocation {
                    tag: RequirementKindTag::Llm,
                    id: llm,
                },
                RequirementAllocation {
                    tag: RequirementKindTag::Tool,
                    id: tool,
                },
                RequirementAllocation {
                    tag: RequirementKindTag::Llm,
                    id: llm_2,
                },
            ],
        );
        assert_eq!(
            ids.requirement_ids(RequirementKindTag::Llm),
            vec![llm, llm_2]
        );
        assert_eq!(ids.requirement_ids(RequirementKindTag::Tool), vec![tool]);
        assert!(
            ids.requirement_ids(RequirementKindTag::Interaction)
                .is_empty()
        );
    }

    #[test]
    fn exhausted_source_reports_requirement_id_unavailable() {
        let ids = SeqIds::exhausted();

        let error = ids
            .next_requirement_id(RequirementKindTag::Tool)
            .expect_err("exhausted source must fail");
        assert_eq!(
            error,
            RequirementError::IdUnavailable {
                kind: RequirementKindTag::Tool,
            }
        );
    }

    #[test]
    fn exhausted_source_reports_tool_id_unavailable() {
        let ids = SeqIds::exhausted();
        let call = sample_call();

        let error =
            ToolExecutionIds::tool_call_id(&ids, &call).expect_err("exhausted source must fail");
        assert!(matches!(error, ToolRuntimeError::IdUnavailable { .. }));

        assert!(matches!(
            ids.next_step_id(),
            Err(ToolRuntimeError::IdUnavailable { .. })
        ));
    }

    #[test]
    fn budget_is_shared_and_consumed_by_contract_methods() {
        let ids = SeqIds::with_budget(1);
        let clone = ids.clone();

        // The single unit is consumed here; the shared clone then sees none.
        assert!(ids.next_requirement_id(RequirementKindTag::Llm).is_ok());
        assert_eq!(
            clone.next_requirement_id(RequirementKindTag::Llm),
            Err(RequirementError::IdUnavailable {
                kind: RequirementKindTag::Llm,
            })
        );

        // Inherent helpers ignore the budget entirely.
        let _ = clone.run_id();
    }
}
