//! A data-only scenario model draft and a runner spike (milestone 7, M7-1).
//!
//! Everything the scripted milestones expose — [`ScriptedLlmHandler`], scope
//! wiring, fixtures, and the drain loop — is Rust API. A *scenario* lifts that
//! same intent one level up into **plain data**: a serde-round-trippable value
//! that says "open this turn, script these effects, and expect this observable
//! summary", with no Rust closures, handlers, or `Arc`s in sight. That data
//! shape is the seam a future JSON runner or TS/NAPI entry point would speak
//! (see `docs/TESTABILITY.md` §8.4 / Phase 6), so this module keeps it small,
//! provider-neutral, and reuses `agent-lib`'s own stable serde enums
//! ([`Tool`], [`Role`], [`ToolStatus`], [`LoopCursorKind`]) rather than growing a
//! parallel label vocabulary.
//!
//! This is an intentionally minimal **spike**, not a stable DSL: it covers the
//! three canonical minimal turns — plain text, a tool round-trip, and a guarded
//! approval round-trip — and is deliberately narrow so the shape can still
//! change before it is committed to (`docs/TESTABILITY.md` §9.4).
//!
//! # The model
//!
//! - [`Scenario`] is the whole turn: declared [`tools`](Scenario::tools), an
//!   [`approval`](Scenario::approval) policy, the opening [`input`](Scenario::input),
//!   the [`effects`](Scenario::effects) script, and the [`expect`](Scenario::expect)
//!   observations.
//! - [`ScenarioInput`] is the external event that opens the turn (today: a user
//!   message).
//! - [`ScenarioEffectScript`] carries the per-family scripted results
//!   ([`ScenarioLlmStep`], [`ScenarioToolStep`], [`ScenarioInteractionStep`])
//!   consumed in dispatch order, mirroring the scripted handlers.
//! - [`ScenarioExpectation`] is the golden, all-optional observation set: only
//!   the fields a scenario sets are checked.
//!
//! # The runner spike
//!
//! [`run_scenario`] turns a [`Scenario`] into a [`ScenarioSummary`] by building a
//! real [`DefaultAgentMachine`] through the crate fixtures, wiring the scripted
//! handlers the data describes into a [`TestScope`], draining one turn, and
//! reading the committed conversation back. The summary is itself serde data, so
//! it suits a golden JSON comparison; [`ScenarioSummary::check`] additionally
//! diffs it against a [`ScenarioExpectation`] and returns the mismatches.
//!
//! # Summary vs. Rust assertions
//!
//! The scenario summary deliberately captures only what is **data-only and
//! stable across a serialization boundary**:
//!
//! - the committed turn count,
//! - the per-turn message role sequence,
//! - the last assistant text,
//! - the per-family call counts (llm / tool / interaction),
//! - each tool result's [`ToolStatus`] keyed by provider call id,
//! - the final [`LoopCursorKind`].
//!
//! Everything that needs live handler, trace, or timing access **stays in the
//! Rust assertion modules** and is intentionally *out* of the summary: trace-tree
//! shape and [budget](crate::assertions::BudgetAssertions) snapshots, notification
//! stream detail, out-of-order / concurrent / peak-in-flight handler behaviour,
//! [`ContentBlock`] internals and
//! provider extras, misaligned-family error injection, cancel timing and
//! panic-on-call, and requirement-id-level step-by-step
//! ([`StepHarness`](crate::harness::StepHarness)) resume/abandon assertions.

use std::fmt;
use std::sync::Arc;

use agent_lib::agent::{
    AgentError, AgentMachine, ApprovalRequirement, DefaultAgentMachine, LoopCursorKind,
    ToolApprovalPolicy, drain,
};
use agent_lib::conversation::{Conversation, ToolCallId};
use agent_lib::model::content::ContentBlock;
use agent_lib::model::message::Role;
use agent_lib::model::tool::{Tool, ToolCall, ToolStatus};
use serde::{Deserialize, Serialize};
use serde_json::Map;

use crate::fixtures::{agent_spec_with_tools, agent_state, default_machine, root_context, usage};
use crate::handlers::{
    InteractionDecision, ScriptedInteractionHandler, ScriptedLlmHandler, ScriptedToolHandler,
};
use crate::ids::SeqIds;
use crate::scope::TestScope;
use crate::script::{LlmStep, ToolStep};

// ----- the data model -----

/// A data-only description of one agent turn.
///
/// A `Scenario` is the whole unit a runner consumes: it declares the tools the
/// agent knows, the approval policy guarding them, the external input that opens
/// the turn, the scripted effect results, and the observations to assert. It is
/// serde-round-trippable, so the same value can be authored inline in Rust or
/// loaded from JSON.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    /// A stable, human-readable name carried into the [`ScenarioSummary`].
    pub name: String,
    /// An optional free-form description of what the scenario proves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The tools the agent declares for this turn (empty for a plain text turn).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<Tool>,
    /// The approval policy governing tool calls.
    #[serde(default)]
    pub approval: ApprovalPolicySpec,
    /// The external event that opens the turn.
    pub input: ScenarioInput,
    /// The scripted per-family effect results.
    #[serde(default)]
    pub effects: ScenarioEffectScript,
    /// The golden observations to assert against the run summary.
    #[serde(default)]
    pub expect: ScenarioExpectation,
}

/// The external event that opens a scenario's turn.
///
/// Modelled as an enum so it can grow additional openers (a pivot, a raw
/// external input) without breaking the single-variant JSON shape; the spike
/// only needs a user message.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ScenarioInput {
    /// A start-of-turn user message carrying `text`.
    User {
        /// The user's message text.
        text: String,
    },
}

/// The approval policy governing a scenario's tool calls.
///
/// The require-approval *policy* is a spec-level decision, not a mockable effect
/// boundary, so it is expressed as data here and materialised inside
/// [`run_scenario`] only to drive the guarded approval flow (via a runner-private
/// require-approval policy).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalPolicySpec {
    /// Every tool call runs without an approval interaction.
    #[default]
    AutoAllow,
    /// Every tool call is forced through an approval interaction.
    RequireApproval,
}

/// The scripted per-family effect results a scenario consumes in dispatch order.
///
/// Each family list mirrors the corresponding scripted handler's step queue.
/// An empty list means that family is left unwired, so any requirement in it
/// surfaces as an `UnhandledRequirement` rather than being silently served.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScenarioEffectScript {
    /// The scripted LLM generations.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub llm: Vec<ScenarioLlmStep>,
    /// The scripted tool executions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool: Vec<ScenarioToolStep>,
    /// The scripted interaction answers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub interaction: Vec<ScenarioInteractionStep>,
}

/// A scripted LLM generation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ScenarioLlmStep {
    /// An assistant text response that closes the turn.
    Text {
        /// The assistant's model-visible answer.
        text: String,
        /// The reported token usage.
        #[serde(default)]
        usage: ScenarioUsage,
    },
    /// An assistant tool-use response requesting one or more tool calls.
    ToolUse {
        /// The requested tool calls.
        calls: Vec<ScenarioToolCall>,
        /// The reported token usage.
        #[serde(default)]
        usage: ScenarioUsage,
    },
}

/// The token usage attached to a scripted LLM generation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScenarioUsage {
    /// Prompt (input) tokens.
    #[serde(default)]
    pub input: u32,
    /// Completion (output) tokens.
    #[serde(default)]
    pub output: u32,
}

/// One tool call requested by a scripted [`ScenarioLlmStep::ToolUse`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScenarioToolCall {
    /// The provider-assigned call id, shared by the request and its result.
    pub id: String,
    /// The tool name.
    pub name: String,
    /// The parsed tool input.
    #[serde(default)]
    pub input: serde_json::Value,
    /// Provider-specific fields carried on the scripted tool call.
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub extra: Map<String, serde_json::Value>,
}

/// A scripted tool execution result.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ScenarioToolStep {
    /// A successful ([`ToolStatus::Ok`]) tool result for `call_id`.
    Ok {
        /// The provider call id this result answers.
        call_id: String,
        /// The model-visible reply text.
        text: String,
    },
    /// A model-visible failed ([`ToolStatus::Error`]) tool result for `call_id`.
    Error {
        /// The provider call id this result answers.
        call_id: String,
        /// The model-visible error text.
        text: String,
    },
}

/// A scripted answer to an interaction (today: an approval).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ScenarioInteractionStep {
    /// Approve an approval interaction.
    Approve,
    /// Approve an approval interaction with a stable message.
    ApproveWith {
        /// The approval message.
        message: String,
    },
    /// Deny an approval interaction, with an optional message.
    Deny {
        /// The optional denial message.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    /// Answer a question interaction with free-form text.
    Answer {
        /// The answer text.
        text: String,
    },
    /// Select a zero-based option for a choice interaction.
    Choice {
        /// The selected option index.
        index: usize,
    },
}

/// The golden, all-optional observation set a scenario asserts.
///
/// Only the fields a scenario sets are checked by [`ScenarioSummary::check`], so
/// a scenario can assert as little or as much of the summary as it cares about.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScenarioExpectation {
    /// The expected final loop cursor (e.g. [`LoopCursorKind::Done`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<LoopCursorKind>,
    /// The expected number of committed turns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub committed_turns: Option<usize>,
    /// The expected last assistant text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_assistant_text: Option<String>,
    /// The expected number of dispatched LLM calls.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llm_calls: Option<usize>,
    /// The expected number of dispatched tool calls.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<usize>,
    /// The expected number of dispatched interaction calls.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interaction_calls: Option<usize>,
    /// The expected tool result statuses, keyed by provider call id.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_results: Vec<ToolResultExpectation>,
    /// The expected per-turn message role sequences.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub message_roles: Vec<TurnRolesExpectation>,
}

/// An expected tool result status for one provider call id.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolResultExpectation {
    /// The provider call id the result answers.
    pub call_id: String,
    /// The expected result status.
    pub status: ToolStatus,
}

/// An expected message role sequence for one committed turn.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TurnRolesExpectation {
    /// The zero-based committed turn index.
    pub turn: usize,
    /// The expected message roles, in order.
    pub roles: Vec<Role>,
}

// ----- the runner spike -----

/// The observable, data-only result of running a [`Scenario`].
///
/// Every field is serde data, so a summary can be golden-compared as JSON.
/// [`check`](Self::check) diffs it against a [`ScenarioExpectation`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScenarioSummary {
    /// The scenario name, echoed back for identification.
    pub name: String,
    /// The final loop cursor after the turn drained.
    pub cursor: LoopCursorKind,
    /// The number of committed turns.
    pub committed_turns: usize,
    /// The last assistant text, or `None` if the turn produced no assistant text.
    pub last_assistant_text: Option<String>,
    /// The number of dispatched LLM calls.
    pub llm_calls: usize,
    /// The number of dispatched tool calls.
    pub tool_calls: usize,
    /// The number of dispatched interaction calls.
    pub interaction_calls: usize,
    /// Every tool result observed, in conversation order.
    pub tool_results: Vec<ToolResultObservation>,
    /// The per-committed-turn message role sequences.
    pub message_roles: Vec<Vec<Role>>,
}

/// One observed tool result: a provider call id and its final status.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolResultObservation {
    /// The provider call id the result answered.
    pub call_id: String,
    /// The observed result status.
    pub status: ToolStatus,
}

impl ScenarioSummary {
    /// Diffs this summary against `expected`, returning one message per mismatch.
    ///
    /// Only the fields `expected` sets are checked; an empty return value means
    /// every asserted field matched. The messages are stable, self-describing
    /// strings so a test can surface them directly.
    #[must_use]
    pub fn check(&self, expected: &ScenarioExpectation) -> Vec<String> {
        let mut mismatches = Vec::new();

        if let Some(cursor) = expected.cursor
            && cursor != self.cursor
        {
            mismatches.push(format!(
                "cursor: expected {cursor:?}, found {:?}",
                self.cursor
            ));
        }
        if let Some(turns) = expected.committed_turns
            && turns != self.committed_turns
        {
            mismatches.push(format!(
                "committed_turns: expected {turns}, found {}",
                self.committed_turns
            ));
        }
        if let Some(text) = &expected.last_assistant_text
            && Some(text) != self.last_assistant_text.as_ref()
        {
            mismatches.push(format!(
                "last_assistant_text: expected {text:?}, found {:?}",
                self.last_assistant_text
            ));
        }
        if let Some(count) = expected.llm_calls
            && count != self.llm_calls
        {
            mismatches.push(format!(
                "llm_calls: expected {count}, found {}",
                self.llm_calls
            ));
        }
        if let Some(count) = expected.tool_calls
            && count != self.tool_calls
        {
            mismatches.push(format!(
                "tool_calls: expected {count}, found {}",
                self.tool_calls
            ));
        }
        if let Some(count) = expected.interaction_calls
            && count != self.interaction_calls
        {
            mismatches.push(format!(
                "interaction_calls: expected {count}, found {}",
                self.interaction_calls
            ));
        }
        for want in &expected.tool_results {
            match self
                .tool_results
                .iter()
                .find(|got| got.call_id == want.call_id)
            {
                Some(got) if got.status == want.status => {}
                Some(got) => mismatches.push(format!(
                    "tool_results[{}]: expected {:?}, found {:?}",
                    want.call_id, want.status, got.status
                )),
                None => mismatches.push(format!(
                    "tool_results[{}]: expected {:?}, but no result was observed",
                    want.call_id, want.status
                )),
            }
        }
        for want in &expected.message_roles {
            match self.message_roles.get(want.turn) {
                Some(got) if *got == want.roles => {}
                Some(got) => mismatches.push(format!(
                    "message_roles[turn {}]: expected {:?}, found {:?}",
                    want.turn, want.roles, got
                )),
                None => mismatches.push(format!(
                    "message_roles[turn {}]: expected {:?}, but the turn does not exist",
                    want.turn, want.roles
                )),
            }
        }

        mismatches
    }
}

/// A failure raised while running a [`Scenario`].
#[derive(Debug)]
pub enum ScenarioError {
    /// The underlying [`drain`] returned a classified [`AgentError`].
    Drain(AgentError),
}

impl fmt::Display for ScenarioError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Drain(error) => write!(formatter, "scenario drain failed: {error}"),
        }
    }
}

impl std::error::Error for ScenarioError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Drain(error) => Some(error),
        }
    }
}

/// Approval policy that guards every tool call, forcing a `NeedInteraction`.
///
/// A require-approval policy is a spec-level decision rather than a mockable
/// effect boundary, so `agent-testkit` does not ship one as reusable API. This
/// private policy exists only so the runner spike can turn an
/// [`ApprovalPolicySpec::RequireApproval`] datum into a running guarded machine.
#[derive(Debug)]
struct ScenarioApprovalPolicy;

impl ToolApprovalPolicy for ScenarioApprovalPolicy {
    fn approval_requirement(&self, _call_id: ToolCallId, _call: &ToolCall) -> ApprovalRequirement {
        ApprovalRequirement::required(Some("human approval required".to_owned()))
    }
}

/// Runs `scenario` end-to-end and returns its observable [`ScenarioSummary`].
///
/// The runner builds a real [`DefaultAgentMachine`] over the crate fixtures,
/// wires only the effect families the [`effects`](Scenario::effects) script
/// populates into a [`TestScope`] (so an unscripted family surfaces as an
/// `UnhandledRequirement` rather than being silently served), drains a single
/// turn, and reads the committed conversation and handler call logs back into a
/// summary.
///
/// # Errors
///
/// Returns [`ScenarioError::Drain`] carrying the classified [`AgentError`] if the
/// turn fails to drain to completion.
pub async fn run_scenario(scenario: &Scenario) -> Result<ScenarioSummary, ScenarioError> {
    let ids = SeqIds::new();
    let ctx = root_context(&ids);
    let spec = agent_spec_with_tools(&ids, scenario.tools.clone());
    let mut machine = default_machine(&ids, agent_state(&ids, spec));
    if scenario.approval == ApprovalPolicySpec::RequireApproval {
        machine = machine.with_approval_policy(Arc::new(ScenarioApprovalPolicy));
    }

    let mut builder = TestScope::builder();

    let mut llm_log = None;
    if !scenario.effects.llm.is_empty() {
        let handler = Arc::new(ScriptedLlmHandler::from_steps(
            scenario.effects.llm.iter().map(llm_step),
        ));
        llm_log = Some(Arc::clone(handler.log()));
        builder = builder.llm(handler);
    }

    let mut tool_log = None;
    if !scenario.effects.tool.is_empty() {
        let handler = Arc::new(ScriptedToolHandler::from_steps(
            scenario.effects.tool.iter().map(tool_step),
        ));
        tool_log = Some(Arc::clone(handler.log()));
        builder = builder.tool(handler);
    }

    let mut interaction_log = None;
    if !scenario.effects.interaction.is_empty() {
        let handler = Arc::new(ScriptedInteractionHandler::sequence(
            scenario
                .effects
                .interaction
                .iter()
                .map(interaction_decision),
        ));
        interaction_log = Some(Arc::clone(handler.log()));
        builder = builder.attended(handler);
    }

    let scope = builder.build();

    let input = match &scenario.input {
        ScenarioInput::User { text } => crate::fixtures::user_input(&ids, text),
    };

    drain(&mut machine, input, &scope, None, &ctx)
        .await
        .map_err(ScenarioError::Drain)?;

    Ok(summarize(
        &scenario.name,
        &machine,
        llm_log.map_or(0, |log| log.len()),
        tool_log.map_or(0, |log| log.len()),
        interaction_log.map_or(0, |log| log.len()),
    ))
}

/// Maps a scenario LLM step onto a scripted [`LlmStep`].
fn llm_step(step: &ScenarioLlmStep) -> LlmStep {
    match step {
        ScenarioLlmStep::Text { text, usage: u } => {
            LlmStep::text(text).with_usage(usage(u.input, u.output))
        }
        ScenarioLlmStep::ToolUse { calls, usage: u } => {
            let calls = calls
                .iter()
                .map(|call| ToolCall {
                    id: call.id.clone(),
                    name: call.name.clone(),
                    input: call.input.clone(),
                    extra: call.extra.clone(),
                })
                .collect();
            LlmStep::tool_use(calls).with_usage(usage(u.input, u.output))
        }
    }
}

/// Maps a scenario tool step onto a scripted [`ToolStep`].
fn tool_step(step: &ScenarioToolStep) -> ToolStep {
    match step {
        ScenarioToolStep::Ok { call_id, text } => ToolStep::ok(call_id, text),
        ScenarioToolStep::Error { call_id, text } => ToolStep::error(call_id, text),
    }
}

/// Maps a scenario interaction step onto an [`InteractionDecision`].
fn interaction_decision(step: &ScenarioInteractionStep) -> InteractionDecision {
    match step {
        ScenarioInteractionStep::Approve => InteractionDecision::Approve,
        ScenarioInteractionStep::ApproveWith { message } => {
            InteractionDecision::ApproveWith(message.clone())
        }
        ScenarioInteractionStep::Deny { message } => InteractionDecision::Deny(message.clone()),
        ScenarioInteractionStep::Answer { text } => InteractionDecision::Answer(text.clone()),
        ScenarioInteractionStep::Choice { index } => InteractionDecision::Choice(*index),
    }
}

/// Reads the drained machine and call counts into a [`ScenarioSummary`].
fn summarize(
    name: &str,
    machine: &DefaultAgentMachine,
    llm_calls: usize,
    tool_calls: usize,
    interaction_calls: usize,
) -> ScenarioSummary {
    let conversation = machine.state().conversation();
    ScenarioSummary {
        name: name.to_owned(),
        cursor: machine.cursor().kind(),
        committed_turns: conversation.turns().len(),
        last_assistant_text: last_assistant_text(conversation),
        llm_calls,
        tool_calls,
        interaction_calls,
        tool_results: tool_results(conversation),
        message_roles: message_roles(conversation),
    }
}

/// Concatenates the text of the last committed assistant message, if any.
fn last_assistant_text(conversation: &Conversation) -> Option<String> {
    for turn in conversation.turns().iter().rev() {
        for message in turn.messages().iter().rev() {
            if message.payload().role == Role::Assistant {
                let mut text = String::new();
                for block in &message.payload().content {
                    if let ContentBlock::Text { text: value, .. } = block {
                        text.push_str(value);
                    }
                }
                return Some(text);
            }
        }
    }
    None
}

/// Collects every committed tool result, in conversation order.
fn tool_results(conversation: &Conversation) -> Vec<ToolResultObservation> {
    let mut results = Vec::new();
    for turn in conversation.turns() {
        for message in turn.messages() {
            for block in &message.payload().content {
                if let ContentBlock::ToolResult {
                    tool_use_id,
                    status,
                    ..
                } = block
                {
                    results.push(ToolResultObservation {
                        call_id: tool_use_id.clone(),
                        status: *status,
                    });
                }
            }
        }
    }
    results
}

/// Collects the per-committed-turn message role sequences.
fn message_roles(conversation: &Conversation) -> Vec<Vec<Role>> {
    conversation
        .turns()
        .iter()
        .map(|turn| {
            turn.messages()
                .iter()
                .map(|message| message.payload().role)
                .collect()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        ApprovalPolicySpec, Scenario, ScenarioEffectScript, ScenarioExpectation, ScenarioInput,
        ScenarioInteractionStep, ScenarioLlmStep, ScenarioToolCall, ScenarioToolStep,
        ScenarioUsage, ToolResultExpectation, TurnRolesExpectation, run_scenario,
    };
    use crate::fixtures::weather_tool;
    use agent_lib::agent::LoopCursorKind;
    use agent_lib::model::message::Role;
    use agent_lib::model::tool::ToolStatus;
    use serde_json::Map;

    const CALL_ID: &str = "call-weather";

    /// A plain `user -> LLM text -> commit` scenario.
    fn text_scenario() -> Scenario {
        Scenario {
            name: "text_turn".to_owned(),
            description: Some("user -> LLM text -> commit".to_owned()),
            tools: Vec::new(),
            approval: ApprovalPolicySpec::AutoAllow,
            input: ScenarioInput::User {
                text: "hello?".to_owned(),
            },
            effects: ScenarioEffectScript {
                llm: vec![ScenarioLlmStep::Text {
                    text: "Hello from the model.".to_owned(),
                    usage: ScenarioUsage {
                        input: 4,
                        output: 3,
                    },
                }],
                ..Default::default()
            },
            expect: ScenarioExpectation {
                cursor: Some(LoopCursorKind::Done),
                committed_turns: Some(1),
                last_assistant_text: Some("Hello from the model.".to_owned()),
                llm_calls: Some(1),
                tool_calls: Some(0),
                interaction_calls: Some(0),
                message_roles: vec![TurnRolesExpectation {
                    turn: 0,
                    roles: vec![Role::User, Role::Assistant],
                }],
                ..Default::default()
            },
        }
    }

    /// A `user -> tool_use -> tool error -> recovery text` scenario.
    fn tool_scenario() -> Scenario {
        Scenario {
            name: "tool_error_turn".to_owned(),
            description: None,
            tools: vec![weather_tool()],
            approval: ApprovalPolicySpec::AutoAllow,
            input: ScenarioInput::User {
                text: "weather?".to_owned(),
            },
            effects: ScenarioEffectScript {
                llm: vec![
                    ScenarioLlmStep::ToolUse {
                        calls: vec![ScenarioToolCall {
                            id: CALL_ID.to_owned(),
                            name: "get_weather".to_owned(),
                            input: serde_json::json!({ "city": "Shanghai" }),
                            extra: Map::new(),
                        }],
                        usage: ScenarioUsage {
                            input: 5,
                            output: 2,
                        },
                    },
                    ScenarioLlmStep::Text {
                        text: "Sorry, the weather service is down.".to_owned(),
                        usage: ScenarioUsage {
                            input: 6,
                            output: 4,
                        },
                    },
                ],
                tool: vec![ScenarioToolStep::Error {
                    call_id: CALL_ID.to_owned(),
                    text: "weather backend unavailable".to_owned(),
                }],
                ..Default::default()
            },
            expect: ScenarioExpectation {
                cursor: Some(LoopCursorKind::Done),
                committed_turns: Some(1),
                last_assistant_text: Some("Sorry, the weather service is down.".to_owned()),
                llm_calls: Some(2),
                tool_calls: Some(1),
                interaction_calls: Some(0),
                tool_results: vec![ToolResultExpectation {
                    call_id: CALL_ID.to_owned(),
                    status: ToolStatus::Error,
                }],
                message_roles: vec![TurnRolesExpectation {
                    turn: 0,
                    roles: vec![Role::User, Role::Assistant, Role::Tool, Role::Assistant],
                }],
            },
        }
    }

    /// A guarded `user -> tool_use -> approval -> tool -> final text` scenario.
    fn approval_scenario() -> Scenario {
        Scenario {
            name: "approval_turn".to_owned(),
            description: None,
            tools: vec![weather_tool()],
            approval: ApprovalPolicySpec::RequireApproval,
            input: ScenarioInput::User {
                text: "weather?".to_owned(),
            },
            effects: ScenarioEffectScript {
                llm: vec![
                    ScenarioLlmStep::ToolUse {
                        calls: vec![ScenarioToolCall {
                            id: CALL_ID.to_owned(),
                            name: "get_weather".to_owned(),
                            input: serde_json::json!({ "city": "Shanghai" }),
                            extra: Map::new(),
                        }],
                        usage: ScenarioUsage::default(),
                    },
                    ScenarioLlmStep::Text {
                        text: "It is sunny in Shanghai.".to_owned(),
                        usage: ScenarioUsage::default(),
                    },
                ],
                tool: vec![ScenarioToolStep::Ok {
                    call_id: CALL_ID.to_owned(),
                    text: "Sunny, 20C.".to_owned(),
                }],
                interaction: vec![ScenarioInteractionStep::Approve],
            },
            expect: ScenarioExpectation {
                cursor: Some(LoopCursorKind::Done),
                committed_turns: Some(1),
                last_assistant_text: Some("It is sunny in Shanghai.".to_owned()),
                llm_calls: Some(2),
                tool_calls: Some(1),
                interaction_calls: Some(1),
                tool_results: vec![ToolResultExpectation {
                    call_id: CALL_ID.to_owned(),
                    status: ToolStatus::Ok,
                }],
                message_roles: vec![TurnRolesExpectation {
                    turn: 0,
                    roles: vec![Role::User, Role::Assistant, Role::Tool, Role::Assistant],
                }],
            },
        }
    }

    #[test]
    fn scenario_round_trips_through_json() {
        for scenario in [text_scenario(), tool_scenario(), approval_scenario()] {
            let json = serde_json::to_string_pretty(&scenario).expect("scenario serializes");
            let decoded: Scenario = serde_json::from_str(&json).expect("scenario deserializes");
            assert_eq!(decoded, scenario, "scenario survives a JSON round-trip");
        }
    }

    #[test]
    fn summary_round_trips_and_check_is_empty_on_match() {
        // A summary is itself serde data suitable for a golden comparison.
        let scenario = text_scenario();
        let summary = super::ScenarioSummary {
            name: scenario.name.clone(),
            cursor: LoopCursorKind::Done,
            committed_turns: 1,
            last_assistant_text: Some("Hello from the model.".to_owned()),
            llm_calls: 1,
            tool_calls: 0,
            interaction_calls: 0,
            tool_results: Vec::new(),
            message_roles: vec![vec![Role::User, Role::Assistant]],
        };
        let json = serde_json::to_string(&summary).expect("summary serializes");
        let decoded: super::ScenarioSummary =
            serde_json::from_str(&json).expect("summary deserializes");
        assert_eq!(decoded, summary);
        assert!(summary.check(&scenario.expect).is_empty());
    }

    #[tokio::test]
    async fn text_scenario_runs_and_matches_expectation() {
        let scenario = text_scenario();
        let summary = run_scenario(&scenario).await.expect("text scenario runs");

        assert_eq!(
            summary.check(&scenario.expect),
            Vec::<String>::new(),
            "the observed summary matches every asserted field"
        );
        assert_eq!(summary.cursor, LoopCursorKind::Done);
        assert_eq!(summary.llm_calls, 1);
        assert_eq!(summary.tool_calls, 0);
        assert!(summary.tool_results.is_empty());
    }

    #[tokio::test]
    async fn tool_scenario_runs_and_records_the_error_result() {
        let scenario = tool_scenario();
        let summary = run_scenario(&scenario).await.expect("tool scenario runs");

        assert_eq!(summary.check(&scenario.expect), Vec::<String>::new());
        assert_eq!(summary.tool_results.len(), 1);
        assert_eq!(summary.tool_results[0].call_id, CALL_ID);
        assert_eq!(summary.tool_results[0].status, ToolStatus::Error);
        assert_eq!(summary.llm_calls, 2);
        assert_eq!(summary.tool_calls, 1);
    }

    #[tokio::test]
    async fn approval_scenario_runs_the_guarded_tool_after_approval() {
        let scenario = approval_scenario();
        let summary = run_scenario(&scenario)
            .await
            .expect("approval scenario runs");

        assert_eq!(summary.check(&scenario.expect), Vec::<String>::new());
        assert_eq!(summary.interaction_calls, 1);
        assert_eq!(summary.tool_results.len(), 1);
        assert_eq!(summary.tool_results[0].status, ToolStatus::Ok);
    }

    #[tokio::test]
    async fn scenario_loaded_from_json_runs_the_same_way() {
        // Proves the data-only path: author as JSON, parse, run, assert.
        let scenario = tool_scenario();
        let json = serde_json::to_string(&scenario).expect("serialize");
        let loaded: Scenario = serde_json::from_str(&json).expect("parse");

        let summary = run_scenario(&loaded).await.expect("loaded scenario runs");
        assert_eq!(summary.check(&loaded.expect), Vec::<String>::new());
    }

    #[tokio::test]
    async fn check_reports_mismatches_against_a_wrong_expectation() {
        let scenario = text_scenario();
        let summary = run_scenario(&scenario).await.expect("text scenario runs");

        let wrong = ScenarioExpectation {
            committed_turns: Some(2),
            last_assistant_text: Some("nope".to_owned()),
            ..Default::default()
        };
        let mismatches = summary.check(&wrong);
        assert_eq!(mismatches.len(), 2, "both wrong fields are reported");
        assert!(mismatches.iter().any(|m| m.contains("committed_turns")));
        assert!(mismatches.iter().any(|m| m.contains("last_assistant_text")));
    }

    #[test]
    fn interaction_steps_map_onto_every_decision() {
        // Guards the data-to-decision mapping without needing a running turn.
        use super::interaction_decision;
        use crate::handlers::InteractionDecision;

        assert!(matches!(
            interaction_decision(&ScenarioInteractionStep::Approve),
            InteractionDecision::Approve
        ));
        assert!(matches!(
            interaction_decision(&ScenarioInteractionStep::ApproveWith {
                message: "ok".to_owned()
            }),
            InteractionDecision::ApproveWith(_)
        ));
        assert!(matches!(
            interaction_decision(&ScenarioInteractionStep::Deny { message: None }),
            InteractionDecision::Deny(None)
        ));
        assert!(matches!(
            interaction_decision(&ScenarioInteractionStep::Answer {
                text: "hi".to_owned()
            }),
            InteractionDecision::Answer(_)
        ));
        assert!(matches!(
            interaction_decision(&ScenarioInteractionStep::Choice { index: 2 }),
            InteractionDecision::Choice(2)
        ));
    }
}
