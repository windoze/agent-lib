//! Synchronous step harness that reduces the boilerplate of driving an
//! [`AgentMachine`] one [`StepInput`] at a time.
//!
//! Many basic agent-layer tests advance the machine by hand: build an
//! [`AgentInput`], call [`AgentMachine::step`], pull the single requirement out
//! of the returned [`StepOutcome`], fabricate a [`RequirementResolution`], step
//! again, and inspect the cursor after every hop. [`StepHarness`] collapses that
//! pattern into a handful of named moves — [`user`](StepHarness::user),
//! [`external`](StepHarness::external), [`pivot`](StepHarness::pivot),
//! [`resume`](StepHarness::resume), [`abandon`](StepHarness::abandon) — while
//! keeping every intermediate [`Requirement`], [`Notification`], and
//! [`LoopCursor`] snapshot visible on the returned [`StepObservation`].
//!
//! # Synchronous by construction
//!
//! The whole harness is `async`-free: [`AgentMachine::step`] is a pure,
//! synchronous pull, and this harness only advances that pull and folds the
//! result back in. There is no client, tool, or process to await, so a plain
//! `#[test]` can drive a full step-by-step turn without a runtime.
//!
//! # It exposes, it does not hide
//!
//! Unlike the drain harness (M4-2), which runs a whole turn to completion, the
//! step harness deliberately stops at every blocking point so a test can assert
//! on the exact requirement batch the machine emitted mid-turn. The harness
//! tracks the still-outstanding requirements itself, so a misaddressed
//! [`resume`](StepHarness::resume) or [`abandon`](StepHarness::abandon) fails
//! *before* the machine is stepped, with a diagnostic that names the current
//! cursor, the outstanding requirement ids, and the most recent step label.

use std::collections::BTreeMap;
use std::fmt;

use agent_lib::agent::{
    AgentInput, AgentMachine, LoopCursor, LoopCursorKind, Notification, PivotSource, QueuedPivot,
    Requirement, RequirementId, RequirementKindTag, RequirementResolution, RequirementResult,
    StepInput, StepOutcome,
};

use crate::fixtures::{user_input, user_message};
use crate::ids::SeqIds;

/// A synchronous driver that advances an [`AgentMachine`] one step at a time and
/// keeps every intermediate observation inspectable.
///
/// Build one with [`StepHarness::new`] (which mints a fresh [`SeqIds`] for the
/// `user`/`pivot` conveniences) or [`StepHarness::with_ids`] to share an existing
/// id tree with the fixtures that built the machine. Each stepping move returns a
/// [`StepObservation`]; misuse (a resume/abandon addressing a requirement that is
/// not outstanding, or a result of the wrong family) surfaces as a
/// [`StepHarnessError`] whose message carries the current cursor, the outstanding
/// requirement ids, and the last step label.
pub struct StepHarness<M: AgentMachine> {
    machine: M,
    ids: SeqIds,
    outstanding: BTreeMap<RequirementId, Requirement>,
    last_label: Option<String>,
}

impl<M: AgentMachine> StepHarness<M> {
    /// Wraps `machine`, minting a fresh [`SeqIds`] for the `user`/`pivot`
    /// conveniences.
    #[must_use]
    pub fn new(machine: M) -> Self {
        Self::with_ids(machine, SeqIds::new())
    }

    /// Wraps `machine`, drawing turn/message/step ids for the `user`/`pivot`
    /// conveniences from `ids`.
    ///
    /// Pass the same [`SeqIds`] the machine was built from so every fabricated
    /// input id stays globally unique within the test tree.
    #[must_use]
    pub fn with_ids(machine: M, ids: SeqIds) -> Self {
        Self {
            machine,
            ids,
            outstanding: BTreeMap::new(),
            last_label: None,
        }
    }

    /// Returns a shared reference to the wrapped machine.
    pub const fn machine(&self) -> &M {
        &self.machine
    }

    /// Returns a mutable reference to the wrapped machine.
    ///
    /// Prefer the stepping moves; this is an escape hatch for machine-specific
    /// configuration that must happen after wrapping.
    pub const fn machine_mut(&mut self) -> &mut M {
        &mut self.machine
    }

    /// Returns the id source backing the `user`/`pivot` conveniences.
    #[must_use]
    pub const fn ids(&self) -> &SeqIds {
        &self.ids
    }

    /// Returns a read-only view of the machine's current cursor.
    pub fn cursor(&self) -> &LoopCursor {
        self.machine.cursor()
    }

    /// Returns the ids of every requirement the machine has emitted but not yet
    /// had resumed or abandoned, in id order.
    #[must_use]
    pub fn outstanding_ids(&self) -> Vec<RequirementId> {
        self.outstanding.keys().copied().collect()
    }

    /// Returns the label of the most recently attempted step, if any.
    #[must_use]
    pub fn last_label(&self) -> Option<&str> {
        self.last_label.as_deref()
    }

    /// Consumes the harness and returns the wrapped machine.
    #[must_use]
    pub fn into_machine(self) -> M {
        self.machine
    }

    /// Opens or soft-turns the machine with a fresh external `input`.
    ///
    /// This is the general escape hatch; [`user`](Self::user) and
    /// [`pivot`](Self::pivot) are readable shorthands over it.
    pub fn external(&mut self, input: AgentInput) -> StepObservation {
        let label = external_label(&input);
        self.drive_external(input, label)
    }

    /// Opens a new user turn carrying `text`, minting fresh ids from the
    /// harness's [`SeqIds`].
    pub fn user(&mut self, text: &str) -> StepObservation {
        let input = user_input(&self.ids, text);
        self.drive_external(input, format!("user({text:?})"))
    }

    /// Injects a `Role::User` [`PivotSource::Human`] pivot carrying `text` at the
    /// current step boundary.
    ///
    /// Pivot injection is only legal on the machines and cursors that accept it
    /// (for [`DefaultAgentMachine`](agent_lib::agent::DefaultAgentMachine), a
    /// streaming-step boundary); the machine decides whether to fold the pivot in
    /// or fail its cursor, and this harness reports whatever it returns.
    pub fn pivot(&mut self, text: &str) -> StepObservation {
        let pivot = QueuedPivot::new(
            self.ids.message_id(),
            user_message(text),
            PivotSource::Human,
        )
        .expect("user_message fixture is always Role::User");
        self.drive_external(AgentInput::pivot(pivot), format!("pivot({text:?})"))
    }

    /// Feeds a requirement's fulfilled `result` back into the machine.
    ///
    /// Panics with a [`StepHarnessError`] diagnostic when `id` is not currently
    /// outstanding or when `result` is the wrong family for the requirement; use
    /// [`try_resume`](Self::try_resume) for the fallible form.
    pub fn resume(&mut self, id: RequirementId, result: RequirementResult) -> StepObservation {
        self.try_resume(id, result)
            .unwrap_or_else(|error| panic!("{error}"))
    }

    /// Fallible [`resume`](Self::resume): returns the diagnostic instead of
    /// panicking when the resume is misaddressed or mistyped.
    ///
    /// # Errors
    ///
    /// Returns a [`StepHarnessError`] when `id` is not an outstanding
    /// requirement, or when `result`'s family does not match the requirement's
    /// family. The machine is not stepped in either case.
    pub fn try_resume(
        &mut self,
        id: RequirementId,
        result: RequirementResult,
    ) -> Result<StepObservation, StepHarnessError> {
        let label = format!("resume({id})");
        let resolution = RequirementResolution::new(id, result);
        let rejection = match self.outstanding.get(&id) {
            None => Some(format!(
                "resume targets requirement `{id}`, which is not outstanding"
            )),
            Some(requirement) => requirement
                .accepts_resolution(&resolution)
                .err()
                .map(|error| format!("resume result rejected: {error}")),
        };
        if let Some(message) = rejection {
            self.last_label = Some(label);
            return Err(self.error(message));
        }

        let outcome = self.machine.step(StepInput::resume(resolution));
        self.outstanding.remove(&id);
        self.ingest(&outcome);
        self.last_label = Some(label.clone());
        Ok(self.build_observation(label, outcome))
    }

    /// Discards an outstanding requirement without ever resuming it.
    ///
    /// Panics with a [`StepHarnessError`] diagnostic when `id` is not currently
    /// outstanding; use [`try_abandon`](Self::try_abandon) for the fallible form.
    pub fn abandon(&mut self, id: RequirementId) -> StepObservation {
        self.try_abandon(id)
            .unwrap_or_else(|error| panic!("{error}"))
    }

    /// Fallible [`abandon`](Self::abandon): returns the diagnostic instead of
    /// panicking when `id` is not outstanding.
    ///
    /// # Errors
    ///
    /// Returns a [`StepHarnessError`] when `id` is not an outstanding
    /// requirement. The machine is not stepped in that case.
    pub fn try_abandon(&mut self, id: RequirementId) -> Result<StepObservation, StepHarnessError> {
        let label = format!("abandon({id})");
        if !self.outstanding.contains_key(&id) {
            self.last_label = Some(label);
            return Err(self.error(format!(
                "abandon targets requirement `{id}`, which is not outstanding"
            )));
        }

        let outcome = self.machine.step(StepInput::abandon(id));
        self.outstanding.remove(&id);
        self.ingest(&outcome);
        self.last_label = Some(label.clone());
        Ok(self.build_observation(label, outcome))
    }

    /// Steps the machine on a fresh external input and records the result.
    fn drive_external(&mut self, input: AgentInput, label: String) -> StepObservation {
        let outcome = self.machine.step(StepInput::external(input));
        self.ingest(&outcome);
        self.last_label = Some(label.clone());
        self.build_observation(label, outcome)
    }

    /// Folds a step's freshly emitted requirements into the outstanding set.
    ///
    /// Re-emitting an id (as a pivot does under the same requirement id) simply
    /// refreshes the stored requirement rather than duplicating it.
    fn ingest(&mut self, outcome: &StepOutcome) {
        for requirement in &outcome.requirements {
            self.outstanding.insert(requirement.id, requirement.clone());
        }
    }

    /// Builds a [`StepObservation`] from a completed step and the current cursor.
    fn build_observation(&self, label: String, outcome: StepOutcome) -> StepObservation {
        StepObservation {
            label,
            notifications: outcome.notifications,
            requirements: outcome.requirements,
            quiescent: outcome.quiescent,
            cursor: self.machine.cursor().clone(),
        }
    }

    /// Builds a harness-level diagnostic stamped with the current cursor,
    /// outstanding ids, and last step label.
    fn error(&self, message: String) -> StepHarnessError {
        StepHarnessError {
            message,
            cursor: self.machine.cursor().kind(),
            outstanding: self.outstanding.keys().copied().collect(),
            last_label: self.last_label.clone(),
        }
    }
}

impl<M: AgentMachine + fmt::Debug> fmt::Debug for StepHarness<M> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StepHarness")
            .field("machine", &self.machine)
            .field("cursor", &self.machine.cursor().kind())
            .field("outstanding", &self.outstanding_ids())
            .field("last_label", &self.last_label)
            .finish()
    }
}

/// The product of one [`StepHarness`] move: the notifications and requirements
/// this step produced, whether the machine came to rest, and a snapshot of the
/// cursor afterwards.
///
/// The convenience extractors ([`single_llm`](Self::single_llm),
/// [`single_tool`](Self::single_tool),
/// [`single_interaction`](Self::single_interaction),
/// [`requirements_by_tag`](Self::requirements_by_tag)) pull a specific
/// requirement out of the batch without a hand-written `match`, and report a
/// diagnostic that names the observed cursor and requirement families when the
/// expectation does not hold.
#[derive(Clone, Debug)]
pub struct StepObservation {
    label: String,
    notifications: Vec<Notification>,
    requirements: Vec<Requirement>,
    quiescent: bool,
    cursor: LoopCursor,
}

impl StepObservation {
    /// Returns the label of the step that produced this observation.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Returns the notifications emitted this step.
    #[must_use]
    pub fn notifications(&self) -> &[Notification] {
        &self.notifications
    }

    /// Returns the requirements the machine newly blocked on this step.
    #[must_use]
    pub fn requirements(&self) -> &[Requirement] {
        &self.requirements
    }

    /// Returns whether the machine came to rest after this step.
    #[must_use]
    pub const fn is_quiescent(&self) -> bool {
        self.quiescent
    }

    /// Returns a snapshot of the machine's cursor after this step.
    #[must_use]
    pub const fn cursor(&self) -> &LoopCursor {
        &self.cursor
    }

    /// Returns every requirement of the `tag` family emitted this step, in
    /// emission order.
    #[must_use]
    pub fn requirements_by_tag(&self, tag: RequirementKindTag) -> Vec<&Requirement> {
        self.requirements
            .iter()
            .filter(|requirement| requirement.tag() == tag)
            .collect()
    }

    /// Returns the single requirement emitted this step, of any family.
    ///
    /// # Errors
    ///
    /// Returns a [`StepHarnessError`] when the step emitted zero or more than one
    /// requirement.
    pub fn single(&self) -> Result<&Requirement, StepHarnessError> {
        match self.requirements.as_slice() {
            [requirement] => Ok(requirement),
            requirements => Err(self.observation_error(format!(
                "expected exactly one requirement, found {} ({})",
                requirements.len(),
                tags_summary(requirements)
            ))),
        }
    }

    /// Returns the single `NeedLlm` requirement emitted this step.
    ///
    /// # Errors
    ///
    /// Returns a [`StepHarnessError`] when the step did not emit exactly one
    /// requirement of the LLM family.
    pub fn single_llm(&self) -> Result<&Requirement, StepHarnessError> {
        self.single_of(RequirementKindTag::Llm)
    }

    /// Returns the single `NeedTool` requirement emitted this step.
    ///
    /// # Errors
    ///
    /// Returns a [`StepHarnessError`] when the step did not emit exactly one
    /// requirement of the tool family.
    pub fn single_tool(&self) -> Result<&Requirement, StepHarnessError> {
        self.single_of(RequirementKindTag::Tool)
    }

    /// Returns the single `NeedInteraction` requirement emitted this step.
    ///
    /// # Errors
    ///
    /// Returns a [`StepHarnessError`] when the step did not emit exactly one
    /// requirement of the interaction family.
    pub fn single_interaction(&self) -> Result<&Requirement, StepHarnessError> {
        self.single_of(RequirementKindTag::Interaction)
    }

    /// Returns the single requirement of `tag`, or a diagnostic naming what was
    /// actually emitted.
    fn single_of(&self, tag: RequirementKindTag) -> Result<&Requirement, StepHarnessError> {
        let matches = self.requirements_by_tag(tag);
        if matches.len() == 1 {
            Ok(matches[0])
        } else {
            Err(self.observation_error(format!(
                "expected exactly one `{tag}` requirement, found {} of {} total ({})",
                matches.len(),
                self.requirements.len(),
                tags_summary(&self.requirements)
            )))
        }
    }

    /// Builds an extractor diagnostic stamped with this step's cursor, the ids of
    /// the requirements this step emitted, and this step's label.
    fn observation_error(&self, message: String) -> StepHarnessError {
        StepHarnessError {
            message,
            cursor: self.cursor.kind(),
            outstanding: self.requirements.iter().map(|req| req.id).collect(),
            last_label: Some(self.label.clone()),
        }
    }
}

/// A diagnostic produced by a misaddressed step or a failed extractor.
///
/// Its [`Display`](fmt::Display) always names the current cursor, the outstanding
/// requirement ids, and the most recent step label, so a failing assertion points
/// at *where* the machine actually is rather than only what was expected.
#[derive(Clone, Debug)]
pub struct StepHarnessError {
    message: String,
    cursor: LoopCursorKind,
    outstanding: Vec<RequirementId>,
    last_label: Option<String>,
}

impl StepHarnessError {
    /// Returns the human-readable summary of what went wrong.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Returns the cursor kind the machine (or observation) was in.
    #[must_use]
    pub const fn cursor(&self) -> LoopCursorKind {
        self.cursor
    }

    /// Returns the outstanding requirement ids at the point of failure.
    #[must_use]
    pub fn outstanding(&self) -> &[RequirementId] {
        &self.outstanding
    }

    /// Returns the most recent step label, if any.
    #[must_use]
    pub fn last_label(&self) -> Option<&str> {
        self.last_label.as_deref()
    }
}

impl fmt::Display for StepHarnessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} [cursor: {:?}, outstanding: [",
            self.message, self.cursor
        )?;
        for (index, id) in self.outstanding.iter().enumerate() {
            if index > 0 {
                write!(f, ", ")?;
            }
            write!(f, "{id}")?;
        }
        match &self.last_label {
            Some(label) => write!(f, "], last step: {label}]"),
            None => write!(f, "], last step: <none>]"),
        }
    }
}

impl std::error::Error for StepHarnessError {}

/// Renders a stable per-family summary of a requirement batch for diagnostics.
fn tags_summary(requirements: &[Requirement]) -> String {
    if requirements.is_empty() {
        return "no requirements".to_owned();
    }
    requirements
        .iter()
        .map(|requirement| requirement.tag().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Derives a stable label for a fresh external input.
fn external_label(input: &AgentInput) -> String {
    match input {
        AgentInput::UserMessage(_) => "external(user_message)".to_owned(),
        AgentInput::Pivot(_) => "external(pivot)".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::StepHarness;
    use crate::fixtures::{
        agent_spec, agent_spec_with_tools, agent_state, assistant_text, assistant_tool_use,
        default_machine, tool_call, usage, weather_tool,
    };
    use crate::ids::SeqIds;
    use agent_lib::agent::{
        LoopCursorKind, RequirementId, RequirementKind, RequirementKindTag, RequirementResult,
    };
    use serde_json::json;

    #[test]
    fn text_only_turn_runs_step_by_step() {
        let ids = SeqIds::new();
        let machine = default_machine(&ids, agent_state(&ids, agent_spec(&ids)));
        let mut harness = StepHarness::with_ids(machine, ids);

        // Opening the turn parks on exactly one NeedLlm requirement.
        let opened = harness.user("hello");
        assert!(!opened.is_quiescent() || opened.requirements().len() == 1);
        assert_eq!(opened.cursor().kind(), LoopCursorKind::StreamingStep);
        let llm = opened.single_llm().expect("a text turn opens on NeedLlm");
        assert!(matches!(llm.kind, RequirementKind::NeedLlm { .. }));
        let llm_id = llm.id;
        assert_eq!(harness.outstanding_ids(), vec![llm_id]);

        // Resuming with an assistant text response commits the turn and rests.
        let committed = harness.resume(
            llm_id,
            RequirementResult::Llm(Ok(assistant_text("hi", usage(3, 2)))),
        );
        assert!(committed.is_quiescent());
        assert!(committed.requirements().is_empty());
        assert_eq!(committed.cursor().kind(), LoopCursorKind::Done);
        assert!(harness.outstanding_ids().is_empty());
    }

    #[test]
    fn tool_turn_exposes_the_intermediate_tool_requirement() {
        let ids = SeqIds::new();
        let spec = agent_spec_with_tools(&ids, vec![weather_tool()]);
        let machine = default_machine(&ids, agent_state(&ids, spec));
        let mut harness = StepHarness::with_ids(machine, ids);

        let opened = harness.user("weather?");
        let llm_id = opened.single_llm().expect("opens on NeedLlm").id;

        // A tool-use response folds into a mid-turn NeedTool that the harness
        // exposes rather than draining past.
        let call = tool_call("call-weather", "get_weather", json!({ "city": "SH" }));
        let folded = harness.resume(
            llm_id,
            RequirementResult::Llm(Ok(assistant_tool_use(vec![call], usage(5, 2)))),
        );
        let tool = folded.single_tool().expect("tool-use folds into NeedTool");
        assert_eq!(folded.requirements_by_tag(RequirementKindTag::Llm).len(), 0);
        assert!(matches!(tool.kind, RequirementKind::NeedTool { .. }));
        assert_eq!(harness.outstanding_ids(), vec![tool.id]);
    }

    #[test]
    fn wrong_id_resume_reports_cursor_and_outstanding_ids() {
        let ids = SeqIds::new();
        let machine = default_machine(&ids, agent_state(&ids, agent_spec(&ids)));
        let mut harness = StepHarness::with_ids(machine, ids);

        let opened = harness.user("hello");
        let real_id = opened.single_llm().expect("opens on NeedLlm").id;

        let stray = RequirementId::parse_str("018f0d9c-7b6a-7c12-8f31-0000feedbeef")
            .expect("valid stray id");
        let error = harness
            .try_resume(
                stray,
                RequirementResult::Llm(Ok(assistant_text("hi", usage(1, 1)))),
            )
            .expect_err("a stray id cannot be resumed");

        // The diagnostic names the cursor, the outstanding requirement, and the
        // attempted step label.
        assert_eq!(error.cursor(), LoopCursorKind::StreamingStep);
        assert_eq!(error.outstanding(), [real_id].as_slice());
        let rendered = error.to_string();
        assert!(rendered.contains("StreamingStep"), "cursor: {rendered}");
        assert!(rendered.contains(&real_id.to_string()), "id: {rendered}");
        assert!(rendered.contains(&stray.to_string()), "stray: {rendered}");
        assert!(rendered.contains("resume("), "label: {rendered}");

        // The machine was never stepped: the real requirement is still open.
        assert_eq!(harness.outstanding_ids(), vec![real_id]);
        assert_eq!(harness.cursor().kind(), LoopCursorKind::StreamingStep);

        // And resuming the real id still commits cleanly afterwards.
        let committed = harness.resume(
            real_id,
            RequirementResult::Llm(Ok(assistant_text("hi", usage(1, 1)))),
        );
        assert_eq!(committed.cursor().kind(), LoopCursorKind::Done);
    }

    #[test]
    fn wrong_family_resume_is_rejected_before_stepping() {
        let ids = SeqIds::new();
        let machine = default_machine(&ids, agent_state(&ids, agent_spec(&ids)));
        let mut harness = StepHarness::with_ids(machine, ids);

        let llm_id = harness.user("hello").single_llm().expect("NeedLlm").id;

        // A tool result cannot fulfil an LLM requirement; the harness rejects it
        // without advancing the machine.
        let error = harness
            .try_resume(
                llm_id,
                RequirementResult::Tool(Ok(crate::fixtures::tool_ok("call-x", "nope"))),
            )
            .expect_err("wrong family is rejected");
        assert!(error.message().contains("rejected"), "{error}");
        assert_eq!(harness.outstanding_ids(), vec![llm_id]);
        assert_eq!(harness.cursor().kind(), LoopCursorKind::StreamingStep);
    }

    #[test]
    fn abandon_reports_and_then_clears_the_outstanding_requirement() {
        let ids = SeqIds::new();
        let machine = default_machine(&ids, agent_state(&ids, agent_spec(&ids)));
        let mut harness = StepHarness::with_ids(machine, ids);

        let llm_id = harness.user("hello").single_llm().expect("NeedLlm").id;

        // Abandoning an id the machine never emitted is rejected up front.
        let stray = RequirementId::parse_str("018f0d9c-7b6a-7c12-8f31-0000abadcafe")
            .expect("valid stray id");
        let error = harness
            .try_abandon(stray)
            .expect_err("stray abandon is rejected");
        assert!(error.to_string().contains("not outstanding"), "{error}");

        // Abandoning the real requirement clears it from the outstanding set.
        harness.abandon(llm_id);
        assert!(harness.outstanding_ids().is_empty());
    }

    #[test]
    fn single_extractors_diagnose_a_missing_family() {
        let ids = SeqIds::new();
        let machine = default_machine(&ids, agent_state(&ids, agent_spec(&ids)));
        let mut harness = StepHarness::with_ids(machine, ids);

        let opened = harness.user("hello");
        // The step emitted an LLM requirement, so asking for a tool requirement
        // must fail with a family-aware diagnostic.
        let error = opened
            .single_tool()
            .expect_err("no tool requirement was emitted");
        assert!(error.message().contains("tool"), "{error}");
        assert!(error.message().contains("llm"), "summary: {error}");
    }

    /// A plain synchronous test witnesses that the harness needs no runtime: the
    /// whole step contract is `async`-free.
    #[test]
    fn step_harness_drives_without_async() {
        let ids = SeqIds::new();
        let machine = default_machine(&ids, agent_state(&ids, agent_spec(&ids)));
        let mut harness = StepHarness::with_ids(machine, ids);

        let llm_id = harness.user("hi").single_llm().expect("NeedLlm").id;
        let done = harness.resume(
            llm_id,
            RequirementResult::Llm(Ok(assistant_text("ok", usage(1, 1)))),
        );
        assert!(done.is_quiescent());
    }
}
