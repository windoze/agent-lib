use super::{
    BudgetDimension, BudgetError, BudgetLimits, BudgetSnapshot, BudgetUsage,
    RequirementDisposition, RunContext, RunContextError, TraceError, TraceNodeId, TraceNodeKind,
    TraceRecord,
};
use crate::{
    agent::{
        RequirementKindTag,
        id::{RunId, StepId},
    },
    model::usage::Usage,
};
use serde_json::{Map, json};
use std::time::Duration;

fn run_id(suffix: &str) -> RunId {
    format!("018f0d9c-7b6a-7c12-8f31-1234567890{suffix}")
        .parse()
        .expect("valid run id")
}

fn step_id() -> StepId {
    "018f0d9c-7b6a-7c12-8f31-1234567890d1"
        .parse()
        .expect("valid step id")
}

fn node_id(value: &str) -> TraceNodeId {
    TraceNodeId::new(value)
}

fn context_with_limits(limits: BudgetLimits) -> RunContext {
    RunContext::new_root(run_id("c1"), limits, node_id("root"))
}

#[test]
fn cancellation_propagates_from_parent_to_child() {
    let parent = context_with_limits(BudgetLimits::unbounded());
    let child = parent
        .derive_child(run_id("c2"), node_id("sub-agent"))
        .expect("derive child");

    assert!(!parent.is_cancelled());
    assert!(!child.is_cancelled());

    parent.cancellation().cancel();

    assert!(parent.is_cancelled());
    assert!(child.is_cancelled());
    assert_eq!(child.check_cancelled(), Err(RunContextError::Cancelled));
}

#[test]
fn child_cancellation_does_not_cancel_parent() {
    let parent = context_with_limits(BudgetLimits::unbounded());
    let child = parent
        .derive_child(run_id("c2"), node_id("sub-agent"))
        .expect("derive child");

    child.cancellation().cancel();

    assert!(!parent.is_cancelled());
    assert!(child.is_cancelled());
}

#[test]
fn depth_starts_at_zero_and_increments_per_derived_child() {
    let root = context_with_limits(BudgetLimits::unbounded());
    assert_eq!(root.depth(), 0);

    let child = root
        .derive_child(run_id("c2"), node_id("sub-agent"))
        .expect("derive child");
    assert_eq!(child.depth(), 1);

    let grandchild = child
        .derive_child(run_id("c3"), node_id("sub-sub-agent"))
        .expect("derive grandchild");
    assert_eq!(grandchild.depth(), 2);
    assert_eq!(root.depth(), 0);
}

#[test]
fn budget_charges_steps_tokens_cost_and_preserves_state_on_exceed() {
    let context = context_with_limits(BudgetLimits::new(
        Some(2),
        Some(10),
        Some(50),
        Some(Duration::from_secs(5)),
    ));

    assert_eq!(context.charge_step().expect("first step").used().steps(), 1);
    assert_eq!(
        context.charge_step().expect("second step").used().steps(),
        2
    );

    let error = context.charge_step().expect_err("third step exceeds limit");
    assert_eq!(
        error,
        RunContextError::Budget(BudgetError::Exceeded {
            dimension: BudgetDimension::Steps,
            limit: 2,
            attempted: 3,
            remaining: 0,
        })
    );
    assert_eq!(context.budget().snapshot().used().steps(), 2);

    assert_eq!(
        context
            .charge_tokens(6)
            .expect("initial token charge")
            .used()
            .tokens(),
        6
    );
    let usage = Usage {
        input: 2,
        output: 1,
        cache_read: 1,
        cache_write: 0,
        reasoning: 0,
        total: None,
        extra: Map::new(),
    };
    assert_eq!(
        context
            .charge_usage(&usage)
            .expect("usage token charge")
            .used()
            .tokens(),
        10
    );
    assert!(matches!(
        context.charge_tokens(1),
        Err(RunContextError::Budget(BudgetError::Exceeded {
            dimension: BudgetDimension::Tokens,
            limit: 10,
            attempted: 11,
            remaining: 0,
        }))
    ));

    assert_eq!(
        context
            .charge_cost_micros(45)
            .expect("initial cost charge")
            .used()
            .cost_micros(),
        45
    );
    assert!(matches!(
        context.charge_cost_micros(6),
        Err(RunContextError::Budget(BudgetError::Exceeded {
            dimension: BudgetDimension::CostMicros,
            limit: 50,
            attempted: 51,
            remaining: 5,
        }))
    ));

    context
        .check_wall_clock(Duration::from_secs(5))
        .expect("equal to limit is allowed");
    assert_eq!(
        context.check_wall_clock(Duration::from_secs(6)),
        Err(RunContextError::Budget(BudgetError::WallClockExceeded {
            limit: Duration::from_secs(5),
            elapsed: Duration::from_secs(6),
        }))
    );
}

#[test]
fn child_context_shares_parent_budget_limits_and_usage() {
    let parent = context_with_limits(BudgetLimits::new(Some(4), Some(5), None, None));
    let child = parent
        .derive_child(run_id("c2"), node_id("sub-agent"))
        .expect("derive child");

    child.charge_tokens(4).expect("child charge");

    assert_eq!(parent.budget().snapshot().used().tokens(), 4);
    assert!(matches!(
        parent.charge_tokens(2),
        Err(RunContextError::Budget(BudgetError::Exceeded {
            dimension: BudgetDimension::Tokens,
            limit: 5,
            attempted: 6,
            remaining: 1,
        }))
    ));
}

#[test]
fn trace_records_parent_chain_for_run_step_llm_tool_and_sub_agent() {
    let context = context_with_limits(BudgetLimits::unbounded());
    let step = context
        .trace()
        .record_step(node_id("step-1"), step_id())
        .expect("record step");
    let step_trace = context
        .trace()
        .with_parent(step.id().clone())
        .expect("step trace handle");

    let llm = step_trace
        .record_llm(node_id("llm-1"), "primary-model")
        .expect("record llm");
    let tool = step_trace
        .record_tool(node_id("tool-1"), "get_weather")
        .expect("record tool");
    let child = context
        .derive_child(run_id("c2"), node_id("sub-agent"))
        .expect("derive child");
    let child_tool = child
        .trace()
        .record_tool(node_id("child-tool-1"), "read_file")
        .expect("record child tool");

    assert_eq!(step.parent(), Some(context.trace().current_parent()));
    assert_eq!(llm.parent(), Some(step.id()));
    assert_eq!(tool.parent(), Some(step.id()));
    assert_eq!(child.trace().current_parent().as_str(), "sub-agent");
    assert_eq!(child_tool.parent(), Some(child.trace().current_parent()));

    let records = context.trace().records();
    assert_eq!(records.len(), 6);
    assert_eq!(records[0].kind(), TraceNodeKind::Run);
    assert_eq!(records[0].parent(), None);
    assert_eq!(records[0].label(), Some(run_id("c1").to_string().as_str()));
}

#[test]
fn trace_rejects_duplicate_node_ids_and_unknown_parents() {
    let context = context_with_limits(BudgetLimits::unbounded());
    context
        .trace()
        .record_llm(node_id("llm-1"), "primary-model")
        .expect("record llm");

    assert_eq!(
        context
            .trace()
            .record_tool(node_id("llm-1"), "same id")
            .expect_err("duplicate trace node"),
        TraceError::DuplicateNodeId {
            node_id: node_id("llm-1"),
        }
    );
    assert_eq!(
        context
            .trace()
            .with_parent(node_id("missing"))
            .expect_err("unknown parent"),
        TraceError::UnknownParent {
            parent: node_id("missing"),
        }
    );
}

#[test]
fn trace_index_tracks_many_node_ids_without_losing_records() {
    let context = context_with_limits(BudgetLimits::unbounded());
    for index in 0..1_000 {
        let id = format!("step-{index}");
        context
            .trace()
            .record_step(node_id(&id), step_id())
            .expect("record indexed trace node");
    }

    assert_eq!(context.trace().records().len(), 1_001);
    assert_eq!(
        context
            .trace()
            .record_tool(node_id("step-999"), "duplicate")
            .expect_err("duplicate remains indexed"),
        TraceError::DuplicateNodeId {
            node_id: node_id("step-999"),
        }
    );
}

#[test]
fn budget_and_trace_records_are_serializable_data() {
    let snapshot = BudgetSnapshot::from_parts(
        BudgetLimits::new(Some(2), Some(100), Some(500), Some(Duration::from_secs(30))),
        BudgetUsage::new(1, 25, 75),
    );
    let encoded = serde_json::to_value(snapshot).expect("serialize budget snapshot");
    assert_eq!(encoded["limits"]["max_steps"], json!(2));
    assert_eq!(encoded["used"]["tokens"], json!(25));

    let decoded: BudgetSnapshot =
        serde_json::from_value(encoded).expect("deserialize budget snapshot");
    assert_eq!(decoded, snapshot);

    let record = TraceRecord::new(
        node_id("llm-1"),
        Some(node_id("step-1")),
        TraceNodeKind::Llm,
        Some("primary-model".to_owned()),
    );
    let encoded = serde_json::to_value(&record).expect("serialize trace record");
    assert_eq!(encoded["id"], json!("llm-1"));
    assert_eq!(encoded["parent"], json!("step-1"));
    assert_eq!(encoded["kind"], json!("llm"));
    assert_eq!(encoded["label"], json!("primary-model"));

    let decoded: TraceRecord = serde_json::from_value(encoded).expect("deserialize trace record");
    assert_eq!(decoded, record);
}

#[test]
fn requirement_trace_node_round_trips_through_serde() {
    let record = TraceRecord::new(
        node_id("req-1"),
        Some(node_id("root")),
        TraceNodeKind::Requirement {
            kind_tag: RequirementKindTag::Interaction,
            resolved_at_scope: 1,
            disposition: RequirementDisposition::NeverResumed,
        },
        None,
    );

    let encoded = serde_json::to_value(&record).expect("serialize requirement trace record");
    assert_eq!(encoded["id"], json!("req-1"));
    assert_eq!(encoded["parent"], json!("root"));
    assert_eq!(
        encoded["kind"]["requirement"],
        json!({
            "kind_tag": "interaction",
            "resolved_at_scope": 1,
            "disposition": "never_resumed",
        })
    );

    let decoded: TraceRecord =
        serde_json::from_value(encoded).expect("deserialize requirement trace record");
    assert_eq!(decoded, record);
}

#[test]
fn external_shutdown_trace_node_records_disposition_under_parent() {
    use crate::agent::external::ExternalSessionShutdown;

    let context = context_with_limits(BudgetLimits::unbounded());
    let shutdown = context
        .trace()
        .record_external_shutdown(node_id("shutdown-1"), ExternalSessionShutdown::ForcedKill)
        .expect("record external shutdown");

    assert_eq!(shutdown.parent(), Some(context.trace().current_parent()));
    assert_eq!(
        shutdown.kind(),
        TraceNodeKind::ExternalShutdown {
            disposition: ExternalSessionShutdown::ForcedKill,
        }
    );
    // The forced-kill disposition is surfaced as the node label for diagnostics.
    assert_eq!(shutdown.label(), Some("forced_kill"));

    let encoded = serde_json::to_value(&shutdown).expect("serialize external shutdown record");
    assert_eq!(
        encoded["kind"]["external_shutdown"],
        json!({ "disposition": "forced_kill" })
    );

    let decoded: TraceRecord =
        serde_json::from_value(encoded).expect("deserialize external shutdown record");
    assert_eq!(decoded, shutdown);
}
