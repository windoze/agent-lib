//! Provider-neutral fixtures for agent-layer tests.
//!
//! Every helper here builds data through `agent-lib`'s *public* constructors, so
//! tests never reach around a type's invariants or fabricate provider wire JSON.
//! The fixtures stay at the provider-neutral seam: they construct [`Message`],
//! [`Response`], [`ToolCall`], [`ToolResponse`], [`AgentSpec`], [`AgentState`],
//! and [`RunContext`] — never Anthropic/OpenAI bodies.
//!
//! Identity-bearing fixtures draw their ids from a [`SeqIds`] handle so a whole
//! test tree stays deterministic and globally unique (see [`crate::ids`]).

use std::num::NonZeroU32;

use agent_lib::{
    agent::{
        AgentInput, AgentSpec, AgentState, BudgetLimits, DefaultAgentMachine, LlmStepMode,
        LoopPolicy, ModelRef, RunContext, ToolFailurePolicy, ToolSetRef, WorktreeRef,
    },
    client::Response,
    conversation::{Conversation, ConversationConfig},
    model::{
        content::ContentBlock,
        message::{Message, Role},
        normalized::StopReason,
        tool::{Tool, ToolCall, ToolResponse, ToolStatus},
        usage::Usage,
    },
};
use serde_json::{Map, Value};
use std::sync::Arc;

use crate::ids::SeqIds;

/// Builds a `NonZeroU32` from a test constant, panicking on zero.
fn nz(value: u32) -> NonZeroU32 {
    NonZeroU32::new(value).expect("non-zero test value")
}

// ----- message / content fixtures -----

/// Builds a plain [`ContentBlock::Text`] with no provider extras.
#[must_use]
pub fn text_block(text: &str) -> ContentBlock {
    ContentBlock::Text {
        text: text.to_owned(),
        extra: Map::new(),
    }
}

/// Builds a single-text-block [`Role::User`] [`Message`].
#[must_use]
pub fn user_message(text: &str) -> Message {
    Message {
        role: Role::User,
        content: vec![text_block(text)],
    }
}

/// Builds a start-of-turn [`AgentInput`] from a fresh set of ids and `text`.
///
/// The turn, user-message, assistant-message, and step ids are all drawn from
/// `ids`, so repeated calls on the same tree never collide.
///
/// # Panics
///
/// Panics if the constructed message is not a `Role::User` payload, which cannot
/// happen for the [`user_message`] this helper builds.
#[must_use]
pub fn user_input(ids: &SeqIds, text: &str) -> AgentInput {
    AgentInput::user_message(
        ids.turn_id(),
        ids.message_id(),
        user_message(text),
        ids.message_id(),
        ids.step_id(),
    )
    .expect("user_message fixture is always Role::User")
}

// ----- LLM response fixtures -----

/// Builds a [`Usage`] record with only `input`/`output` populated.
#[must_use]
pub fn usage(input: u32, output: u32) -> Usage {
    Usage {
        input,
        output,
        ..Usage::default()
    }
}

/// Builds an assistant text [`Response`] that stops on `end_turn`.
#[must_use]
pub fn assistant_text(text: &str, usage: Usage) -> Response {
    Response {
        message: Message {
            role: Role::Assistant,
            content: vec![text_block(text)],
        },
        usage,
        stop_reason: StopReason::normalize("end_turn"),
        extra: Map::new(),
    }
}

/// Builds an assistant tool-use [`Response`] that stops on `tool_use`.
///
/// Each [`ToolCall`] becomes one [`ContentBlock::ToolUse`], preserving the
/// provider-assigned call id, tool name, and parsed input.
#[must_use]
pub fn assistant_tool_use(calls: Vec<ToolCall>, usage: Usage) -> Response {
    Response {
        message: Message {
            role: Role::Assistant,
            content: calls
                .into_iter()
                .map(|call| ContentBlock::ToolUse {
                    id: call.id,
                    name: call.name,
                    input: call.input,
                    extra: Map::new(),
                })
                .collect(),
        },
        usage,
        stop_reason: StopReason::normalize("tool_use"),
        extra: Map::new(),
    }
}

// ----- tool fixtures -----

/// Builds a [`ToolCall`] from a provider-assigned id, tool name, and input.
#[must_use]
pub fn tool_call(provider_id: &str, name: &str, input: Value) -> ToolCall {
    ToolCall {
        id: provider_id.to_owned(),
        name: name.to_owned(),
        input,
    }
}

/// Builds a single-text-block [`ToolResponse`] with an explicit [`ToolStatus`].
#[must_use]
pub fn tool_response(provider_call_id: &str, text: &str, status: ToolStatus) -> ToolResponse {
    ToolResponse {
        tool_call_id: provider_call_id.to_owned(),
        content: vec![text_block(text)],
        status,
        extra: Map::new(),
    }
}

/// Builds a successful ([`ToolStatus::Ok`]) [`ToolResponse`].
#[must_use]
pub fn tool_ok(provider_call_id: &str, text: &str) -> ToolResponse {
    tool_response(provider_call_id, text, ToolStatus::Ok)
}

/// Builds a failed ([`ToolStatus::Error`]) [`ToolResponse`].
#[must_use]
pub fn tool_error_response(provider_call_id: &str, text: &str) -> ToolResponse {
    tool_response(provider_call_id, text, ToolStatus::Error)
}

// ----- tool declaration fixtures -----

/// Declares a `get_weather` [`Tool`] taking a required `city` string.
#[must_use]
pub fn weather_tool() -> Tool {
    Tool {
        name: "get_weather".to_owned(),
        description: "Look up weather for a city.".to_owned(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": { "city": { "type": "string" } },
            "required": ["city"]
        }),
    }
}

/// Declares a `get_calendar` [`Tool`] taking a required `date` string.
#[must_use]
pub fn calendar_tool() -> Tool {
    Tool {
        name: "get_calendar".to_owned(),
        description: "Look up calendar events for a date.".to_owned(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": { "date": { "type": "string" } },
            "required": ["date"]
        }),
    }
}

// ----- agent fixtures -----

/// Builds an [`AgentSpec`] with no initial tools.
#[must_use]
pub fn agent_spec(ids: &SeqIds) -> AgentSpec {
    agent_spec_with_tools(ids, Vec::new())
}

/// Builds an [`AgentSpec`] whose initial tool set carries `tools`.
#[must_use]
pub fn agent_spec_with_tools(ids: &SeqIds, tools: Vec<Tool>) -> AgentSpec {
    AgentSpec::new(
        ids.agent_id(),
        WorktreeRef::new("/repo/agent-lib"),
        Some("Test agent system.".to_owned()),
        ToolSetRef::new(ids.tool_set_id(), tools),
        ModelRef::new("gpt-5.5", nz(512), Some(0.1), None),
        LoopPolicy::new(nz(8), nz(4), ToolFailurePolicy::ReturnErrorToModel),
    )
}

/// Wraps `spec` in a fresh single-conversation [`AgentState`].
#[must_use]
pub fn agent_state(ids: &SeqIds, spec: AgentSpec) -> AgentState {
    AgentState::new(
        spec,
        Conversation::new(
            ids.conversation_id(),
            ConversationConfig::new(Some("Test conversation system.".to_owned())),
        ),
    )
}

/// Builds a non-streaming [`DefaultAgentMachine`] over `state`.
///
/// Both the requirement-id and tool-execution-id sources are clones of `ids`, so
/// the machine mints framework ids from the same deterministic tree as the rest
/// of the fixtures and can therefore run a full tool round-trip.
#[must_use]
pub fn default_machine(ids: &SeqIds, state: AgentState) -> DefaultAgentMachine {
    DefaultAgentMachine::new(state, LlmStepMode::NonStreaming, Arc::new(ids.clone()))
        .with_tool_execution_ids(Arc::new(ids.clone()))
}

/// Builds a root [`RunContext`] with an unbounded budget and a readable trace
/// root drawn from `ids`.
#[must_use]
pub fn root_context(ids: &SeqIds) -> RunContext {
    RunContext::new_root(
        ids.run_id(),
        BudgetLimits::unbounded(),
        ids.trace_node("root"),
    )
}

#[cfg(test)]
mod tests {
    use super::{
        agent_spec, agent_spec_with_tools, agent_state, assistant_text, assistant_tool_use,
        calendar_tool, default_machine, root_context, tool_call, tool_error_response, tool_ok,
        usage, user_input, weather_tool,
    };
    use crate::ids::SeqIds;
    use agent_lib::{
        agent::{
            AgentInput, AgentMachine, LoopCursorKind, RequirementKind, RequirementResolution,
            RequirementResult, StepInput, ToolSetRef,
        },
        model::{
            message::Role,
            tool::{Tool, ToolStatus},
        },
    };
    use serde_json::json;

    #[test]
    fn user_input_builds_a_valid_user_message_turn() {
        let ids = SeqIds::new();
        let AgentInput::UserMessage(input) = user_input(&ids, "hello") else {
            panic!("user_input must build a UserMessage");
        };
        assert_eq!(input.message().role, Role::User);
    }

    #[test]
    fn assistant_text_response_commits_a_turn_in_the_default_machine() {
        let ids = SeqIds::new();
        let mut machine = default_machine(&ids, agent_state(&ids, agent_spec(&ids)));

        let opened = machine.step(StepInput::external(user_input(&ids, "hello")));
        assert_eq!(opened.requirements.len(), 1, "the turn parks on NeedLlm");
        let RequirementKind::NeedLlm { .. } = &opened.requirements[0].kind else {
            panic!("a text turn must first emit NeedLlm");
        };

        let resolution = RequirementResolution::new(
            opened.requirements[0].id,
            RequirementResult::Llm(Ok(assistant_text("hi", usage(3, 2)))),
        );
        let committed = machine.step(StepInput::resume(resolution));

        assert!(committed.is_quiescent());
        assert!(committed.requirements.is_empty());
        assert_eq!(machine.cursor().kind(), LoopCursorKind::Done);
    }

    #[test]
    fn assistant_tool_use_response_folds_into_a_need_tool() {
        let ids = SeqIds::new();
        let spec = agent_spec_with_tools(&ids, vec![weather_tool()]);
        let mut machine = default_machine(&ids, agent_state(&ids, spec));

        let opened = machine.step(StepInput::external(user_input(&ids, "weather?")));
        let call = tool_call("call-weather", "get_weather", json!({ "city": "Shanghai" }));
        let resolution = RequirementResolution::new(
            opened.requirements[0].id,
            RequirementResult::Llm(Ok(assistant_tool_use(vec![call], usage(5, 2)))),
        );
        let folded = machine.step(StepInput::resume(resolution));

        assert_eq!(folded.requirements.len(), 1, "the tool call is reified");
        let RequirementKind::NeedTool { call, .. } = &folded.requirements[0].kind else {
            panic!("a tool-use response must fold into a NeedTool requirement");
        };
        assert_eq!(call.name, "get_weather");
    }

    #[test]
    fn tool_responses_carry_the_expected_status() {
        assert_eq!(tool_ok("call-1", "sunny").status, ToolStatus::Ok);
        assert_eq!(
            tool_error_response("call-1", "boom").status,
            ToolStatus::Error
        );
    }

    #[test]
    fn tool_declarations_round_trip_through_a_tool_set_ref() {
        let ids = SeqIds::new();
        let tools = vec![weather_tool(), calendar_tool()];
        let set = ToolSetRef::new(ids.tool_set_id(), tools.clone());

        assert_eq!(set.tools(), tools.as_slice());

        let json = serde_json::to_string(&set).expect("serialize tool set");
        let decoded: ToolSetRef = serde_json::from_str(&json).expect("deserialize tool set");
        assert_eq!(decoded.id(), set.id());
        assert_eq!(decoded.tools(), set.tools());

        // A spec exposes the same declarations it was built with.
        let spec = agent_spec_with_tools(&ids, tools.clone());
        let declared: Vec<Tool> = spec.initial_tools().tools().to_vec();
        assert_eq!(declared, tools);
    }

    #[test]
    fn root_context_is_an_unbounded_root() {
        let ids = SeqIds::new();
        let context = root_context(&ids);
        assert_eq!(context.depth(), 0);
        assert_eq!(
            context.budget().snapshot().limits(),
            &agent_lib::agent::BudgetLimits::unbounded()
        );
    }
}
