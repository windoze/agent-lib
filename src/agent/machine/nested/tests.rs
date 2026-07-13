use super::{MachineTreeState, NestedMachine};
use crate::{
    agent::{
        AgentInput, AgentMachine, AgentPath, AgentSlot, AgentSpec, AgentState, DefaultAgentMachine,
        LlmStepMode, LoopCursor, LoopCursorKind, LoopPolicy, ModelRef, RequirementError,
        RequirementId, RequirementIds, RequirementKind, RequirementKindTag, RequirementResolution,
        RequirementResult, StepId, StepInput, ToolFailurePolicy, ToolSetRef, WorktreeRef,
    },
    client::Response,
    conversation::{Conversation, ConversationConfig, ConversationId, MessageId, TurnId},
    model::{
        content::ContentBlock,
        message::{Message, Role},
        normalized::StopReason,
        usage::Usage,
    },
};
use serde_json::Map;
use std::{num::NonZeroU32, sync::Arc};

/// A requirement-id source that always hands back one fixed id, so a node's
/// first reified requirement has a known, distinct identity per machine.
#[derive(Debug)]
struct FixedRequirementIds(RequirementId);

impl RequirementIds for FixedRequirementIds {
    fn next_requirement_id(
        &self,
        _kind_tag: RequirementKindTag,
    ) -> Result<RequirementId, RequirementError> {
        Ok(self.0)
    }
}

fn nz(value: u32) -> NonZeroU32 {
    NonZeroU32::new(value).expect("non-zero test value")
}

fn uuid(suffix: &str) -> String {
    format!("018f0d9c-7b6a-7c12-8f31-1234567890{suffix}")
}

fn parent_requirement_id() -> RequirementId {
    RequirementId::parse_str(&uuid("a1")).expect("parent requirement id")
}

fn child_requirement_id() -> RequirementId {
    RequirementId::parse_str(&uuid("b1")).expect("child requirement id")
}

fn spec(agent_suffix: &str, tool_set_suffix: &str) -> AgentSpec {
    AgentSpec::new(
        uuid(agent_suffix).parse().expect("agent id"),
        WorktreeRef::new("/repo/agent-lib"),
        Some("Spec fallback system.".to_owned()),
        ToolSetRef::new(
            uuid(tool_set_suffix).parse().expect("tool set id"),
            Vec::new(),
        ),
        ModelRef::new("gpt-5.5", nz(512), Some(0.1), None),
        LoopPolicy::new(nz(8), nz(1), ToolFailurePolicy::ReturnErrorToModel),
    )
}

fn state(agent_suffix: &str, tool_set_suffix: &str, conversation_suffix: &str) -> AgentState {
    let conversation_id: ConversationId =
        uuid(conversation_suffix).parse().expect("conversation id");
    AgentState::new(
        spec(agent_suffix, tool_set_suffix),
        Conversation::new(
            conversation_id,
            ConversationConfig::new(Some("Conversation system.".to_owned())),
        ),
    )
}

fn node_machine(state: AgentState, requirement_id: RequirementId) -> DefaultAgentMachine {
    DefaultAgentMachine::new(
        state,
        LlmStepMode::NonStreaming,
        Arc::new(FixedRequirementIds(requirement_id)),
    )
}

fn parent_machine() -> DefaultAgentMachine {
    node_machine(state("01", "02", "04"), parent_requirement_id())
}

fn child_machine() -> DefaultAgentMachine {
    node_machine(state("11", "12", "14"), child_requirement_id())
}

fn user_message(text: &str) -> Message {
    Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: text.to_owned(),
            extra: Map::new(),
        }],
    }
}

fn user_input(turn: &str, message: &str, assistant: &str, step: &str, text: &str) -> AgentInput {
    AgentInput::user_message(
        uuid(turn).parse::<TurnId>().expect("turn id"),
        uuid(message).parse::<MessageId>().expect("message id"),
        user_message(text),
        uuid(assistant).parse::<MessageId>().expect("assistant id"),
        uuid(step).parse::<StepId>().expect("step id"),
    )
    .expect("valid user input")
}

fn parent_feed() -> AgentInput {
    user_input("05", "06", "07", "08", "parent")
}

fn child_brief() -> AgentInput {
    user_input("15", "16", "17", "18", "child")
}

fn text_response(text: &str) -> Response {
    Response {
        message: Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Text {
                text: text.to_owned(),
                extra: Map::new(),
            }],
        },
        usage: Usage::default(),
        stop_reason: StopReason::normalize("end_turn"),
        extra: Map::new(),
    }
}

const CHILD_SLOT: AgentSlot = AgentSlot::new(1);

/// Reads the real [`AgentPath`] origin recorded in a streaming-step cursor's
/// requirement binding, panicking on any other cursor shape.
fn cursor_origin(cursor: &LoopCursor) -> &AgentPath {
    match cursor {
        LoopCursor::StreamingStep(step) => step.requirement().expect("bound requirement").origin(),
        other => panic!("expected a streaming-step cursor, found {other:?}"),
    }
}

/// Builds a parent + one child two-level tree, child pending its opening brief.
fn opened_tree() -> NestedMachine {
    let mut tree = NestedMachine::new(parent_machine());
    tree.attach_child(
        CHILD_SLOT,
        NestedMachine::new(child_machine()),
        child_brief(),
    )
    .expect("attach child");
    tree
}

#[test]
fn step_aggregates_parent_and_child_requirements_with_real_paths() {
    let mut tree = opened_tree();

    let outcome = tree.step(StepInput::external(parent_feed()));

    assert!(outcome.is_quiescent());
    assert_eq!(outcome.requirements.len(), 2);

    let parent = outcome
        .requirements
        .iter()
        .find(|requirement| requirement.id == parent_requirement_id())
        .expect("parent requirement present");
    assert!(parent.origin.is_root(), "parent origin is the root path");
    assert!(matches!(parent.kind, RequirementKind::NeedLlm { .. }));

    let child = outcome
        .requirements
        .iter()
        .find(|requirement| requirement.id == child_requirement_id())
        .expect("child requirement present");
    assert_eq!(
        child.origin.slots(),
        &[CHILD_SLOT][..],
        "child origin is [slot]"
    );
    assert!(matches!(child.kind, RequirementKind::NeedLlm { .. }));

    // The same addressing is exposed as absolute (id, path) pairs.
    let outstanding = tree.outstanding_requirements();
    assert!(outstanding.contains(&(parent_requirement_id(), AgentPath::root())));
    assert!(outstanding.contains(&(child_requirement_id(), AgentPath::root().child(CHILD_SLOT))));

    // Both nodes are parked on their own LLM step, and each node's persisted
    // cursor binding records its real path (bullet 4): root for the parent,
    // `[slot]` for the child.
    assert_eq!(tree.cursor().kind(), LoopCursorKind::StreamingStep);
    assert!(cursor_origin(tree.cursor()).is_root());
    let child_node = tree.child(CHILD_SLOT).expect("child");
    assert_eq!(child_node.cursor().kind(), LoopCursorKind::StreamingStep);
    assert_eq!(
        cursor_origin(child_node.cursor()).slots(),
        &[CHILD_SLOT][..]
    );
}

#[test]
fn resume_routes_by_id_to_the_child_only() {
    let mut tree = opened_tree();
    tree.step(StepInput::external(parent_feed()));

    // A resolution addressed to the child's requirement id reaches only the
    // child machine; the parent stays parked on its own LLM step.
    let resolution = RequirementResolution::new(
        child_requirement_id(),
        RequirementResult::Llm(Ok(text_response("child done"))),
    );
    let outcome = tree.step(StepInput::resume(resolution));
    assert!(outcome.is_quiescent());
    assert!(outcome.requirements.is_empty());

    let child = tree.child(CHILD_SLOT).expect("child");
    assert_eq!(child.cursor().kind(), LoopCursorKind::Done);
    assert_eq!(child.own().state().conversation().turns().len(), 1);

    // The parent is untouched: still parked on its own requirement at the root.
    assert_eq!(tree.cursor().kind(), LoopCursorKind::StreamingStep);
    assert_eq!(
        tree.cursor().pending_requirement_ids(),
        vec![parent_requirement_id()]
    );
    assert!(cursor_origin(tree.cursor()).is_root());
    assert!(tree.own().state().conversation().pending().is_some());

    // Only the parent's requirement remains outstanding across the tree.
    assert_eq!(
        tree.outstanding_requirements(),
        vec![(parent_requirement_id(), AgentPath::root())]
    );
}

#[test]
fn whole_tree_round_trips_and_each_cursor_restores_independently() {
    // A tree is serializable only at a committed boundary: a node parked
    // mid-requirement holds a pending conversation turn, which the Conversation
    // core refuses to snapshot. So drive the parent to a committed `Done` and
    // leave the child unopened at `Idle` (still owing its opening brief) — two
    // distinct, independently restorable cursors at a valid snapshot boundary.
    let mut tree = NestedMachine::new(parent_machine());
    tree.step(StepInput::external(parent_feed()));
    tree.step(StepInput::resume(RequirementResolution::new(
        parent_requirement_id(),
        RequirementResult::Llm(Ok(text_response("parent done"))),
    )));
    tree.attach_child(
        CHILD_SLOT,
        NestedMachine::new(child_machine()),
        child_brief(),
    )
    .expect("attach child");

    assert_eq!(tree.cursor().kind(), LoopCursorKind::Done);
    assert_eq!(
        tree.child(CHILD_SLOT).expect("child").cursor().kind(),
        LoopCursorKind::Idle
    );

    let encoded = serde_json::to_string(&tree).expect("serialize tree");
    let snapshot: MachineTreeState = serde_json::from_str(&encoded).expect("deserialize snapshot");

    // The snapshot preserves both nodes' cursors independently.
    assert_eq!(snapshot.node().loop_cursor().kind(), LoopCursorKind::Done);
    assert_eq!(
        snapshot
            .child(CHILD_SLOT)
            .expect("child snapshot")
            .node()
            .loop_cursor()
            .kind(),
        LoopCursorKind::Idle
    );

    // Rebuilding the live tree re-injects handles per node and restores each
    // cursor independently.
    let restored = NestedMachine::from_state(snapshot, &|state| {
        node_machine(state, parent_requirement_id())
    });
    assert_eq!(restored.cursor().kind(), LoopCursorKind::Done);
    let child = restored.child(CHILD_SLOT).expect("child");
    assert_eq!(child.cursor().kind(), LoopCursorKind::Idle);
    // The parent's finished turn survived, and the child still owes its brief.
    assert_eq!(restored.own().state().conversation().turns().len(), 1);
    assert!(child.own().state().conversation().turns().is_empty());
    assert!(restored.outstanding_requirements().is_empty());

    // The restored child opens on the next feed, proving its pending brief and
    // real path survived the round-trip. (The test rebuild uses one id source
    // for every node, so assert on the re-stamped path, not the id.)
    let mut restored = restored;
    let outcome = restored.step(StepInput::external(parent_feed()));
    assert!(
        outcome
            .requirements
            .iter()
            .any(|requirement| requirement.origin.slots() == [CHILD_SLOT])
    );
}

#[test]
fn attach_child_rejects_an_occupied_slot() {
    let mut tree = NestedMachine::new(parent_machine());
    tree.attach_child(
        CHILD_SLOT,
        NestedMachine::new(child_machine()),
        child_brief(),
    )
    .expect("first attach");
    let error = tree
        .attach_child(
            CHILD_SLOT,
            NestedMachine::new(child_machine()),
            child_brief(),
        )
        .expect_err("duplicate slot rejected");
    assert_eq!(
        error,
        super::NestedMachineError::SlotOccupied { slot: CHILD_SLOT }
    );
}
