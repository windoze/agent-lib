//! Read-only assertions over the [`Requirement`] batch a machine step emitted.
//!
//! [`assert_requirements`] wraps a `&[Requirement]` — typically
//! [`StepObservation::requirements`](crate::harness::StepObservation::requirements)
//! — and lets a test assert on the batch shape (count, single family) and then
//! drill into one requirement through a [`RequirementView`] (id, origin, family,
//! request summary).

use agent_lib::agent::{
    AgentPath, LlmStepMode, Requirement, RequirementId, RequirementKind, RequirementKindTag,
};
use agent_lib::model::tool::ToolCall;

/// Starts a fluent, read-only assertion over a requirement batch.
#[must_use]
pub fn assert_requirements(requirements: &[Requirement]) -> RequirementAssertions<'_> {
    RequirementAssertions { requirements }
}

/// A fluent, read-only assertion builder over a slice of [`Requirement`]s.
#[derive(Clone, Copy)]
pub struct RequirementAssertions<'a> {
    requirements: &'a [Requirement],
}

impl<'a> RequirementAssertions<'a> {
    /// Asserts the number of requirements in the batch.
    pub fn count(self, expected: usize) -> Self {
        let actual = self.requirements.len();
        assert!(
            actual == expected,
            "expected {expected} requirement(s), found {actual}: {}",
            self.summary()
        );
        self
    }

    /// Asserts that the batch is empty (a quiescent step emits none).
    pub fn empty(self) -> Self {
        assert!(
            self.requirements.is_empty(),
            "expected no requirements, found {}: {}",
            self.requirements.len(),
            self.summary()
        );
        self
    }

    /// Asserts exactly one requirement is present and returns a view over it.
    pub fn single(self) -> RequirementView<'a> {
        assert!(
            self.requirements.len() == 1,
            "expected exactly one requirement, found {}: {}",
            self.requirements.len(),
            self.summary()
        );
        RequirementView {
            requirement: &self.requirements[0],
        }
    }

    /// Asserts exactly one requirement of the given family and returns a view.
    pub fn single_of(self, tag: RequirementKindTag) -> RequirementView<'a> {
        let matches: Vec<&Requirement> = self
            .requirements
            .iter()
            .filter(|requirement| requirement.tag() == tag)
            .collect();
        assert!(
            matches.len() == 1,
            "expected exactly one `{tag}` requirement, found {} (of {} total): {}",
            matches.len(),
            self.requirements.len(),
            self.summary()
        );
        RequirementView {
            requirement: matches[0],
        }
    }

    /// Asserts exactly one `NeedLlm` requirement and returns a view.
    pub fn single_llm(self) -> RequirementView<'a> {
        self.single_of(RequirementKindTag::Llm)
    }

    /// Asserts exactly one `NeedTool` requirement and returns a view.
    pub fn single_tool(self) -> RequirementView<'a> {
        self.single_of(RequirementKindTag::Tool)
    }

    /// Asserts exactly one `NeedInteraction` requirement and returns a view.
    pub fn single_interaction(self) -> RequirementView<'a> {
        self.single_of(RequirementKindTag::Interaction)
    }

    /// Asserts exactly one `NeedSubagent` requirement and returns a view.
    pub fn single_subagent(self) -> RequirementView<'a> {
        self.single_of(RequirementKindTag::Subagent)
    }

    /// Asserts exactly one `NeedReconfigRegistry` requirement and returns a view.
    pub fn single_reconfig(self) -> RequirementView<'a> {
        self.single_of(RequirementKindTag::Reconfig)
    }

    fn summary(self) -> String {
        if self.requirements.is_empty() {
            return "[]".to_owned();
        }
        let items = self
            .requirements
            .iter()
            .map(describe_requirement)
            .collect::<Vec<_>>()
            .join(", ");
        format!("[{items}]")
    }
}

/// A read-only view over one [`Requirement`], returned by the `single*`
/// assertions. Every assertion method returns `Self` so checks chain; the
/// getters expose the underlying data for family-specific inspection.
#[derive(Clone, Copy)]
pub struct RequirementView<'a> {
    requirement: &'a Requirement,
}

impl<'a> RequirementView<'a> {
    /// Returns the underlying requirement for escape-hatch inspection.
    pub const fn requirement(self) -> &'a Requirement {
        self.requirement
    }

    /// Asserts the requirement's routing id.
    pub fn id(self, expected: RequirementId) -> Self {
        assert!(
            self.requirement.id == expected,
            "expected requirement id {expected}, found {}: {}",
            self.requirement.id,
            describe_requirement(self.requirement)
        );
        self
    }

    /// Asserts the requirement's family tag.
    pub fn tag(self, expected: RequirementKindTag) -> Self {
        let actual = self.requirement.tag();
        assert!(
            actual == expected,
            "expected requirement family `{expected}`, found `{actual}`: {}",
            describe_requirement(self.requirement)
        );
        self
    }

    /// Asserts the requirement originates at the root machine (empty path).
    pub fn origin_root(self) -> Self {
        assert!(
            self.requirement.origin.is_root(),
            "expected requirement to originate at the root, found origin {:?}: {}",
            self.requirement.origin,
            describe_requirement(self.requirement)
        );
        self
    }

    /// Asserts the requirement's origin path equals `expected`.
    pub fn origin(self, expected: &AgentPath) -> Self {
        assert!(
            &self.requirement.origin == expected,
            "expected requirement origin {expected:?}, found {:?}: {}",
            self.requirement.origin,
            describe_requirement(self.requirement)
        );
        self
    }

    /// Asserts a `NeedLlm` requirement carries the given transport mode.
    pub fn llm_mode(self, expected: LlmStepMode) -> Self {
        match &self.requirement.kind {
            RequirementKind::NeedLlm { mode, .. } => {
                assert!(
                    *mode == expected,
                    "expected LLM mode {expected:?}, found {mode:?}: {}",
                    describe_requirement(self.requirement)
                );
            }
            other => panic!(
                "expected a `NeedLlm` requirement, found `{}`: {}",
                other.tag(),
                describe_requirement(self.requirement)
            ),
        }
        self
    }

    /// Returns the [`ToolCall`] of a `NeedTool` requirement, panicking otherwise.
    pub fn tool_call(self) -> &'a ToolCall {
        match &self.requirement.kind {
            RequirementKind::NeedTool { call, .. } => call,
            other => panic!(
                "expected a `NeedTool` requirement, found `{}`: {}",
                other.tag(),
                describe_requirement(self.requirement)
            ),
        }
    }

    /// Asserts a `NeedTool` requirement selects the tool named `expected`.
    pub fn tool_name(self, expected: &str) -> Self {
        let actual = &self.tool_call().name;
        assert!(
            actual == expected,
            "expected tool name {expected:?}, found {actual:?}: {}",
            describe_requirement(self.requirement)
        );
        self
    }

    /// Returns a compact, human-readable summary of the request payload.
    pub fn request_summary(self) -> String {
        describe_requirement(self.requirement)
    }
}

/// Renders one requirement as a compact, family-tagged summary for diagnostics
/// and for [`RequirementView::request_summary`].
fn describe_requirement(requirement: &Requirement) -> String {
    let origin = if requirement.origin.is_root() {
        "root".to_owned()
    } else {
        format!("{:?}", requirement.origin)
    };
    let body = match &requirement.kind {
        RequirementKind::NeedLlm { request, mode } => {
            format!("llm({} msg, {mode:?})", request.messages.len())
        }
        RequirementKind::NeedTool { call, .. } => format!("tool({}, id={})", call.name, call.id),
        RequirementKind::NeedInteraction { request } => {
            format!("interaction({})", request.kind().tag())
        }
        RequirementKind::NeedSubagent { spec_ref, .. } => format!("subagent({:?})", spec_ref.0),
        RequirementKind::NeedReconfigRegistry { tool_set } => {
            format!("reconfig(tool_set={:?})", tool_set.id())
        }
        RequirementKind::NeedExternalSession { request } => {
            format!(
                "external({:?}, agent={:?})",
                request.runtime, request.agent_id
            )
        }
    };
    format!("{{id={}, origin={origin}, {body}}}", requirement.id)
}

#[cfg(test)]
mod tests {
    use super::assert_requirements;
    use crate::fixtures::{
        agent_spec_with_tools, agent_state, assistant_tool_use, default_machine, tool_call, usage,
        user_input,
    };
    use crate::ids::SeqIds;
    use agent_lib::agent::{
        AgentMachine, LlmStepMode, RequirementKindTag, RequirementResolution, RequirementResult,
        StepInput,
    };
    use serde_json::json;

    /// Opens a text turn and returns the single `NeedLlm` requirement's batch.
    fn open_llm_batch() -> (
        SeqIds,
        agent_lib::agent::DefaultAgentMachine,
        Vec<agent_lib::agent::Requirement>,
    ) {
        let ids = SeqIds::new();
        let spec = agent_spec_with_tools(&ids, vec![]);
        let mut machine = default_machine(&ids, agent_state(&ids, spec));
        let opened = machine.step(StepInput::external(user_input(&ids, "hello")));
        (ids, machine, opened.requirements)
    }

    #[test]
    fn single_llm_happy_path_exposes_mode_and_origin() {
        let (_ids, _machine, batch) = open_llm_batch();
        assert_requirements(&batch)
            .count(1)
            .single_llm()
            .origin_root()
            .tag(RequirementKindTag::Llm)
            .llm_mode(LlmStepMode::NonStreaming);
    }

    #[test]
    fn single_tool_happy_path_exposes_tool_call() {
        let ids = SeqIds::new();
        let spec = agent_spec_with_tools(&ids, vec![crate::fixtures::weather_tool()]);
        let mut machine = default_machine(&ids, agent_state(&ids, spec));
        let opened = machine.step(StepInput::external(user_input(&ids, "weather?")));
        let call = tool_call("call-weather", "get_weather", json!({ "city": "SH" }));
        let folded = machine.step(StepInput::resume(RequirementResolution::new(
            opened.requirements[0].id,
            RequirementResult::Llm(Ok(assistant_tool_use(vec![call], usage(5, 2)))),
        )));

        let view = assert_requirements(&folded.requirements)
            .count(1)
            .single_tool()
            .tool_name("get_weather");
        assert_eq!(view.tool_call().id, "call-weather");
        assert!(view.request_summary().contains("get_weather"));
    }

    #[test]
    fn single_of_wrong_family_failure_message_lists_batch() {
        let (_ids, _machine, batch) = open_llm_batch();
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            assert_requirements(&batch).single_of(RequirementKindTag::Tool);
        }))
        .expect_err("asking for a missing family must panic");
        let message = panic
            .downcast_ref::<String>()
            .expect("panic payload is a String");
        assert!(
            message.contains("expected exactly one `tool` requirement, found 0"),
            "message names the missing family: {message}"
        );
        assert!(
            message.contains("llm("),
            "message shows the actual batch contents: {message}"
        );
    }
}
