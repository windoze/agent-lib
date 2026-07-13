//! Sans-io Agent state machine contract.
//!
//! The effect model replaces the loop-drives-itself shape with an
//! externally-driven *pull*: a driver repeatedly calls
//! [`AgentMachine::step`], and the machine advances its own state and *requests*
//! IO instead of performing it. `step` is pure and synchronous — it never
//! `await`s and never touches a client, tool, or process. All awaiting happens
//! in the driver, on the [`Requirement`]s the machine hands back.
//!
//! # Backpressure
//!
//! `step` takes `&mut self`, which is the natural backpressure: once the machine
//! is blocked on a batch of requirements, it cannot advance until their results
//! are fed back in via [`StepInput::Resume`]. There is no hidden internal queue
//! that keeps running ahead of the driver.
//!
//! # What this module defines
//!
//! This is the Stage-1 type skeleton (migration doc §2): the [`AgentMachine`]
//! trait plus the [`StepInput`] / [`StepOutcome`] data boundaries. Concrete
//! `step` logic for the LLM and tool paths lands in later milestones (M2-3 /
//! M2-4), and elevating the machine's own serializable state onto
//! [`LoopCursor`] lands in M2-2.
//!
//! # Persistence boundary
//!
//! [`StepOutcome`] is a persistable description: its [`Notification`]s and
//! [`Requirement`]s all serialize. [`StepInput`] is *not* persistable, because
//! [`StepInput::Resume`] carries a runtime [`RequirementResolution`] (live
//! values and runtime errors). The serializable inputs are the [`AgentInput`]
//! inside [`StepInput::External`] and the [`RequirementId`] inside
//! [`StepInput::Abandon`].

use crate::agent::{
    AgentInput, LoopCursor, Notification, Requirement, RequirementId, RequirementResolution,
};
use serde::{Deserialize, Serialize};

/// A pure Agent state machine: it advances state and requests IO without doing
/// IO, and without `async`.
///
/// Implementations are object-safe so a driver can hold a
/// `Box<dyn AgentMachine>`.
pub trait AgentMachine {
    /// Advances the machine by one step.
    ///
    /// Pure-function semantics: this never `await`s and never touches a client,
    /// tool, or process. `input` is either a fresh external input (a user turn
    /// or pivot) or the fulfilled result of an outstanding requirement. The
    /// returned [`StepOutcome`] carries the notifications produced this step and
    /// any newly blocked requirements (possibly none).
    fn step(&mut self, input: StepInput) -> StepOutcome;

    /// Returns a read-only view of the machine's current cursor state.
    ///
    /// This is the effect-model equivalent of inspecting the loop cursor.
    fn cursor(&self) -> &LoopCursor;
}

/// Input to one [`AgentMachine::step`]: an external input or a requirement's
/// fulfillment on the return path.
///
/// Not persistable as a whole — see the [module docs](self#persistence-boundary).
#[derive(Clone, Debug)]
pub enum StepInput {
    /// A fresh external input that opens a new turn or soft-turns the machine.
    External(AgentInput),
    /// The fulfilled result of a previously emitted requirement (return path).
    Resume(RequirementResolution),
    /// A decision to discard a previously emitted requirement (never-resume).
    Abandon(RequirementId),
}

impl StepInput {
    /// Creates a step input from a fresh external input.
    #[must_use]
    pub const fn external(input: AgentInput) -> Self {
        Self::External(input)
    }

    /// Creates a step input that feeds a requirement's fulfilled result back.
    #[must_use]
    pub const fn resume(resolution: RequirementResolution) -> Self {
        Self::Resume(resolution)
    }

    /// Creates a step input that abandons an outstanding requirement.
    #[must_use]
    pub const fn abandon(id: RequirementId) -> Self {
        Self::Abandon(id)
    }
}

/// Product of one [`AgentMachine::step`]: a notification slice, the requirements
/// newly blocked this step, and whether the machine came to rest.
///
/// A single `step` advances synchronously from the current state to the next
/// blocking point or to quiescence, and may hand back a *batch* of requirements
/// at once (migration decision B). This is a persistable description: every
/// field serializes.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StepOutcome {
    /// Notifications produced this step. Safe to skip; a driver only forwards
    /// them.
    #[serde(default)]
    pub notifications: Vec<Notification>,
    /// Requirements newly blocked this step, awaiting external fulfillment.
    /// Possibly empty.
    #[serde(default)]
    pub requirements: Vec<Requirement>,
    /// Whether the machine is at rest after this step (every branch has either
    /// produced output or is blocked on a requirement).
    #[serde(default)]
    pub quiescent: bool,
}

impl StepOutcome {
    /// Creates a step outcome from its parts.
    #[must_use]
    pub const fn new(
        notifications: Vec<Notification>,
        requirements: Vec<Requirement>,
        quiescent: bool,
    ) -> Self {
        Self {
            notifications,
            requirements,
            quiescent,
        }
    }

    /// Returns whether the machine came to rest after this step.
    #[must_use]
    pub const fn is_quiescent(&self) -> bool {
        self.quiescent
    }

    /// Returns whether this step blocked on at least one new requirement.
    #[must_use]
    pub const fn has_requirements(&self) -> bool {
        !self.requirements.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::{AgentMachine, StepInput, StepOutcome};
    use crate::agent::{
        AgentInput, LoopCursor, LoopCursorKind, LoopDoneReason, Notification, Requirement,
        RequirementId, RequirementKind, RequirementResolution, RequirementResult, StepId,
        ToolCallFinished, ToolCallStarted,
    };
    use crate::conversation::{MessageId, ToolCallId, TurnId};
    use crate::model::{
        content::ContentBlock,
        message::{Message, Role},
        tool::{ToolCall, ToolResponse, ToolStatus},
    };
    use serde_json::{Map, json};

    fn step_id() -> StepId {
        "018f0d9c-7b6a-7c12-8f31-1234567890e9"
            .parse()
            .expect("step id")
    }

    fn turn_id() -> TurnId {
        "018f0d9c-7b6a-7c12-8f31-1234567890f2"
            .parse()
            .expect("turn id")
    }

    fn message_id() -> MessageId {
        "018f0d9c-7b6a-7c12-8f31-1234567890f3"
            .parse()
            .expect("message id")
    }

    fn assistant_message_id() -> MessageId {
        "018f0d9c-7b6a-7c12-8f31-1234567890f6"
            .parse()
            .expect("assistant message id")
    }

    fn tool_call_id() -> ToolCallId {
        "018f0d9c-7b6a-7c12-8f31-1234567890c1"
            .parse()
            .expect("tool call id")
    }

    fn requirement_id() -> RequirementId {
        RequirementId::parse_str("018f0d9c-7b6a-7c12-8f31-1234567890a1").expect("requirement id")
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

    fn tool_call() -> ToolCall {
        ToolCall {
            id: "call-weather".to_owned(),
            name: "get_weather".to_owned(),
            input: json!({ "city": "Shanghai" }),
        }
    }

    fn tool_response() -> ToolResponse {
        ToolResponse {
            tool_call_id: "call-weather".to_owned(),
            content: vec![ContentBlock::Text {
                text: "Sunny".to_owned(),
                extra: Map::new(),
            }],
            status: ToolStatus::Ok,
            extra: Map::new(),
        }
    }

    fn need_tool_requirement() -> Requirement {
        Requirement::at_root(
            requirement_id(),
            RequirementKind::NeedTool {
                call_id: tool_call_id(),
                call: tool_call(),
            },
        )
    }

    fn started_notification() -> Notification {
        Notification::ToolCallStarted(ToolCallStarted::new(
            step_id(),
            tool_call_id(),
            tool_call(),
            None,
        ))
    }

    fn finished_notification() -> Notification {
        Notification::ToolCallFinished(ToolCallFinished::new(
            step_id(),
            tool_call_id(),
            tool_response(),
            None,
        ))
    }

    /// A minimal machine used only to exercise the trait contract: it records
    /// how it was driven and moves a cursor. It performs no IO and holds no
    /// serialized state.
    #[derive(Default)]
    struct FakeMachine {
        cursor: LoopCursor,
        abandons: usize,
    }

    impl AgentMachine for FakeMachine {
        fn step(&mut self, input: StepInput) -> StepOutcome {
            match input {
                StepInput::External(_) => {
                    self.cursor = LoopCursor::streaming_step(step_id(), None);
                    StepOutcome::new(
                        vec![started_notification()],
                        vec![need_tool_requirement()],
                        true,
                    )
                }
                StepInput::Resume(_) => {
                    self.cursor = LoopCursor::done(LoopDoneReason::Completed);
                    StepOutcome::new(vec![finished_notification()], Vec::new(), true)
                }
                StepInput::Abandon(_) => {
                    self.abandons += 1;
                    StepOutcome::default()
                }
            }
        }

        fn cursor(&self) -> &LoopCursor {
            &self.cursor
        }
    }

    fn user_input() -> AgentInput {
        AgentInput::user_message(
            turn_id(),
            message_id(),
            user_message("hello"),
            assistant_message_id(),
            step_id(),
        )
        .expect("valid user input")
    }

    #[test]
    fn fake_machine_is_object_safe_and_drives_through_a_batch() {
        let mut machine: Box<dyn AgentMachine> = Box::new(FakeMachine::default());
        assert_eq!(machine.cursor().kind(), LoopCursorKind::Idle);

        let outcome = machine.step(StepInput::external(user_input()));
        assert!(outcome.is_quiescent());
        assert!(outcome.has_requirements());
        assert_eq!(outcome.requirements.len(), 1);
        assert_eq!(outcome.notifications.len(), 1);
        assert_eq!(machine.cursor().kind(), LoopCursorKind::StreamingStep);

        let requirement = &outcome.requirements[0];
        let resolution = RequirementResolution::new(
            requirement.id,
            RequirementResult::Tool(Ok(tool_response())),
        );
        requirement
            .accepts_resolution(&resolution)
            .expect("tool result aligns with the emitted requirement");

        let resumed = machine.step(StepInput::resume(resolution));
        assert!(resumed.is_quiescent());
        assert!(!resumed.has_requirements());
        assert_eq!(machine.cursor().kind(), LoopCursorKind::Done);
    }

    #[test]
    fn abandon_input_reaches_the_machine() {
        let mut machine = FakeMachine::default();
        let outcome = machine.step(StepInput::abandon(requirement_id()));
        assert_eq!(outcome, StepOutcome::default());
        assert_eq!(machine.abandons, 1);
    }

    #[test]
    fn step_outcome_round_trips_notifications_and_requirements() {
        let outcome = StepOutcome::new(
            vec![started_notification()],
            vec![need_tool_requirement()],
            true,
        );
        let encoded = serde_json::to_value(&outcome).expect("serialize step outcome");
        let decoded: StepOutcome =
            serde_json::from_value(encoded).expect("deserialize step outcome");
        assert_eq!(decoded, outcome);
    }

    #[test]
    fn empty_step_outcome_round_trips() {
        let outcome = StepOutcome::default();
        assert!(!outcome.is_quiescent());
        assert!(!outcome.has_requirements());
        let encoded = serde_json::to_value(&outcome).expect("serialize empty outcome");
        let decoded: StepOutcome =
            serde_json::from_value(encoded).expect("deserialize empty outcome");
        assert_eq!(decoded, outcome);
    }

    #[test]
    fn step_input_variants_construct_and_clone() {
        let external = StepInput::external(user_input());
        assert!(matches!(external.clone(), StepInput::External(_)));

        let resume = StepInput::resume(RequirementResolution::new(
            requirement_id(),
            RequirementResult::Tool(Ok(tool_response())),
        ));
        assert!(matches!(resume.clone(), StepInput::Resume(_)));

        let abandon = StepInput::abandon(requirement_id());
        assert!(matches!(abandon.clone(), StepInput::Abandon(_)));

        // Debug is available for driver-side tracing of the return path.
        assert!(format!("{abandon:?}").contains("Abandon"));
    }
}
