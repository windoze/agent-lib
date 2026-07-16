//! End-to-end interactive agent: real LLM provider + a mocked, approval-gated
//! tool, driven by a hand-built [`HandlerScope`].
//!
//! This example shows how the sans-io [`DefaultAgentMachine`] and the effect
//! model fit together against a *live* endpoint:
//!
//! - The LLM is a real provider selected by `AGENT_LIB_PROVIDER` (see the shared
//!   `support` module / README for the endpoint variables).
//! - `get_weather` is a demo tool whose result is mocked (a pseudo-random
//!   temperature and condition), so no external weather API is needed.
//! - Every `get_weather` call requires approval, demonstrating the
//!   `NeedInteraction` return path: the machine pauses and a line-based
//!   [`InteractionHandler`] asks you to allow or deny the call on stdin.
//! - Type messages at the `you>` prompt for a normal chat turn. `/quit` exits
//!   and prints the session token usage summed across every committed turn.
//!
//! Unlike the `ReferenceScope` convenience, this wires its own
//! [`HandlerScope`] (LLM + tool + a custom interaction handler) and drives the
//! machine with [`drain`], because the reference scope only accepts a fixed
//! approval decision rather than an interactive backend.

mod support;

use agent_lib::agent::{
    AgentId, AgentInput, AgentSpec, AgentState, ApprovalDecision, ApprovalRequirement,
    ApprovalResponse, BudgetLimits, DefaultAgentMachine, HandlerScope, Interaction,
    InteractionHandler, InteractionKind, InteractionResponse, LlmClientHandler, LlmHandler,
    LlmStepMode, LoopCursor, LoopPolicy, ModelRef, RequirementError, RequirementId, RequirementIds,
    RequirementKindTag, RequirementResult, RunContext, RunId, StepId, ToolApprovalPolicy,
    ToolExecutionIds, ToolFailurePolicy, ToolHandler, ToolRegistry, ToolRegistryHandler,
    ToolRuntimeError, ToolSetId, ToolSetRef, TraceNodeId, WorktreeRef, drain,
};
use agent_lib::client::LlmClient;
use agent_lib::conversation::{
    Conversation, ConversationConfig, ConversationId, MessageId, ToolCallId, TurnId,
};
use agent_lib::model::{
    content::ContentBlock,
    message::Role,
    tool::{Tool, ToolCall, ToolResponse, ToolStatus},
    usage::Usage,
};
use async_trait::async_trait;
use serde_json::{Map, json};
use std::io::{self, Write};
use std::num::NonZeroU32;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use support::{ExampleResult, configured_target, text_message};

/// Caller-supplied identity source.
///
/// The library never mints ids itself; a host supplies them through
/// [`RequirementIds`] and [`ToolExecutionIds`]. This demo hands out globally
/// unique ids from a single monotonic counter (starting at 1 so no id is nil).
#[derive(Debug, Clone)]
struct DemoIds {
    counter: Arc<AtomicU64>,
}

impl DemoIds {
    fn new() -> Self {
        Self {
            counter: Arc::new(AtomicU64::new(1)),
        }
    }

    fn next_uuid(&self) -> uuid::Uuid {
        uuid::Uuid::from_u128(u128::from(self.counter.fetch_add(1, Ordering::SeqCst)))
    }

    fn agent_id(&self) -> AgentId {
        AgentId::new(self.next_uuid())
    }
    fn run_id(&self) -> RunId {
        RunId::new(self.next_uuid())
    }
    fn tool_set_id(&self) -> ToolSetId {
        ToolSetId::new(self.next_uuid())
    }
    fn conversation_id(&self) -> ConversationId {
        ConversationId::new(self.next_uuid())
    }
    fn turn_id(&self) -> TurnId {
        TurnId::new(self.next_uuid())
    }
    fn message_id(&self) -> MessageId {
        MessageId::new(self.next_uuid())
    }
    fn step_id(&self) -> StepId {
        StepId::new(self.next_uuid())
    }
    fn trace_root(&self) -> TraceNodeId {
        TraceNodeId::new("chat-root".to_owned())
    }
}

impl RequirementIds for DemoIds {
    fn next_requirement_id(
        &self,
        _kind_tag: RequirementKindTag,
    ) -> Result<RequirementId, RequirementError> {
        Ok(RequirementId::new(self.next_uuid()))
    }
}

impl ToolExecutionIds for DemoIds {
    fn tool_call_id(&self, _call: &ToolCall) -> Result<ToolCallId, ToolRuntimeError> {
        Ok(ToolCallId::new(self.next_uuid()))
    }

    fn tool_result_message_id(
        &self,
        _call_id: ToolCallId,
        _call: &ToolCall,
    ) -> Result<MessageId, ToolRuntimeError> {
        Ok(MessageId::new(self.next_uuid()))
    }

    fn next_assistant_message_id(&self) -> Result<MessageId, ToolRuntimeError> {
        Ok(MessageId::new(self.next_uuid()))
    }

    fn next_step_id(&self) -> Result<StepId, ToolRuntimeError> {
        Ok(StepId::new(self.next_uuid()))
    }
}

/// A registry exposing a single mocked `get_weather` tool.
#[derive(Debug)]
struct WeatherRegistry;

impl WeatherRegistry {
    fn declaration() -> Tool {
        Tool {
            name: "get_weather".to_owned(),
            description: "Look up the current weather for a city.".to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": { "city": { "type": "string" } },
                "required": ["city"]
            }),
        }
    }
}

#[async_trait]
impl ToolRegistry for WeatherRegistry {
    fn declarations(&self) -> Vec<Tool> {
        vec![Self::declaration()]
    }

    async fn execute(
        &self,
        _call_id: ToolCallId,
        call: ToolCall,
    ) -> Result<ToolResponse, ToolRuntimeError> {
        if call.name != "get_weather" {
            return Err(ToolRuntimeError::UnknownTool { name: call.name });
        }
        let city = call
            .input
            .get("city")
            .and_then(|value| value.as_str())
            .unwrap_or("unknown");

        // Mock the result with a pseudo-random temperature and condition derived
        // from the wall clock; no external weather API is contacted.
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let conditions = ["sunny", "cloudy", "rainy", "windy", "foggy"];
        let condition = conditions[nanos as usize % conditions.len()];
        let temperature = 5 + (nanos % 30); // 5..=34 °C

        let text = format!("Weather in {city}: {condition}, {temperature}°C.");
        Ok(ToolResponse {
            tool_call_id: call.id,
            content: vec![ContentBlock::Text {
                text,
                extra: Map::new(),
            }],
            status: ToolStatus::Ok,
            extra: Map::new(),
        })
    }
}

/// Approval policy that gates every tool call behind an interaction.
///
/// The reason string carries the tool name and arguments so the interaction
/// handler can show them (an [`Interaction`] only names the framework call id).
#[derive(Debug)]
struct RequireApproval;

impl ToolApprovalPolicy for RequireApproval {
    fn approval_requirement(&self, _call_id: ToolCallId, call: &ToolCall) -> ApprovalRequirement {
        ApprovalRequirement::required(Some(format!(
            "run tool `{}` with {}",
            call.name, call.input
        )))
    }
}

/// A line-based interaction backend: asks the operator to allow or deny each
/// approval on stdin.
#[derive(Debug)]
struct StdinApproval;

#[async_trait]
impl InteractionHandler for StdinApproval {
    async fn fulfill(&self, request: &Interaction, _ctx: &RunContext) -> RequirementResult {
        let response = match request.kind() {
            InteractionKind::Approval {
                call_id,
                requirement,
            } => {
                let reason = requirement.reason().unwrap_or("execute a tool");
                println!("\n[approval] the agent wants to {reason}");
                print!("allow? [y/N] ");
                let _ = io::stdout().flush();

                let mut line = String::new();
                let _ = io::stdin().read_line(&mut line);
                let decision = if matches!(line.trim(), "y" | "Y" | "yes") {
                    ApprovalDecision::Approve
                } else {
                    ApprovalDecision::Deny
                };
                println!("[approval] {decision:?}");

                InteractionResponse::Approval(ApprovalResponse::new(
                    request.step_id(),
                    *call_id,
                    decision,
                    None,
                ))
            }
            // The default machine never emits open questions/choices/permissions.
            InteractionKind::Question { .. } => InteractionResponse::answer(String::new()),
            InteractionKind::Choice { .. } => InteractionResponse::Choice(0),
            InteractionKind::Permission { .. } => {
                panic!("the default machine never emits permission interactions")
            }
        };
        RequirementResult::Interaction(response)
    }
}

/// One drain layer wiring a live LLM client, the weather registry, and the
/// stdin approval backend into a total scope.
struct ChatScope {
    llm: LlmClientHandler,
    tool: ToolRegistryHandler,
    interaction: StdinApproval,
}

impl HandlerScope for ChatScope {
    fn llm(&self) -> Option<&dyn LlmHandler> {
        Some(&self.llm)
    }
    fn tool(&self) -> Option<&dyn ToolHandler> {
        Some(&self.tool)
    }
    fn interaction(&self) -> Option<&dyn InteractionHandler> {
        Some(&self.interaction)
    }
}

/// Concatenates the final assistant text of the most recently committed turn.
fn last_assistant_text(conversation: &Conversation) -> String {
    let Some(turn) = conversation.turns().last() else {
        return String::new();
    };
    let Some(message) = turn.messages().last() else {
        return String::new();
    };
    message
        .payload()
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Sums token usage across every committed turn.
fn total_usage(conversation: &Conversation) -> Usage {
    let mut total = Usage::default();
    for turn in conversation.turns() {
        let usage = turn.meta().usage();
        total.input += usage.input;
        total.output += usage.output;
        total.cache_read += usage.cache_read;
        total.cache_write += usage.cache_write;
        total.reasoning += usage.reasoning;
    }
    total
}

#[tokio::main]
async fn main() -> ExampleResult<()> {
    let target = configured_target()?;
    let model = target.model.clone();
    let label = target.label;
    let client: Arc<dyn LlmClient> = Arc::from(target.client);

    let ids = Arc::new(DemoIds::new());
    let registry: Arc<dyn ToolRegistry> = Arc::new(WeatherRegistry);

    // Static configuration: worktree, system prompt, tool set, model, loop policy.
    let spec = AgentSpec::new(
        ids.agent_id(),
        WorktreeRef::new("."),
        Some(
            "You are a concise weather assistant. Use the get_weather tool when \
             the user asks about the weather."
                .to_owned(),
        ),
        ToolSetRef::new(ids.tool_set_id(), registry.declarations()),
        ModelRef::new(model, NonZeroU32::new(512).unwrap(), None, None),
        LoopPolicy::new(
            NonZeroU32::new(8).unwrap(),
            NonZeroU32::new(4).unwrap(),
            ToolFailurePolicy::ReturnErrorToModel,
        ),
    );

    // State holds the single active Conversation; the machine is sans-io and
    // reuses one AgentState across every turn of the chat.
    let state = AgentState::new(
        spec,
        Conversation::new(ids.conversation_id(), ConversationConfig::new(None)),
    );
    let mut machine = DefaultAgentMachine::new(state, LlmStepMode::NonStreaming, ids.clone())
        .with_tool_execution_ids(ids.clone())
        .with_approval_policy(Arc::new(RequireApproval));

    let scope = ChatScope {
        llm: LlmClientHandler::new(client),
        tool: ToolRegistryHandler::new(registry),
        interaction: StdinApproval,
    };
    let ctx = RunContext::new_root(ids.run_id(), BudgetLimits::unbounded(), ids.trace_root());

    eprintln!("chatting through {label}. ask about the weather; type /quit to exit.");

    loop {
        print!("\nyou> ");
        io::stdout().flush()?;

        let mut line = String::new();
        if io::stdin().read_line(&mut line)? == 0 {
            break; // EOF (Ctrl-D)
        }
        let text = line.trim();
        if text.is_empty() {
            continue;
        }
        if text == "/quit" {
            break;
        }

        let input = AgentInput::user_message(
            ids.turn_id(),
            ids.message_id(),
            text_message(Role::User, text),
            ids.message_id(),
            ids.step_id(),
        )?;

        let done = drain(&mut machine, input, &scope, None, &ctx).await?;
        match done.cursor() {
            LoopCursor::Error(error) => eprintln!("agent error: {}", error.message()),
            _ => {
                let reply = last_assistant_text(machine.state().conversation());
                println!("bot> {reply}");
            }
        }
    }

    let usage = total_usage(machine.state().conversation());
    println!("\n── session token usage ──");
    println!("input tokens:     {}", usage.input);
    println!("output tokens:    {}", usage.output);
    if usage.cache_read > 0 {
        println!("cache-read tokens: {}", usage.cache_read);
    }
    if usage.reasoning > 0 {
        println!("reasoning tokens: {}", usage.reasoning);
    }
    println!("total (in+out):   {}", usage.input + usage.output);
    Ok(())
}
