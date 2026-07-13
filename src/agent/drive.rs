//! Handler scope and effect handlers for driving an
//! [`AgentMachine`](crate::agent::AgentMachine).
//!
//! [`AgentMachine::step`](crate::agent::AgentMachine::step) is a pure state
//! machine: it never performs IO, it only *reifies* the IO it needs into
//! [`Requirement`](crate::agent::Requirement)s. Something outside the machine
//! must actually fulfill those requirements. This module defines that
//! *mechanism* (migration doc §6): a single drain layer is one set of
//! requirement handlers, exposed through a [`HandlerScope`], and the default
//! behavior for any requirement a scope does not handle is to *pop* it to the
//! outer scope.
//!
//! # Scope and handlers
//!
//! A [`HandlerScope`] offers up to four handlers, one per
//! [`RequirementKind`](crate::agent::RequirementKind) family. Each accessor
//! defaults to `None`, meaning "this layer cannot fulfill that family" — such a
//! requirement pops outward. A layer overrides only the accessors it can serve:
//!
//! - [`LlmHandler`] fulfills a `NeedLlm` by talking to an
//!   [`LlmClient`](crate::client::LlmClient).
//! - [`ToolHandler`] fulfills a `NeedTool` through a
//!   [`ToolRegistry`](crate::agent::ToolRegistry).
//! - [`InteractionHandler`] fulfills a `NeedInteraction` from an interaction
//!   backend — a human UI (attended) or a
//!   [`ToolApprovalPolicy`](crate::agent::ToolApprovalPolicy) (unattended).
//! - [`SubagentHandler`] fulfills a `NeedSubagent` by deriving and driving a
//!   child agent. Only its *signature* is defined in this stage; the
//!   implementation lands in M5.
//!
//! Handlers are `async` because they perform the real IO the sans-io machine
//! deferred; all `await`ing lives here, never in `step`.
//!
//! # Return-path type alignment
//!
//! Every handler hands back a [`RequirementResult`]. The result *family* must
//! match the requirement kind it fulfills: an [`LlmHandler`] returns
//! [`RequirementResult::Llm`], a [`ToolHandler`] returns
//! [`RequirementResult::Tool`], and so on. Failures are encoded inside the
//! result (for example `RequirementResult::Llm(Err(..))`), not by returning the
//! wrong family. The driver validates this alignment with
//! [`RequirementKind::accepts`](crate::agent::RequirementKind::accepts) before
//! resuming the machine.
//!
//! # What this module defines
//!
//! This task (M3-1) defines only the scope and handler traits. The `drain`
//! reference implementation, the `Pop` routing across scopes, and the
//! `UnhandledRequirement` error land in M3-2; a reference driver that wraps a
//! client / registry / policy into a single scope and replays the existing loop
//! integration tests lands in M3-3.

use crate::{
    agent::{
        LlmStepMode, RunContext,
        interaction::Interaction,
        requirement::{AgentSpecRef, RequirementResult},
    },
    client::ChatRequest,
    conversation::ToolCallId,
    model::tool::ToolCall,
};
use async_trait::async_trait;
use serde_json::Value;

/// One drain layer's set of effect handlers.
///
/// A scope exposes up to four handlers, one per requirement family. Each
/// accessor defaults to `None`, so an empty scope handles nothing and every
/// requirement pops to the outer scope. A layer overrides only the families it
/// can fulfill. See the [module docs](self) for how scopes compose into a drain.
pub trait HandlerScope: Send + Sync {
    /// Returns this layer's [`LlmHandler`], if it fulfills `NeedLlm`.
    fn llm(&self) -> Option<&dyn LlmHandler> {
        None
    }

    /// Returns this layer's [`ToolHandler`], if it fulfills `NeedTool`.
    fn tool(&self) -> Option<&dyn ToolHandler> {
        None
    }

    /// Returns this layer's [`InteractionHandler`], if it fulfills
    /// `NeedInteraction`.
    fn interaction(&self) -> Option<&dyn InteractionHandler> {
        None
    }

    /// Returns this layer's [`SubagentHandler`], if it fulfills `NeedSubagent`.
    fn subagent(&self) -> Option<&dyn SubagentHandler> {
        None
    }
}

/// Fulfills a `NeedLlm` requirement by running one LLM generation.
///
/// The returned [`RequirementResult`] must be a [`RequirementResult::Llm`];
/// transport failures are carried inside its `Err`.
#[async_trait]
pub trait LlmHandler: Send + Sync {
    /// Runs `request` in the requested `mode` and returns the folded result.
    async fn fulfill(
        &self,
        request: &ChatRequest,
        mode: LlmStepMode,
        ctx: &RunContext,
    ) -> RequirementResult;
}

/// Fulfills a `NeedTool` requirement by executing one tool call.
///
/// The returned [`RequirementResult`] must be a [`RequirementResult::Tool`];
/// execution failures are carried inside its `Err`.
#[async_trait]
pub trait ToolHandler: Send + Sync {
    /// Executes `call` under the framework `call_id` and returns its result.
    async fn fulfill(
        &self,
        call_id: ToolCallId,
        call: &ToolCall,
        ctx: &RunContext,
    ) -> RequirementResult;
}

/// Fulfills a `NeedInteraction` requirement from an interaction backend.
///
/// The backend may be a human UI (attended) or a
/// [`ToolApprovalPolicy`](crate::agent::ToolApprovalPolicy) (unattended). The
/// returned [`RequirementResult`] must be a [`RequirementResult::Interaction`]
/// whose response family matches the interaction request.
#[async_trait]
pub trait InteractionHandler: Send + Sync {
    /// Presents `request` to the backend and returns the resolved response.
    async fn fulfill(&self, request: &Interaction, ctx: &RunContext) -> RequirementResult;
}

/// Fulfills a `NeedSubagent` requirement by deriving and driving a child agent.
///
/// This is the only scope-deepening handler: fulfilling it opens another drain
/// layer for the child machine. Only the signature is defined in this stage
/// (M3-1); the derivation, nested drain, and scope enforcement land in M5. The
/// returned [`RequirementResult`] must be a [`RequirementResult::Subagent`].
#[async_trait]
pub trait SubagentHandler: Send + Sync {
    /// Derives the child agent named by `spec_ref`, drives it against `brief`
    /// (optionally constrained by `result_schema`), and returns its result.
    async fn fulfill(
        &self,
        spec_ref: &AgentSpecRef,
        brief: &Interaction,
        result_schema: Option<&Value>,
        ctx: &RunContext,
    ) -> RequirementResult;
}

#[cfg(test)]
mod tests {
    use super::{HandlerScope, InteractionHandler, LlmHandler, ToolHandler};
    use crate::{
        agent::{
            ApprovalDecision, ApprovalRequirement, ApprovalResponse, BudgetLimits, LlmStepMode,
            RunContext, RunId, ToolApprovalPolicy, TraceNodeId,
            interaction::{Interaction, InteractionKind, InteractionResponse},
            requirement::{RequirementKind, RequirementResult},
            tool::{ToolRegistry, ToolRuntimeError},
        },
        client::{Capability, ChatRequest, ClientError, LlmClient, Response},
        conversation::ToolCallId,
        model::tool::{Tool, ToolCall, ToolResponse, ToolStatus},
        stream::{
            StreamEvent,
            accumulator::{CollectError, collect},
        },
    };
    use async_trait::async_trait;
    use futures::stream::BoxStream;
    use serde_json::{Map, json};
    use std::sync::Arc;

    fn run_context() -> RunContext {
        let run_id: RunId = "018f0d9c-7b6a-7c12-8f31-1234567890a1"
            .parse()
            .expect("run id");
        RunContext::new_root(run_id, BudgetLimits::default(), TraceNodeId::new("root"))
    }

    fn step_id() -> crate::agent::StepId {
        "018f0d9c-7b6a-7c12-8f31-1234567890e9"
            .parse()
            .expect("step id")
    }

    fn tool_call_id() -> ToolCallId {
        "018f0d9c-7b6a-7c12-8f31-1234567890c1"
            .parse()
            .expect("tool call id")
    }

    fn chat_request() -> ChatRequest {
        ChatRequest {
            model: "test-model".to_owned(),
            messages: Vec::new(),
            tools: Vec::new(),
            system: None,
            max_tokens: 16,
            temperature: None,
            stream: false,
            provider_extras: None,
        }
    }

    fn tool_call() -> ToolCall {
        ToolCall {
            id: "call-weather".to_owned(),
            name: "get_weather".to_owned(),
            input: json!({ "city": "Shanghai" }),
        }
    }

    fn response() -> Response {
        serde_json::from_value(json!({
            "message": {
                "role": "assistant",
                "content": [{ "type": "text", "text": "hi" }]
            },
            "usage": { "input": 1, "output": 1 },
            "stop_reason": { "value": "end_turn", "raw": "end_turn" }
        }))
        .expect("response")
    }

    /// Minimal [`LlmClient`] that always returns a fixed complete response.
    #[derive(Debug)]
    struct FakeClient;

    #[async_trait]
    impl LlmClient for FakeClient {
        fn capability(&self) -> &Capability {
            &crate::client::ANTHROPIC_DEFAULT_CAPABILITY
        }

        async fn chat(&self, _request: ChatRequest) -> Result<Response, ClientError> {
            Ok(response())
        }

        async fn chat_stream(
            &self,
            _request: ChatRequest,
        ) -> Result<BoxStream<'static, Result<StreamEvent, ClientError>>, ClientError> {
            Err(ClientError::Other(
                "streaming not used in fixture".to_owned(),
            ))
        }
    }

    /// Minimal [`ToolRegistry`] that echoes an `Ok` response for any call.
    #[derive(Debug)]
    struct FakeRegistry;

    #[async_trait]
    impl ToolRegistry for FakeRegistry {
        fn declarations(&self) -> Vec<Tool> {
            Vec::new()
        }

        async fn execute(
            &self,
            _call_id: ToolCallId,
            call: ToolCall,
        ) -> Result<ToolResponse, ToolRuntimeError> {
            Ok(ToolResponse {
                tool_call_id: call.id,
                content: Vec::new(),
                status: ToolStatus::Ok,
                extra: Map::new(),
            })
        }
    }

    /// Wraps an [`LlmClient`] into an [`LlmHandler`].
    struct LlmClientHandler {
        client: Arc<dyn LlmClient>,
    }

    #[async_trait]
    impl LlmHandler for LlmClientHandler {
        async fn fulfill(
            &self,
            request: &ChatRequest,
            mode: LlmStepMode,
            _ctx: &RunContext,
        ) -> RequirementResult {
            let mut request = request.clone();
            let result = match mode {
                LlmStepMode::NonStreaming => {
                    request.stream = false;
                    self.client.chat(request).await
                }
                LlmStepMode::Streaming => {
                    request.stream = true;
                    match self.client.chat_stream(request).await {
                        Ok(stream) => collect(stream).await.map_err(|error| match error {
                            CollectError::Stream(err) => err,
                            CollectError::Accumulator(err) => {
                                ClientError::Protocol(err.to_string())
                            }
                        }),
                        Err(err) => Err(err),
                    }
                }
            };
            RequirementResult::Llm(result)
        }
    }

    /// Wraps a [`ToolRegistry`] into a [`ToolHandler`].
    #[derive(Debug)]
    struct ToolRegistryHandler {
        registry: Arc<dyn ToolRegistry>,
    }

    #[async_trait]
    impl ToolHandler for ToolRegistryHandler {
        async fn fulfill(
            &self,
            call_id: ToolCallId,
            call: &ToolCall,
            _ctx: &RunContext,
        ) -> RequirementResult {
            RequirementResult::Tool(self.registry.execute(call_id, call.clone()).await)
        }
    }

    /// Wraps a [`ToolApprovalPolicy`] into an unattended [`InteractionHandler`].
    #[derive(Debug)]
    struct PolicyInteractionHandler {
        policy: Arc<dyn ToolApprovalPolicy>,
        call: ToolCall,
    }

    #[async_trait]
    impl InteractionHandler for PolicyInteractionHandler {
        async fn fulfill(&self, request: &Interaction, _ctx: &RunContext) -> RequirementResult {
            let response = match request.kind() {
                InteractionKind::Approval { call_id, .. } => {
                    let decision = match self.policy.approval_requirement(*call_id, &self.call) {
                        ApprovalRequirement::AutoApprove => ApprovalDecision::Approve,
                        ApprovalRequirement::RequireApproval { .. } => ApprovalDecision::Deny,
                    };
                    InteractionResponse::Approval(ApprovalResponse::new(
                        request.step_id(),
                        *call_id,
                        decision,
                        None,
                    ))
                }
                InteractionKind::Question { .. } => InteractionResponse::answer("ok".to_owned()),
                InteractionKind::Choice { .. } => InteractionResponse::Choice(0),
            };
            RequirementResult::Interaction(response)
        }
    }

    /// Scope with no overrides: every accessor keeps the `None` default.
    struct EmptyScope;

    impl HandlerScope for EmptyScope {}

    /// Scope wiring the three implemented handler families (no subagent yet).
    struct WrappedScope {
        llm: LlmClientHandler,
        tool: ToolRegistryHandler,
        interaction: PolicyInteractionHandler,
    }

    impl HandlerScope for WrappedScope {
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

    fn wrapped_scope() -> WrappedScope {
        WrappedScope {
            llm: LlmClientHandler {
                client: Arc::new(FakeClient),
            },
            tool: ToolRegistryHandler {
                registry: Arc::new(FakeRegistry),
            },
            interaction: PolicyInteractionHandler {
                policy: Arc::new(crate::agent::NoApprovalPolicy),
                call: tool_call(),
            },
        }
    }

    #[test]
    fn empty_scope_handles_no_requirement_family() {
        let scope = EmptyScope;
        assert!(scope.llm().is_none());
        assert!(scope.tool().is_none());
        assert!(scope.interaction().is_none());
        assert!(scope.subagent().is_none());
    }

    #[test]
    fn wrapped_scope_exposes_implemented_families_only() {
        let scope = wrapped_scope();
        assert!(scope.llm().is_some());
        assert!(scope.tool().is_some());
        assert!(scope.interaction().is_some());
        // SubagentHandler stays unimplemented until M5.
        assert!(scope.subagent().is_none());
    }

    #[tokio::test]
    async fn llm_handler_result_is_accepted_by_its_requirement() {
        let scope = wrapped_scope();
        let ctx = run_context();
        let request = chat_request();
        let mode = LlmStepMode::NonStreaming;

        let result = scope
            .llm()
            .expect("llm handler")
            .fulfill(&request, mode, &ctx)
            .await;

        assert!(matches!(result, RequirementResult::Llm(Ok(_))));
        let kind = RequirementKind::NeedLlm { request, mode };
        kind.accepts(&result).expect("llm result aligns with kind");
    }

    #[tokio::test]
    async fn tool_handler_result_is_accepted_by_its_requirement() {
        let scope = wrapped_scope();
        let ctx = run_context();
        let call = tool_call();
        let call_id = tool_call_id();

        let result = scope
            .tool()
            .expect("tool handler")
            .fulfill(call_id, &call, &ctx)
            .await;

        assert!(matches!(result, RequirementResult::Tool(Ok(_))));
        let kind = RequirementKind::NeedTool { call_id, call };
        kind.accepts(&result).expect("tool result aligns with kind");
    }

    #[tokio::test]
    async fn interaction_handler_result_is_accepted_by_its_requirement() {
        let scope = wrapped_scope();
        let ctx = run_context();
        let request =
            Interaction::approval(step_id(), tool_call_id(), ApprovalRequirement::AutoApprove);

        let result = scope
            .interaction()
            .expect("interaction handler")
            .fulfill(&request, &ctx)
            .await;

        assert!(matches!(result, RequirementResult::Interaction(_)));
        let kind = RequirementKind::NeedInteraction { request };
        kind.accepts(&result)
            .expect("interaction result aligns with kind");
    }
}
