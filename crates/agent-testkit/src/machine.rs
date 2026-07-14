//! The [`ScriptMachine`] [`AgentMachine`] double for driving scripted step
//! sequences.
//!
//! `agent-lib`'s own tests grow an ad hoc "batch machine" per file
//! (`drive.rs`, `subagent/tests.rs`, `agent_effect_e2e.rs`): each emits a fixed
//! [`Requirement`] batch on its opening turn, routes fulfilled results back by
//! id, and completes once every requirement is resumed. [`ScriptMachine`]
//! collapses those doubles into one configurable machine so a test can exercise
//! the [`drain`](agent_lib::agent::drain) driver, [`Pop`](agent_lib::agent::Pop)
//! routing, and subagent mechanics *without* depending on
//! [`DefaultAgentMachine`](agent_lib::agent::DefaultAgentMachine)'s internal
//! LLM/tool folding.
//!
//! # What it does
//!
//! - On a [`StepInput::External`] input it emits its fixed batch and rests on a
//!   non-terminal *waiting* cursor that [`drain`](agent_lib::agent::drain)
//!   recognises (a [`LoopCursor::StreamingStep`] by default).
//! - On a [`StepInput::Resume`] it records the resume order and result family in
//!   a shared [`ScriptMachineLog`], routing purely by requirement id so an
//!   out-of-order batch resume settles cleanly. Once every outstanding
//!   requirement is resumed it moves to [`LoopCursor::Done`] — provided the
//!   builder opted into [`done_after_all_resumed`](ScriptMachineBuilder::done_after_all_resumed).
//! - A [`StepInput::Resume`] carrying an id the machine never emitted moves it to
//!   a diagnostic [`LoopCursor::Error`] cursor instead of silently dropping the
//!   stray result.
//! - On a [`StepInput::Abandon`] it counts the abandonment and, when configured,
//!   settles onto a caller-chosen cursor ([`idle_on_abandon`](ScriptMachineBuilder::idle_on_abandon)
//!   or [`abandon_cursor`](ScriptMachineBuilder::abandon_cursor)).
//!
//! # Observability
//!
//! The resume order, resume result tags, and abandon count live behind an
//! [`Arc<ScriptMachineLog>`](ScriptMachineLog) so a test can clone the log
//! *before* handing the machine off to a driver (or nesting it as a child
//! machine) and still read what happened after the drive completes.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use agent_lib::agent::{
    AgentMachine, LoopCursor, LoopDoneReason, Requirement, RequirementId, RequirementKindTag,
    StepId, StepInput, StepOutcome,
};

/// Stable step id backing the default waiting cursor.
///
/// The concrete value is irrelevant to a driver — it only needs a syntactically
/// valid [`StepId`] to name the streaming step the machine parks on — so a fixed
/// literal keeps the default deterministic without pulling in an id source.
const DEFAULT_WAITING_STEP_ID: &str = "018f0d9c-7b6a-7c12-8f31-5c817e000001";

/// Builds the default non-terminal waiting cursor a [`ScriptMachine`] rests on.
fn default_waiting_cursor() -> LoopCursor {
    let step_id: StepId = DEFAULT_WAITING_STEP_ID
        .parse()
        .expect("DEFAULT_WAITING_STEP_ID is a valid step id");
    LoopCursor::streaming_step(step_id, None)
}

/// Shared, post-drive-observable record of how a [`ScriptMachine`] was driven.
///
/// A test clones the [`Arc`] before the machine is moved into a driver, then
/// reads the resume order, resume result families, and abandon count back once
/// the drive settles. Every field uses interior mutability so the machine can
/// record through a shared handle from its `&mut self` step.
#[derive(Debug, Default)]
pub struct ScriptMachineLog {
    resume_order: Mutex<Vec<RequirementId>>,
    resume_tags: Mutex<Vec<RequirementKindTag>>,
    abandon_count: AtomicUsize,
}

impl ScriptMachineLog {
    /// Creates an empty log.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records one resumed requirement's id and result family, in resume order.
    fn record_resume(&self, id: RequirementId, tag: RequirementKindTag) {
        self.resume_order.lock().expect("resume order").push(id);
        self.resume_tags.lock().expect("resume tags").push(tag);
    }

    /// Records one abandonment.
    fn record_abandon(&self) {
        self.abandon_count.fetch_add(1, Ordering::SeqCst);
    }

    /// Returns the requirement ids in the order they were resumed.
    #[must_use]
    pub fn resume_order(&self) -> Vec<RequirementId> {
        self.resume_order.lock().expect("resume order").clone()
    }

    /// Returns the result families in the order they were resumed.
    #[must_use]
    pub fn resume_tags(&self) -> Vec<RequirementKindTag> {
        self.resume_tags.lock().expect("resume tags").clone()
    }

    /// Returns how many requirements have been resumed.
    #[must_use]
    pub fn resume_count(&self) -> usize {
        self.resume_order.lock().expect("resume order").len()
    }

    /// Returns how many requirements have been abandoned.
    #[must_use]
    pub fn abandon_count(&self) -> usize {
        self.abandon_count.load(Ordering::SeqCst)
    }
}

/// A scripted [`AgentMachine`] that emits a fixed requirement batch and settles
/// once every requirement is resumed.
///
/// Build one with [`ScriptMachine::builder`]. See the [module docs](self) for
/// the emit / resume / abandon contract and how the shared
/// [`ScriptMachineLog`] exposes what happened.
#[derive(Debug)]
pub struct ScriptMachine {
    cursor: LoopCursor,
    waiting_cursor: LoopCursor,
    batch: Vec<Requirement>,
    outstanding: BTreeSet<RequirementId>,
    done_after_all_resumed: bool,
    abandon_cursor: Option<LoopCursor>,
    label: String,
    log: Arc<ScriptMachineLog>,
}

impl ScriptMachine {
    /// Starts a fresh [`ScriptMachineBuilder`] with an empty batch.
    #[must_use]
    pub fn builder() -> ScriptMachineBuilder {
        ScriptMachineBuilder::new()
    }

    /// Returns the shared log recording resume order, tags, and abandon count.
    ///
    /// Clone this before handing the machine to a driver so the record survives
    /// the drive.
    #[must_use]
    pub fn log(&self) -> &Arc<ScriptMachineLog> {
        &self.log
    }

    /// Returns the diagnostic label attached at build time (empty by default).
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Returns the ids of requirements emitted but not yet resumed.
    #[must_use]
    pub fn outstanding(&self) -> Vec<RequirementId> {
        self.outstanding.iter().copied().collect()
    }

    /// Returns the requirement ids in the order they were resumed.
    #[must_use]
    pub fn resume_order(&self) -> Vec<RequirementId> {
        self.log.resume_order()
    }

    /// Returns the result families in the order they were resumed.
    #[must_use]
    pub fn resume_tags(&self) -> Vec<RequirementKindTag> {
        self.log.resume_tags()
    }

    /// Returns how many requirements have been abandoned.
    #[must_use]
    pub fn abandon_count(&self) -> usize {
        self.log.abandon_count()
    }

    /// Formats the label as a ` "<label>"` suffix for diagnostics, or empty.
    fn label_suffix(&self) -> String {
        if self.label.is_empty() {
            String::new()
        } else {
            format!(" {:?}", self.label)
        }
    }
}

impl AgentMachine for ScriptMachine {
    fn step(&mut self, input: StepInput) -> StepOutcome {
        match input {
            StepInput::External(_) => {
                // Re-arm the batch and park on the waiting cursor.
                self.outstanding = self
                    .batch
                    .iter()
                    .map(|requirement| requirement.id)
                    .collect();
                self.cursor = self.waiting_cursor.clone();
                StepOutcome::new(Vec::new(), self.batch.clone(), true)
            }
            StepInput::Resume(resolution) => {
                self.log.record_resume(resolution.id, resolution.tag());
                if self.outstanding.remove(&resolution.id) {
                    if self.outstanding.is_empty() && self.done_after_all_resumed {
                        self.cursor = LoopCursor::done(LoopDoneReason::Completed);
                    }
                } else {
                    // A stray result for an id we never emitted is a real
                    // driver/test bug, not something to swallow: surface it as a
                    // diagnostic error cursor.
                    self.cursor = LoopCursor::error(format!(
                        "ScriptMachine{} resumed with unknown requirement id {}",
                        self.label_suffix(),
                        resolution.id
                    ))
                    .expect("unknown-resume message is non-empty");
                }
                StepOutcome::new(Vec::new(), Vec::new(), true)
            }
            StepInput::Abandon(_) => {
                self.log.record_abandon();
                if let Some(cursor) = &self.abandon_cursor {
                    self.cursor = cursor.clone();
                }
                StepOutcome::default()
            }
        }
    }

    fn cursor(&self) -> &LoopCursor {
        &self.cursor
    }
}

/// A fluent builder for [`ScriptMachine`].
///
/// Only the batch is required; every other knob has a sensible default. Note
/// that [`done_after_all_resumed`](Self::done_after_all_resumed) is *opt-in*: a
/// machine built without it parks on its waiting cursor forever, which is what a
/// manual `step`-driving test wants but would make [`drain`](agent_lib::agent::drain)
/// report a non-terminal quiescence. Drain-based tests should call it.
#[derive(Debug, Default)]
pub struct ScriptMachineBuilder {
    requirements: Vec<Requirement>,
    done_after_all_resumed: bool,
    abandon_cursor: Option<LoopCursor>,
    initial_cursor: Option<LoopCursor>,
    label: Option<String>,
}

impl ScriptMachineBuilder {
    /// Creates a builder with an empty batch and default behaviour.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends `requirements` to the fixed batch the machine emits.
    #[must_use]
    pub fn requirements(mut self, requirements: impl IntoIterator<Item = Requirement>) -> Self {
        self.requirements.extend(requirements);
        self
    }

    /// Appends a single [`Requirement`] to the fixed batch.
    #[must_use]
    pub fn requirement(mut self, requirement: Requirement) -> Self {
        self.requirements.push(requirement);
        self
    }

    /// Makes the machine move to [`LoopCursor::Done`] once every outstanding
    /// requirement is resumed.
    ///
    /// Without this, the machine stays on its waiting cursor after the last
    /// resume — useful for manual `step`-driving, but a
    /// [`drain`](agent_lib::agent::drain) needs a terminal cursor to finish.
    #[must_use]
    pub fn done_after_all_resumed(mut self) -> Self {
        self.done_after_all_resumed = true;
        self
    }

    /// Makes the machine settle onto [`LoopCursor::Idle`] on a
    /// [`StepInput::Abandon`].
    ///
    /// A readability shorthand for [`abandon_cursor(LoopCursor::Idle)`](Self::abandon_cursor).
    #[must_use]
    pub fn idle_on_abandon(self) -> Self {
        self.abandon_cursor(LoopCursor::Idle)
    }

    /// Makes the machine settle onto `cursor` on a [`StepInput::Abandon`].
    ///
    /// Leaving this unset keeps the cursor unchanged across an abandonment
    /// (the machine only counts it).
    #[must_use]
    pub fn abandon_cursor(mut self, cursor: LoopCursor) -> Self {
        self.abandon_cursor = Some(cursor);
        self
    }

    /// Sets the non-terminal waiting cursor the machine parks on after emitting
    /// its batch (and holds initially).
    ///
    /// Defaults to a [`LoopCursor::StreamingStep`]. Any non-terminal cursor that
    /// [`drain`](agent_lib::agent::drain) accepts as a rest point works — for
    /// example an [`AwaitingTool`](agent_lib::agent::LoopCursor::AwaitingTool)
    /// cursor to mimic a machine blocked on tool results.
    #[must_use]
    pub fn initial_cursor(mut self, cursor: LoopCursor) -> Self {
        self.initial_cursor = Some(cursor);
        self
    }

    /// Attaches a diagnostic label, surfaced in the unknown-resume error cursor
    /// and via [`ScriptMachine::label`].
    #[must_use]
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Finalises the builder into a [`ScriptMachine`].
    #[must_use]
    pub fn build(self) -> ScriptMachine {
        let waiting_cursor = self.initial_cursor.unwrap_or_else(default_waiting_cursor);
        ScriptMachine {
            cursor: waiting_cursor.clone(),
            waiting_cursor,
            batch: self.requirements,
            outstanding: BTreeSet::new(),
            done_after_all_resumed: self.done_after_all_resumed,
            abandon_cursor: self.abandon_cursor,
            label: self.label.unwrap_or_default(),
            log: Arc::new(ScriptMachineLog::new()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ScriptMachine, ScriptMachineLog};
    use crate::fixtures::{root_context, tool_call, tool_ok, user_input};
    use crate::handlers::ScriptedToolHandler;
    use crate::ids::SeqIds;
    use crate::scope::TestScope;
    use crate::script::ToolStep;
    use agent_lib::agent::{
        AgentMachine, Interaction, InteractionResponse, LoopCursor, LoopCursorKind, LoopDoneReason,
        Requirement, RequirementId, RequirementKind, RequirementKindTag, RequirementResolution,
        RequirementResult, StepInput, drain,
    };
    use std::sync::Arc;

    fn tool_requirement(ids: &SeqIds, provider_call_id: &str) -> Requirement {
        Requirement::at_root(
            ids.requirement_id(),
            RequirementKind::NeedTool {
                call_id: ids.tool_call_id(),
                call: tool_call(
                    provider_call_id,
                    "get_weather",
                    serde_json::json!({ "city": "SH" }),
                ),
            },
        )
    }

    fn interaction_requirement(ids: &SeqIds) -> Requirement {
        Requirement::at_root(
            ids.requirement_id(),
            RequirementKind::NeedInteraction {
                request: Interaction::question(ids.step_id(), "need a human".to_owned()),
            },
        )
    }

    fn tool_resolution(id: RequirementId, provider_call_id: &str) -> RequirementResolution {
        RequirementResolution::new(
            id,
            RequirementResult::Tool(Ok(tool_ok(provider_call_id, "sunny"))),
        )
    }

    fn interaction_resolution(id: RequirementId) -> RequirementResolution {
        RequirementResolution::new(
            id,
            RequirementResult::Interaction(InteractionResponse::answer("ok".to_owned())),
        )
    }

    #[test]
    fn emits_the_batch_and_completes_on_out_of_order_resume() {
        let ids = SeqIds::new();
        let tool = tool_requirement(&ids, "call-weather");
        let interaction = interaction_requirement(&ids);
        let tool_id = tool.id;
        let interaction_id = interaction.id;

        let mut machine = ScriptMachine::builder()
            .requirements([tool, interaction])
            .done_after_all_resumed()
            .build();

        let opened = machine.step(StepInput::external(user_input(&ids, "hi")));
        assert!(opened.is_quiescent());
        assert_eq!(opened.requirements.len(), 2, "the whole batch is emitted");
        assert_eq!(machine.cursor().kind(), LoopCursorKind::StreamingStep);

        // Resume the interaction (second in the batch) before the tool.
        let mid = machine.step(StepInput::resume(interaction_resolution(interaction_id)));
        assert!(mid.requirements.is_empty());
        assert_eq!(
            machine.cursor().kind(),
            LoopCursorKind::StreamingStep,
            "one requirement is still outstanding"
        );

        let done = machine.step(StepInput::resume(tool_resolution(tool_id, "call-weather")));
        assert!(done.is_quiescent());
        assert_eq!(machine.cursor().kind(), LoopCursorKind::Done);

        // Resume order and families are recorded in resume order, not emit order.
        assert_eq!(machine.resume_order(), vec![interaction_id, tool_id]);
        assert_eq!(
            machine.resume_tags(),
            vec![RequirementKindTag::Interaction, RequirementKindTag::Tool]
        );
        assert_eq!(machine.abandon_count(), 0);
    }

    #[test]
    fn unknown_resume_id_moves_to_an_error_cursor() {
        let ids = SeqIds::new();
        let tool = tool_requirement(&ids, "call-weather");
        let known_id = tool.id;

        let mut machine = ScriptMachine::builder()
            .requirement(tool)
            .done_after_all_resumed()
            .label("child")
            .build();

        machine.step(StepInput::external(user_input(&ids, "hi")));

        // A resolution for an id the machine never emitted.
        let stray_id = ids.requirement_id();
        assert_ne!(stray_id, known_id);
        let outcome = machine.step(StepInput::resume(tool_resolution(stray_id, "call-stray")));

        assert!(outcome.requirements.is_empty());
        assert_eq!(machine.cursor().kind(), LoopCursorKind::Error);
        // The genuine requirement is untouched, so it is still outstanding.
        assert_eq!(machine.outstanding(), vec![known_id]);
    }

    #[test]
    fn abandon_behaviour_is_configurable() {
        let ids = SeqIds::new();

        // idle_on_abandon settles onto Idle.
        let mut idling = ScriptMachine::builder()
            .requirement(tool_requirement(&ids, "call-a"))
            .idle_on_abandon()
            .build();
        let opened = idling.step(StepInput::external(user_input(&ids, "hi")));
        idling.step(StepInput::abandon(opened.requirements[0].id));
        assert_eq!(idling.cursor().kind(), LoopCursorKind::Idle);
        assert_eq!(idling.abandon_count(), 1);

        // No abandon cursor: the cursor is left on the waiting state.
        let mut sticky = ScriptMachine::builder()
            .requirement(tool_requirement(&ids, "call-b"))
            .build();
        let opened = sticky.step(StepInput::external(user_input(&ids, "hi")));
        sticky.step(StepInput::abandon(opened.requirements[0].id));
        assert_eq!(sticky.cursor().kind(), LoopCursorKind::StreamingStep);
        assert_eq!(sticky.abandon_count(), 1);

        // An explicit abandon cursor settles onto that cursor.
        let mut finishing = ScriptMachine::builder()
            .requirement(tool_requirement(&ids, "call-c"))
            .abandon_cursor(LoopCursor::done(LoopDoneReason::Cancelled))
            .build();
        let opened = finishing.step(StepInput::external(user_input(&ids, "hi")));
        finishing.step(StepInput::abandon(opened.requirements[0].id));
        assert_eq!(finishing.cursor().kind(), LoopCursorKind::Done);
    }

    #[tokio::test]
    async fn drain_fulfils_a_local_tool_batch_through_a_test_scope() {
        // A ScriptMachine emitting a NeedTool, drained against a TestScope that
        // serves the tool family locally, must run to Done with the tool called
        // once and the resume recorded.
        let ids = SeqIds::new();
        let ctx = root_context(&ids);
        let tool = tool_requirement(&ids, "call-weather");
        let tool_id = tool.id;

        let mut machine = ScriptMachine::builder()
            .requirement(tool)
            .done_after_all_resumed()
            .build();
        let machine_log = Arc::clone(machine.log());

        let handler = ScriptedToolHandler::from_steps([ToolStep::ok("call-weather", "sunny")]);
        let tool_log = Arc::clone(handler.log());
        let scope = TestScope::builder().tool(Arc::new(handler)).build();

        let done = drain(
            &mut machine,
            user_input(&ids, "weather?"),
            &scope,
            None,
            &ctx,
        )
        .await
        .expect("the local tool batch drains to done");

        assert_eq!(done.cursor().kind(), LoopCursorKind::Done);
        assert_eq!(tool_log.len(), 1, "the scripted tool ran exactly once");
        assert_eq!(machine_log.resume_order(), vec![tool_id]);
        assert_eq!(machine_log.resume_tags(), vec![RequirementKindTag::Tool]);
        assert_eq!(machine_log.abandon_count(), 0);
    }

    #[test]
    fn log_starts_empty() {
        let log = ScriptMachineLog::new();
        assert_eq!(log.resume_count(), 0);
        assert_eq!(log.abandon_count(), 0);
        assert!(log.resume_order().is_empty());
        assert!(log.resume_tags().is_empty());
    }
}
