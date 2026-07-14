//! Scripted effect handlers implementing `agent-lib`'s public handler traits.
//!
//! These handlers fulfil [`Requirement`](agent_lib::agent::Requirement) values
//! by *directly* implementing [`LlmHandler`], [`ToolHandler`],
//! [`InteractionHandler`], and [`ReconfigHandler`] — never by mocking an
//! [`LlmClient`](agent_lib::client::LlmClient) or a provider HTTP/SSE wire
//! format. Each one wraps the scripted primitives from [`crate::script`]:
//!
//! - [`ScriptedLlmHandler`], [`ScriptedToolHandler`], and
//!   [`ScriptedReconfigHandler`] drive a [`Script`] of their family's steps and
//!   record every call in an observable [`CallLog`]. When the script is drained
//!   under [`StrictMode::Error`] the
//!   exhaustion is folded into a *family-aligned* failure
//!   ([`RequirementResult::Llm(Err(..))`](RequirementResult::Llm) and friends),
//!   never a wrong-family result.
//! - [`ScriptedInteractionHandler`] answers interactions reactively — the
//!   [`InteractionResponse`] family carries no `Err`, and an approval response
//!   must address the live request's `step_id`/`call_id`, so it reacts to each
//!   request with an [`InteractionDecision`] instead of replaying a pre-built
//!   response queue. It offers [`approve_all`](ScriptedInteractionHandler::approve_all),
//!   [`deny_all`](ScriptedInteractionHandler::deny_all), and an ordered
//!   [`sequence`](ScriptedInteractionHandler::sequence) of decisions.
//! - [`ScriptedToolRegistry`] is the [`ToolRegistry`] variant, for tests that
//!   route a `NeedTool` through the reference
//!   [`ToolRegistryHandler`](agent_lib::agent::ToolRegistryHandler) rather than a
//!   bare [`ToolHandler`].
//! - [`MisalignedHandler`] is a deliberately wrong-family handler used to prove
//!   the driver's [`RequirementKind::accepts`](agent_lib::agent::RequirementKind::accepts)
//!   check rejects a misaligned result.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use agent_lib::agent::{
    ApprovalDecision, ApprovalResponse, Interaction, InteractionHandler, InteractionKind,
    InteractionResponse, LlmHandler, LlmStepMode, ReconfigHandler, RequirementResult, RunContext,
    ToolHandler, ToolRegistry, ToolRuntimeError, ToolSetRef,
};
use agent_lib::client::{ChatRequest, ClientError};
use agent_lib::conversation::ToolCallId;
use agent_lib::model::tool::{Tool, ToolCall, ToolResponse};
use async_trait::async_trait;

use crate::script::{CallLog, LlmStep, ReconfigStep, Script, ScriptStep, StrictMode, ToolStep};

/// The observable call log of a scripted LLM handler.
pub type LlmCallLog = CallLog<ChatRequest, RequirementResult>;
/// The observable call log of a scripted tool handler or registry.
pub type ToolCallLog = CallLog<ToolCall, RequirementResult>;
/// The observable call log of a scripted interaction handler.
pub type InteractionCallLog = CallLog<Interaction, InteractionResponse>;
/// The observable call log of a scripted reconfiguration handler.
pub type ReconfigCallLog = CallLog<ToolSetRef, RequirementResult>;

/// Fulfils a `NeedLlm` from a [`Script`] of [`LlmStep`]s.
///
/// Every call is recorded in an observable [`LlmCallLog`]. When the script is
/// drained under [`StrictMode::Error`] the
/// exhaustion folds into a transport failure carried inside
/// [`RequirementResult::Llm`]'s `Err`, keeping the failure in the LLM family.
pub struct ScriptedLlmHandler {
    script: Arc<Script<LlmStep>>,
    log: Arc<LlmCallLog>,
}

impl ScriptedLlmHandler {
    /// Wraps a shared `script`, tracking calls in a fresh log.
    #[must_use]
    pub fn new(script: Arc<Script<LlmStep>>) -> Self {
        Self {
            script,
            log: Arc::new(CallLog::new()),
        }
    }

    /// Builds a handler over a fresh script of `steps`.
    #[must_use]
    pub fn from_steps(steps: impl IntoIterator<Item = LlmStep>) -> Self {
        Self::new(Arc::new(Script::new(steps)))
    }

    /// Returns the shared script this handler consumes.
    #[must_use]
    pub fn script(&self) -> &Arc<Script<LlmStep>> {
        &self.script
    }

    /// Returns the shared call log recording every fulfilled call.
    #[must_use]
    pub fn log(&self) -> &Arc<LlmCallLog> {
        &self.log
    }
}

#[async_trait]
impl LlmHandler for ScriptedLlmHandler {
    async fn fulfill(
        &self,
        request: &ChatRequest,
        _mode: LlmStepMode,
        _ctx: &RunContext,
    ) -> RequirementResult {
        let ticket = self.log.begin(request.clone());
        let result = match self.script.next_step() {
            Ok(step) => step.into_result(),
            Err(error) => RequirementResult::Llm(Err(ClientError::Other(error.to_string()))),
        };
        self.log.complete(ticket, result.clone());
        result
    }
}

/// Fulfils a `NeedTool` from a [`Script`] of [`ToolStep`]s.
///
/// Every call is recorded in an observable [`ToolCallLog`]. When the script is
/// drained under [`StrictMode::Error`] the
/// exhaustion folds into a [`ToolRuntimeError::ExecutionFailed`] carried inside
/// [`RequirementResult::Tool`]'s `Err`, keeping the failure in the tool family.
pub struct ScriptedToolHandler {
    script: Arc<Script<ToolStep>>,
    log: Arc<ToolCallLog>,
}

impl ScriptedToolHandler {
    /// Wraps a shared `script`, tracking calls in a fresh log.
    #[must_use]
    pub fn new(script: Arc<Script<ToolStep>>) -> Self {
        Self {
            script,
            log: Arc::new(CallLog::new()),
        }
    }

    /// Builds a handler over a fresh script of `steps`.
    #[must_use]
    pub fn from_steps(steps: impl IntoIterator<Item = ToolStep>) -> Self {
        Self::new(Arc::new(Script::new(steps)))
    }

    /// Returns the shared script this handler consumes.
    #[must_use]
    pub fn script(&self) -> &Arc<Script<ToolStep>> {
        &self.script
    }

    /// Returns the shared call log recording every fulfilled call.
    #[must_use]
    pub fn log(&self) -> &Arc<ToolCallLog> {
        &self.log
    }
}

#[async_trait]
impl ToolHandler for ScriptedToolHandler {
    async fn fulfill(
        &self,
        _call_id: ToolCallId,
        call: &ToolCall,
        _ctx: &RunContext,
    ) -> RequirementResult {
        let ticket = self.log.begin(call.clone());
        let result = tool_step_result(&self.script, call);
        self.log.complete(ticket, result.clone());
        result
    }
}

/// Runs the next [`ToolStep`] for `call`, folding exhaustion into a tool-family
/// failure so the result never leaves the tool family.
fn tool_step_result(script: &Script<ToolStep>, call: &ToolCall) -> RequirementResult {
    match script.next_step() {
        Ok(step) => step.into_result(),
        Err(error) => RequirementResult::Tool(Err(ToolRuntimeError::ExecutionFailed {
            tool_name: call.name.clone(),
            message: error.to_string(),
        })),
    }
}

/// Fulfils a `NeedReconfigRegistry` from a [`Script`] of [`ReconfigStep`]s.
///
/// Every call is recorded in an observable [`ReconfigCallLog`]. When the script
/// is drained under [`StrictMode::Error`] the
/// exhaustion folds into a [`ToolRuntimeError::InvalidRegistry`] carried inside
/// [`RequirementResult::Reconfig`]'s `Err`, keeping the failure in the reconfig
/// family.
pub struct ScriptedReconfigHandler {
    script: Arc<Script<ReconfigStep>>,
    log: Arc<ReconfigCallLog>,
}

impl ScriptedReconfigHandler {
    /// Wraps a shared `script`, tracking calls in a fresh log.
    #[must_use]
    pub fn new(script: Arc<Script<ReconfigStep>>) -> Self {
        Self {
            script,
            log: Arc::new(CallLog::new()),
        }
    }

    /// Builds a handler over a fresh script of `steps`.
    #[must_use]
    pub fn from_steps(steps: impl IntoIterator<Item = ReconfigStep>) -> Self {
        Self::new(Arc::new(Script::new(steps)))
    }

    /// Returns the shared script this handler consumes.
    #[must_use]
    pub fn script(&self) -> &Arc<Script<ReconfigStep>> {
        &self.script
    }

    /// Returns the shared call log recording every fulfilled call.
    #[must_use]
    pub fn log(&self) -> &Arc<ReconfigCallLog> {
        &self.log
    }
}

#[async_trait]
impl ReconfigHandler for ScriptedReconfigHandler {
    async fn fulfill(&self, tool_set: &ToolSetRef, _ctx: &RunContext) -> RequirementResult {
        let ticket = self.log.begin(tool_set.clone());
        let result = match self.script.next_step() {
            Ok(step) => step.into_result(),
            Err(error) => RequirementResult::Reconfig(Err(ToolRuntimeError::InvalidRegistry {
                message: error.to_string(),
            })),
        };
        self.log.complete(ticket, result.clone());
        result
    }
}

/// One reactive decision the [`ScriptedInteractionHandler`] applies to the
/// interaction request it is handed.
///
/// A decision is resolved against the *live* request so an approval response
/// always addresses the request's `step_id`/`call_id`. The approval-oriented
/// variants ([`Approve`](Self::Approve), [`ApproveWith`](Self::ApproveWith),
/// [`Deny`](Self::Deny)) fall back to a trivial in-family response for the
/// question/choice interactions the
/// [`DefaultAgentMachine`](agent_lib::agent::DefaultAgentMachine) never emits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InteractionDecision {
    /// Approve an approval interaction.
    Approve,
    /// Approve an approval interaction with a stable message.
    ApproveWith(String),
    /// Deny an approval interaction, with an optional message.
    Deny(Option<String>),
    /// Resolve an approval interaction as a timeout, with an optional message.
    ///
    /// The machine folds a [`ApprovalDecision::Timeout`] into a denied tool
    /// status, exactly as a real attended backend that never answered in time.
    Timeout(Option<String>),
    /// Cancel an approval interaction, with an optional message.
    ///
    /// The machine folds a [`ApprovalDecision::Cancel`] into a cancelled tool
    /// status, modelling an approver that aborted the request.
    Cancel(Option<String>),
    /// Answer a question interaction with free-form text.
    Answer(String),
    /// Select a zero-based option for a choice interaction.
    Choice(usize),
    /// Return an explicit, pre-built response verbatim.
    ///
    /// The escape hatch for full control; can be used to inject a deliberately
    /// mismatched response for negative tests.
    Response(InteractionResponse),
}

impl InteractionDecision {
    /// Resolves this decision against `request` into a concrete response.
    fn respond(&self, request: &Interaction) -> InteractionResponse {
        match self {
            Self::Approve => approval_response(request, ApprovalDecision::Approve, None),
            Self::ApproveWith(message) => {
                approval_response(request, ApprovalDecision::Approve, Some(message.clone()))
            }
            Self::Deny(message) => {
                approval_response(request, ApprovalDecision::Deny, message.clone())
            }
            Self::Timeout(message) => {
                approval_response(request, ApprovalDecision::Timeout, message.clone())
            }
            Self::Cancel(message) => {
                approval_response(request, ApprovalDecision::Cancel, message.clone())
            }
            Self::Answer(text) => InteractionResponse::answer(text.clone()),
            Self::Choice(index) => InteractionResponse::Choice(*index),
            Self::Response(response) => response.clone(),
        }
    }
}

/// Builds an approval response addressed to `request`, or a trivial in-family
/// response for the non-approval kinds the default machine never emits.
fn approval_response(
    request: &Interaction,
    decision: ApprovalDecision,
    message: Option<String>,
) -> InteractionResponse {
    match request.kind() {
        InteractionKind::Approval { call_id, .. } => InteractionResponse::Approval(
            ApprovalResponse::new(request.step_id(), *call_id, decision, message),
        ),
        InteractionKind::Question { .. } => InteractionResponse::answer(String::new()),
        InteractionKind::Choice { .. } => InteractionResponse::Choice(0),
    }
}

/// How a [`ScriptedInteractionHandler`] chooses a decision for each request.
enum InteractionMode {
    /// Apply one fixed decision to every interaction.
    Fixed(InteractionDecision),
    /// Consume decisions in dispatch order; on exhaustion apply `on_exhausted`
    /// under [`StrictMode::Error`] or panic under [`StrictMode::Panic`].
    Sequence {
        decisions: Mutex<VecDeque<InteractionDecision>>,
        on_exhausted: InteractionDecision,
        strict: StrictMode,
        defined_len: usize,
        label: Option<String>,
    },
}

/// Fulfils a `NeedInteraction` reactively from an [`InteractionDecision`].
///
/// Unlike the script-backed handlers, this handler answers each request by
/// reacting to it: an [`InteractionResponse`] carries no `Err` family, and an
/// approval response must address the live request's `step_id`/`call_id`, so a
/// pre-built response queue would be brittle. Use
/// [`approve_all`](Self::approve_all) / [`deny_all`](Self::deny_all) for a fixed
/// disposition, or [`sequence`](Self::sequence) for an ordered set of decisions.
/// Every call is recorded in an observable [`InteractionCallLog`].
pub struct ScriptedInteractionHandler {
    mode: InteractionMode,
    log: Arc<InteractionCallLog>,
}

impl ScriptedInteractionHandler {
    /// Approves every approval interaction (and gives a trivial in-family answer
    /// to the question/choice kinds the default machine never emits).
    #[must_use]
    pub fn approve_all() -> Self {
        Self::fixed(InteractionDecision::Approve)
    }

    /// Denies every approval interaction with an optional `message`.
    #[must_use]
    pub fn deny_all(message: Option<String>) -> Self {
        Self::fixed(InteractionDecision::Deny(message))
    }

    /// Applies one fixed `decision` to every interaction.
    #[must_use]
    pub fn fixed(decision: InteractionDecision) -> Self {
        Self {
            mode: InteractionMode::Fixed(decision),
            log: Arc::new(CallLog::new()),
        }
    }

    /// Consumes `decisions` in dispatch order.
    ///
    /// Once the sequence is drained, later interactions fall back to the
    /// exhaustion decision (default [`Deny(None)`](InteractionDecision::Deny),
    /// override with [`with_exhausted_decision`](Self::with_exhausted_decision))
    /// under [`StrictMode::Error`], or panic under [`StrictMode::Panic`].
    #[must_use]
    pub fn sequence(decisions: impl IntoIterator<Item = InteractionDecision>) -> Self {
        let decisions: VecDeque<InteractionDecision> = decisions.into_iter().collect();
        let defined_len = decisions.len();
        Self {
            mode: InteractionMode::Sequence {
                decisions: Mutex::new(decisions),
                on_exhausted: InteractionDecision::Deny(None),
                strict: StrictMode::Error,
                defined_len,
                label: None,
            },
            log: Arc::new(CallLog::new()),
        }
    }

    /// Sets the decision applied once a [`sequence`](Self::sequence) is drained
    /// (ignored for the fixed dispositions).
    #[must_use]
    pub fn with_exhausted_decision(mut self, decision: InteractionDecision) -> Self {
        if let InteractionMode::Sequence { on_exhausted, .. } = &mut self.mode {
            *on_exhausted = decision;
        }
        self
    }

    /// Sets the exhaustion behaviour of a [`sequence`](Self::sequence) (ignored
    /// for the fixed dispositions).
    #[must_use]
    pub fn with_strict_mode(mut self, mode: StrictMode) -> Self {
        if let InteractionMode::Sequence { strict, .. } = &mut self.mode {
            *strict = mode;
        }
        self
    }

    /// Attaches a diagnostic label surfaced in a [`sequence`](Self::sequence)
    /// exhaustion panic (ignored for the fixed dispositions).
    #[must_use]
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        if let InteractionMode::Sequence { label: slot, .. } = &mut self.mode {
            *slot = Some(label.into());
        }
        self
    }

    /// Returns the shared call log recording every fulfilled interaction.
    #[must_use]
    pub fn log(&self) -> &Arc<InteractionCallLog> {
        &self.log
    }

    /// Chooses the next decision for one incoming request.
    fn next_decision(&self) -> InteractionDecision {
        match &self.mode {
            InteractionMode::Fixed(decision) => decision.clone(),
            InteractionMode::Sequence {
                decisions,
                on_exhausted,
                strict,
                defined_len,
                label,
            } => {
                if let Some(decision) = decisions
                    .lock()
                    .expect("interaction decisions mutex poisoned")
                    .pop_front()
                {
                    return decision;
                }
                match strict {
                    StrictMode::Error => on_exhausted.clone(),
                    StrictMode::Panic => panic!(
                        "interaction script{} exhausted: requested a decision after \
                         {defined_len} scripted decision(s)",
                        label
                            .as_deref()
                            .map(|label| format!(" `{label}`"))
                            .unwrap_or_default()
                    ),
                }
            }
        }
    }
}

#[async_trait]
impl InteractionHandler for ScriptedInteractionHandler {
    async fn fulfill(&self, request: &Interaction, _ctx: &RunContext) -> RequirementResult {
        let ticket = self.log.begin(request.clone());
        let response = self.next_decision().respond(request);
        self.log.complete(ticket, response.clone());
        RequirementResult::Interaction(response)
    }
}

/// A [`ToolRegistry`] whose executions are scripted by a [`Script`] of
/// [`ToolStep`]s.
///
/// This is the registry-shaped variant of [`ScriptedToolHandler`], for tests
/// that route a `NeedTool` through the reference
/// [`ToolRegistryHandler`](agent_lib::agent::ToolRegistryHandler) (or exercise a
/// turn-boundary reconfiguration) rather than plugging a bare [`ToolHandler`]
/// into a scope. It declares a fixed set of [`Tool`]s and folds script
/// exhaustion into a [`ToolRuntimeError::ExecutionFailed`].
#[derive(Debug)]
pub struct ScriptedToolRegistry {
    declarations: Vec<Tool>,
    script: Arc<Script<ToolStep>>,
    log: Arc<ToolCallLog>,
}

impl ScriptedToolRegistry {
    /// Builds a registry declaring `declarations` and executing `script`.
    #[must_use]
    pub fn new(declarations: Vec<Tool>, script: Arc<Script<ToolStep>>) -> Self {
        Self {
            declarations,
            script,
            log: Arc::new(CallLog::new()),
        }
    }

    /// Builds a registry declaring `declarations` over a fresh script of `steps`.
    #[must_use]
    pub fn from_steps(declarations: Vec<Tool>, steps: impl IntoIterator<Item = ToolStep>) -> Self {
        Self::new(declarations, Arc::new(Script::new(steps)))
    }

    /// Returns the shared script this registry consumes.
    #[must_use]
    pub fn script(&self) -> &Arc<Script<ToolStep>> {
        &self.script
    }

    /// Returns the shared call log recording every executed call.
    #[must_use]
    pub fn log(&self) -> &Arc<ToolCallLog> {
        &self.log
    }
}

#[async_trait]
impl ToolRegistry for ScriptedToolRegistry {
    fn declarations(&self) -> Vec<Tool> {
        self.declarations.clone()
    }

    async fn execute(
        &self,
        _call_id: ToolCallId,
        call: ToolCall,
    ) -> Result<ToolResponse, ToolRuntimeError> {
        let ticket = self.log.begin(call.clone());
        let result = tool_step_result(&self.script, &call);
        self.log.complete(ticket, result.clone());
        match result {
            RequirementResult::Tool(inner) => inner,
            // `tool_step_result` always yields the tool family.
            other => unreachable!("scripted tool registry produced a non-tool result: {other:?}"),
        }
    }
}

/// A deliberately wrong-family handler used to exercise the driver's return-path
/// type check.
///
/// It implements all four handler traits but always returns the same
/// [`RequirementResult`], regardless of the requirement it is asked to fulfil.
/// Wiring it into a scope for a *different* family makes
/// [`drain`](agent_lib::agent::drain) reject the result through
/// [`RequirementKind::accepts`](agent_lib::agent::RequirementKind::accepts),
/// surfacing an [`AgentError::Other`](agent_lib::agent::AgentError::Other) that
/// names the misaligned requirement.
#[derive(Clone, Debug)]
pub struct MisalignedHandler {
    result: RequirementResult,
}

impl MisalignedHandler {
    /// Builds a handler that always returns `result`.
    #[must_use]
    pub fn returning(result: RequirementResult) -> Self {
        Self { result }
    }
}

#[async_trait]
impl LlmHandler for MisalignedHandler {
    async fn fulfill(
        &self,
        _request: &ChatRequest,
        _mode: LlmStepMode,
        _ctx: &RunContext,
    ) -> RequirementResult {
        self.result.clone()
    }
}

#[async_trait]
impl ToolHandler for MisalignedHandler {
    async fn fulfill(
        &self,
        _call_id: ToolCallId,
        _call: &ToolCall,
        _ctx: &RunContext,
    ) -> RequirementResult {
        self.result.clone()
    }
}

#[async_trait]
impl InteractionHandler for MisalignedHandler {
    async fn fulfill(&self, _request: &Interaction, _ctx: &RunContext) -> RequirementResult {
        self.result.clone()
    }
}

#[async_trait]
impl ReconfigHandler for MisalignedHandler {
    async fn fulfill(&self, _tool_set: &ToolSetRef, _ctx: &RunContext) -> RequirementResult {
        self.result.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        InteractionDecision, MisalignedHandler, ScriptedInteractionHandler, ScriptedLlmHandler,
        ScriptedReconfigHandler, ScriptedToolHandler, ScriptedToolRegistry,
    };
    use crate::fixtures::{
        agent_spec, agent_state, default_machine, root_context, tool_call, tool_ok, user_input,
        weather_tool,
    };
    use crate::ids::SeqIds;
    use crate::script::{LlmStep, ReconfigStep, StrictMode, ToolStep};
    use agent_lib::agent::{
        AgentError, ApprovalDecision, ApprovalRequirement, HandlerScope, Interaction,
        InteractionHandler, InteractionResponse, LlmHandler, LlmStepMode, LoopCursorKind,
        ReconfigHandler, RequirementKind, RequirementKindTag, RequirementResult, ToolHandler,
        ToolRegistry, ToolRuntimeError, ToolSetRef, drain,
    };
    use agent_lib::client::{ChatRequest, ClientError};
    use std::sync::Arc;

    /// Builds a minimal non-streaming request for the return-path checks.
    fn chat_request() -> ChatRequest {
        ChatRequest {
            model: "test-model".to_owned(),
            messages: Vec::new(),
            tools: Vec::new(),
            system: None,
            max_tokens: 256,
            temperature: None,
            stream: false,
            provider_extras: None,
        }
    }

    /// A top scope that fulfils only `NeedLlm`, for driving whole-turn drains.
    struct LlmScope {
        llm: Box<dyn LlmHandler>,
    }

    impl HandlerScope for LlmScope {
        fn llm(&self) -> Option<&dyn LlmHandler> {
            Some(self.llm.as_ref())
        }
    }

    // ----- return-path type alignment (`RequirementKind::accepts`) -----

    #[tokio::test]
    async fn scripted_llm_result_is_accepted_by_need_llm() {
        let ids = SeqIds::new();
        let ctx = root_context(&ids);
        let handler = ScriptedLlmHandler::from_steps([LlmStep::text("hello")]);
        let kind = RequirementKind::NeedLlm {
            request: chat_request(),
            mode: LlmStepMode::NonStreaming,
        };

        let result = handler
            .fulfill(&chat_request(), LlmStepMode::NonStreaming, &ctx)
            .await;

        assert_eq!(result.tag(), RequirementKindTag::Llm);
        assert!(kind.accepts(&result).is_ok());
        assert_eq!(handler.log().len(), 1);
        assert_eq!(handler.log().completed_len(), 1);
    }

    #[tokio::test]
    async fn scripted_tool_result_is_accepted_by_need_tool() {
        let ids = SeqIds::new();
        let ctx = root_context(&ids);
        let call_id = ids.tool_call_id();
        let call = tool_call(
            "call-weather",
            "get_weather",
            serde_json::json!({ "city": "SH" }),
        );
        let handler = ScriptedToolHandler::from_steps([ToolStep::ok("call-weather", "sunny")]);
        let kind = RequirementKind::NeedTool {
            call_id,
            call: call.clone(),
        };

        let result = handler.fulfill(call_id, &call, &ctx).await;

        assert_eq!(result.tag(), RequirementKindTag::Tool);
        assert!(kind.accepts(&result).is_ok());
        assert_eq!(handler.log().len(), 1);
    }

    #[tokio::test]
    async fn scripted_interaction_response_is_accepted_by_need_interaction() {
        let ids = SeqIds::new();
        let ctx = root_context(&ids);
        let interaction = Interaction::approval(
            ids.step_id(),
            ids.tool_call_id(),
            ApprovalRequirement::required(Some("please confirm".to_owned())),
        );
        let handler = ScriptedInteractionHandler::approve_all();
        let kind = RequirementKind::NeedInteraction {
            request: interaction.clone(),
        };

        let result = handler.fulfill(&interaction, &ctx).await;

        assert_eq!(result.tag(), RequirementKindTag::Interaction);
        // `accepts` additionally validates the approval addresses the request's
        // step/call, so a reactive response that ignored them would be rejected.
        assert!(kind.accepts(&result).is_ok());
        let RequirementResult::Interaction(InteractionResponse::Approval(approval)) = &result
        else {
            panic!("approve_all must answer an approval with an approval response");
        };
        assert_eq!(approval.decision(), ApprovalDecision::Approve);
    }

    #[tokio::test]
    async fn scripted_reconfig_result_is_accepted_by_need_reconfig() {
        let ids = SeqIds::new();
        let ctx = root_context(&ids);
        let tool_set = ToolSetRef::new(ids.tool_set_id(), vec![weather_tool()]);
        let handler = ScriptedReconfigHandler::from_steps([ReconfigStep::ok()]);
        let kind = RequirementKind::NeedReconfigRegistry {
            tool_set: tool_set.clone(),
        };

        let result = handler.fulfill(&tool_set, &ctx).await;

        assert_eq!(result.tag(), RequirementKindTag::Reconfig);
        assert!(kind.accepts(&result).is_ok());
        assert_eq!(handler.log().len(), 1);
    }

    // ----- error paths stay in their own family -----

    #[tokio::test]
    async fn llm_error_and_exhaustion_stay_in_the_llm_family() {
        let ids = SeqIds::new();
        let ctx = root_context(&ids);
        // One scripted transport failure, then an exhausted over-run.
        let handler = ScriptedLlmHandler::from_steps([LlmStep::error(ClientError::Timeout)]);

        let scripted = handler
            .fulfill(&chat_request(), LlmStepMode::NonStreaming, &ctx)
            .await;
        let RequirementResult::Llm(Err(ClientError::Timeout)) = scripted else {
            panic!("a scripted LLM error must stay an LLM-family Err");
        };

        let overrun = handler
            .fulfill(&chat_request(), LlmStepMode::NonStreaming, &ctx)
            .await;
        let RequirementResult::Llm(Err(ClientError::Other(_))) = overrun else {
            panic!("LLM exhaustion must fold into an LLM-family Err");
        };
    }

    #[tokio::test]
    async fn tool_error_and_exhaustion_stay_in_the_tool_family() {
        let ids = SeqIds::new();
        let ctx = root_context(&ids);
        let call_id = ids.tool_call_id();
        let call = tool_call("call-weather", "get_weather", serde_json::json!({}));
        let handler = ScriptedToolHandler::from_steps([ToolStep::runtime_error(
            ToolRuntimeError::UnknownTool {
                name: "get_weather".to_owned(),
            },
        )]);

        let scripted = handler.fulfill(call_id, &call, &ctx).await;
        let RequirementResult::Tool(Err(ToolRuntimeError::UnknownTool { .. })) = scripted else {
            panic!("a scripted tool runtime error must stay a tool-family Err");
        };

        let overrun = handler.fulfill(call_id, &call, &ctx).await;
        let RequirementResult::Tool(Err(ToolRuntimeError::ExecutionFailed { tool_name, .. })) =
            overrun
        else {
            panic!("tool exhaustion must fold into a tool-family Err");
        };
        assert_eq!(tool_name, "get_weather");
    }

    #[tokio::test]
    async fn interaction_deny_stays_in_the_interaction_family() {
        let ids = SeqIds::new();
        let ctx = root_context(&ids);
        let interaction = Interaction::approval(
            ids.step_id(),
            ids.tool_call_id(),
            ApprovalRequirement::required(None),
        );
        let handler = ScriptedInteractionHandler::deny_all(Some("policy".to_owned()));

        let result = handler.fulfill(&interaction, &ctx).await;

        let RequirementResult::Interaction(InteractionResponse::Approval(approval)) = &result
        else {
            panic!("deny_all must answer an approval with an approval response");
        };
        assert_eq!(approval.decision(), ApprovalDecision::Deny);
        assert_eq!(approval.message(), Some("policy"));
    }

    #[tokio::test]
    async fn interaction_timeout_and_cancel_stay_in_the_interaction_family() {
        let ids = SeqIds::new();
        let ctx = root_context(&ids);
        let approval = |ids: &SeqIds| {
            Interaction::approval(
                ids.step_id(),
                ids.tool_call_id(),
                ApprovalRequirement::required(None),
            )
        };
        let decision = |result: &RequirementResult| -> (ApprovalDecision, Option<String>) {
            let RequirementResult::Interaction(InteractionResponse::Approval(approval)) = result
            else {
                panic!("an approval decision must answer with an approval response");
            };
            (approval.decision(), approval.message().map(str::to_owned))
        };

        // Both dispositions address the live request and keep the family.
        let handler = ScriptedInteractionHandler::sequence([
            InteractionDecision::Timeout(Some("timed out".to_owned())),
            InteractionDecision::Cancel(Some("aborted".to_owned())),
        ]);

        let timed_out = handler.fulfill(&approval(&ids), &ctx).await;
        assert_eq!(
            decision(&timed_out),
            (ApprovalDecision::Timeout, Some("timed out".to_owned()))
        );

        let cancelled = handler.fulfill(&approval(&ids), &ctx).await;
        assert_eq!(
            decision(&cancelled),
            (ApprovalDecision::Cancel, Some("aborted".to_owned()))
        );
    }

    #[tokio::test]
    async fn reconfig_error_and_exhaustion_stay_in_the_reconfig_family() {
        let ids = SeqIds::new();
        let ctx = root_context(&ids);
        let tool_set = ToolSetRef::new(ids.tool_set_id(), Vec::new());
        let handler = ScriptedReconfigHandler::from_steps([ReconfigStep::error(
            ToolRuntimeError::InvalidRegistry {
                message: "bad".to_owned(),
            },
        )]);

        let scripted = handler.fulfill(&tool_set, &ctx).await;
        let RequirementResult::Reconfig(Err(ToolRuntimeError::InvalidRegistry { .. })) = scripted
        else {
            panic!("a scripted reconfig error must stay a reconfig-family Err");
        };

        let overrun = handler.fulfill(&tool_set, &ctx).await;
        let RequirementResult::Reconfig(Err(_)) = overrun else {
            panic!("reconfig exhaustion must fold into a reconfig-family Err");
        };
    }

    // ----- interaction sequencing -----

    #[tokio::test]
    async fn interaction_sequence_consumes_decisions_in_order_then_falls_back() {
        let ids = SeqIds::new();
        let ctx = root_context(&ids);
        let handler = ScriptedInteractionHandler::sequence([
            InteractionDecision::Approve,
            InteractionDecision::Deny(Some("second".to_owned())),
        ])
        .with_exhausted_decision(InteractionDecision::Deny(Some("exhausted".to_owned())));

        let decisions = |response: &RequirementResult| -> (ApprovalDecision, Option<String>) {
            let RequirementResult::Interaction(InteractionResponse::Approval(approval)) = response
            else {
                panic!("sequence must answer approvals with approval responses");
            };
            (approval.decision(), approval.message().map(str::to_owned))
        };

        let approval = |ids: &SeqIds| {
            Interaction::approval(
                ids.step_id(),
                ids.tool_call_id(),
                ApprovalRequirement::required(None),
            )
        };

        let first = handler.fulfill(&approval(&ids), &ctx).await;
        assert_eq!(decisions(&first), (ApprovalDecision::Approve, None));

        let second = handler.fulfill(&approval(&ids), &ctx).await;
        assert_eq!(
            decisions(&second),
            (ApprovalDecision::Deny, Some("second".to_owned()))
        );

        // Third call over-runs the two scripted decisions and falls back.
        let third = handler.fulfill(&approval(&ids), &ctx).await;
        assert_eq!(
            decisions(&third),
            (ApprovalDecision::Deny, Some("exhausted".to_owned()))
        );
        assert_eq!(handler.log().len(), 3);
    }

    #[test]
    fn interaction_sequence_panic_mode_only_panics_when_opted_in() {
        let ids = SeqIds::new();
        let ctx = root_context(&ids);
        let approval = |ids: &SeqIds| {
            Interaction::approval(
                ids.step_id(),
                ids.tool_call_id(),
                ApprovalRequirement::required(None),
            )
        };

        // The default Error mode folds an over-run into the fallback decision.
        let lenient = ScriptedInteractionHandler::sequence([]);
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            futures::executor::block_on(lenient.fulfill(&approval(&ids), &ctx))
        }));
        assert!(
            outcome.is_ok(),
            "the default Error mode must not panic on exhaustion"
        );

        // Opt-in Panic mode aborts the over-run instead.
        let strict = ScriptedInteractionHandler::sequence([])
            .with_strict_mode(StrictMode::Panic)
            .with_label("weather-scenario");
        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            futures::executor::block_on(strict.fulfill(&approval(&ids), &ctx))
        }));
        assert!(
            panicked.is_err(),
            "opt-in Panic mode must panic on exhaustion"
        );
    }

    // ----- scripted tool registry -----

    #[tokio::test]
    async fn scripted_tool_registry_declares_and_executes() {
        let ids = SeqIds::new();
        let registry = ScriptedToolRegistry::from_steps(
            vec![weather_tool()],
            [ToolStep::ok("call-weather", "sunny")],
        );

        assert_eq!(registry.declarations(), vec![weather_tool()]);

        let call = tool_call(
            "call-weather",
            "get_weather",
            serde_json::json!({ "city": "SH" }),
        );
        let ok = registry
            .execute(ids.tool_call_id(), call.clone())
            .await
            .expect("scripted registry returns the scripted response");
        assert_eq!(ok.tool_call_id, "call-weather");

        // Over-running the script surfaces a tool-family runtime error.
        let overrun = registry.execute(ids.tool_call_id(), call).await;
        assert!(matches!(
            overrun,
            Err(ToolRuntimeError::ExecutionFailed { .. })
        ));
        assert_eq!(registry.log().len(), 2);
    }

    // ----- drain integration -----

    #[tokio::test]
    async fn scripted_llm_handler_drives_a_text_turn_through_drain() {
        let ids = SeqIds::new();
        let ctx = root_context(&ids);
        let mut machine = default_machine(&ids, agent_state(&ids, agent_spec(&ids)));

        let handler = ScriptedLlmHandler::from_steps([LlmStep::text("all done")]);
        let log = Arc::clone(handler.log());
        let scope = LlmScope {
            llm: Box::new(handler),
        };

        let done = drain(&mut machine, user_input(&ids, "hi"), &scope, None, &ctx)
            .await
            .expect("a scripted text turn drains to Done");

        assert_eq!(done.cursor().kind(), LoopCursorKind::Done);
        assert_eq!(log.len(), 1);
        assert_eq!(log.completed_len(), 1);
    }

    #[tokio::test]
    async fn misaligned_handler_trips_the_drain_family_check() {
        let ids = SeqIds::new();
        let ctx = root_context(&ids);
        let mut machine = default_machine(&ids, agent_state(&ids, agent_spec(&ids)));

        // The scope's LLM slot returns a *tool* result: `drain` must reject it.
        let misaligned =
            MisalignedHandler::returning(RequirementResult::Tool(Ok(tool_ok("call-x", "nope"))));
        let scope = LlmScope {
            llm: Box::new(misaligned),
        };

        let error = drain(&mut machine, user_input(&ids, "hi"), &scope, None, &ctx)
            .await
            .expect_err("a misaligned result must fail the turn");

        match error {
            AgentError::Other(message) => assert!(
                message.contains("misaligned"),
                "expected a misalignment diagnostic, got: {message}"
            ),
            other => panic!("expected AgentError::Other, got {other:?}"),
        }
    }
}
