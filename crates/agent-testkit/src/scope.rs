//! The [`TestScope`] builder over `agent-lib`'s
//! [`HandlerScope`].
//!
//! Agent-layer tests otherwise hand-write an `impl HandlerScope` per file, and it
//! is easy for such a scope to accidentally become *total* — to hand back a
//! handler for every family — which silently hides an
//! [`UnhandledRequirement`](agent_lib::agent::AgentError::UnhandledRequirement)
//! that the production wiring would have surfaced. [`TestScope`] makes the choice
//! explicit: a family is served **only** when the test attaches a handler for it
//! (or wraps an inner scope that serves it), so a headless top-level scope still
//! pops an unserved family out to nowhere and fails loudly.
//!
//! # Explicit families, no accidental total scope
//!
//! [`TestScopeBuilder`] exposes one setter per requirement family
//! ([`llm`](TestScopeBuilder::llm), [`tool`](TestScopeBuilder::tool),
//! [`interaction`](TestScopeBuilder::interaction),
//! [`subagent`](TestScopeBuilder::subagent),
//! [`reconfig`](TestScopeBuilder::reconfig)). An unset family reports `None` from
//! its accessor. Interaction is never wired by default: a scope built without
//! [`interaction`](TestScopeBuilder::interaction) (or its readability alias
//! [`attended`](TestScopeBuilder::attended)) is *headless*, so any
//! `NeedInteraction` it is the top layer for surfaces as an
//! `UnhandledRequirement` rather than being auto-approved or dropped.
//!
//! # Arc storage and call-log readback
//!
//! Every handler is stored behind an [`Arc`], so a test can hold its own clone of
//! a scripted handler (or that handler's [`CallLog`](crate::script::CallLog))
//! alongside the scope and read the recorded calls back *after* the drain
//! completes.
//!
//! # Wrapping an existing scope
//!
//! [`wrapping`](TestScopeBuilder::wrapping) delegates any family this layer does
//! not override to an inner [`HandlerScope`] — for example a
//! [`ReferenceScope`](agent_lib::agent::ReferenceScope) wired to live backends,
//! or another [`TestScope`]. A per-family override always wins over the wrapped
//! scope, letting a test swap out a single family while reusing the rest.

use std::sync::Arc;

use agent_lib::agent::{
    HandlerScope, InteractionHandler, LlmHandler, ReconfigHandler, SubagentHandler, ToolHandler,
};

/// One drain layer whose per-family handlers are chosen explicitly.
///
/// Build one with [`TestScope::builder`] (or [`TestScope::empty`] for a scope
/// that serves nothing). Each family accessor returns the attached handler, then
/// the [wrapped inner scope](TestScopeBuilder::wrapping)'s handler, then `None`.
/// Because unset families report `None`, a `TestScope` is never accidentally
/// *total*: whatever it does not serve pops outward, all the way to an
/// [`UnhandledRequirement`](agent_lib::agent::AgentError::UnhandledRequirement)
/// at the top layer. See the [module docs](self) for the rationale.
#[derive(Clone, Default)]
pub struct TestScope {
    llm: Option<Arc<dyn LlmHandler>>,
    tool: Option<Arc<dyn ToolHandler>>,
    interaction: Option<Arc<dyn InteractionHandler>>,
    subagent: Option<Arc<dyn SubagentHandler>>,
    reconfig: Option<Arc<dyn ReconfigHandler>>,
    inner: Option<Arc<dyn HandlerScope>>,
}

impl TestScope {
    /// Starts a fresh [`TestScopeBuilder`] with no families attached.
    #[must_use]
    pub fn builder() -> TestScopeBuilder {
        TestScopeBuilder::new()
    }

    /// Builds a scope that serves nothing: every accessor returns `None`.
    ///
    /// Useful as an intentionally empty top or inner layer.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }
}

impl std::fmt::Debug for TestScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Handlers are trait objects, so summarise which families are wired
        // rather than trying to format them.
        f.debug_struct("TestScope")
            .field("llm", &self.llm.is_some())
            .field("tool", &self.tool.is_some())
            .field("interaction", &self.interaction.is_some())
            .field("subagent", &self.subagent.is_some())
            .field("reconfig", &self.reconfig.is_some())
            .field("wraps_inner", &self.inner.is_some())
            .finish()
    }
}

impl HandlerScope for TestScope {
    fn llm(&self) -> Option<&dyn LlmHandler> {
        match &self.llm {
            Some(handler) => Some(handler.as_ref()),
            None => self.inner.as_ref().and_then(|scope| scope.llm()),
        }
    }

    fn tool(&self) -> Option<&dyn ToolHandler> {
        match &self.tool {
            Some(handler) => Some(handler.as_ref()),
            None => self.inner.as_ref().and_then(|scope| scope.tool()),
        }
    }

    fn interaction(&self) -> Option<&dyn InteractionHandler> {
        match &self.interaction {
            Some(handler) => Some(handler.as_ref()),
            None => self.inner.as_ref().and_then(|scope| scope.interaction()),
        }
    }

    fn subagent(&self) -> Option<&dyn SubagentHandler> {
        match &self.subagent {
            Some(handler) => Some(handler.as_ref()),
            None => self.inner.as_ref().and_then(|scope| scope.subagent()),
        }
    }

    fn reconfig(&self) -> Option<&dyn ReconfigHandler> {
        match &self.reconfig {
            Some(handler) => Some(handler.as_ref()),
            None => self.inner.as_ref().and_then(|scope| scope.reconfig()),
        }
    }
}

/// A fluent builder for [`TestScope`].
///
/// Attach only the families under test; unset families stay `None` so the scope
/// never silently becomes total. Every setter takes an [`Arc`] so a test can keep
/// its own clone of the handler (or its call log) and inspect it after the drain.
/// A concrete `Arc<H>` coerces to the trait-object `Arc` at the call site, so
/// `.tool(Arc::new(ScriptedToolHandler::from_steps(..)))` type-checks directly.
#[derive(Clone, Default)]
pub struct TestScopeBuilder {
    llm: Option<Arc<dyn LlmHandler>>,
    tool: Option<Arc<dyn ToolHandler>>,
    interaction: Option<Arc<dyn InteractionHandler>>,
    subagent: Option<Arc<dyn SubagentHandler>>,
    reconfig: Option<Arc<dyn ReconfigHandler>>,
    inner: Option<Arc<dyn HandlerScope>>,
}

impl TestScopeBuilder {
    /// Creates a builder with no families attached.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Attaches the [`LlmHandler`] that serves this layer's `NeedLlm`.
    #[must_use]
    pub fn llm(mut self, handler: Arc<dyn LlmHandler>) -> Self {
        self.llm = Some(handler);
        self
    }

    /// Attaches the [`ToolHandler`] that serves this layer's `NeedTool`.
    #[must_use]
    pub fn tool(mut self, handler: Arc<dyn ToolHandler>) -> Self {
        self.tool = Some(handler);
        self
    }

    /// Attaches the [`InteractionHandler`] that serves this layer's
    /// `NeedInteraction`, making the layer *attended*.
    ///
    /// Leaving this unset keeps the layer headless; see
    /// [`attended`](Self::attended) for a readability alias.
    #[must_use]
    pub fn interaction(mut self, handler: Arc<dyn InteractionHandler>) -> Self {
        self.interaction = Some(handler);
        self
    }

    /// Alias for [`interaction`](Self::interaction) that names the intent:
    /// attaching an interaction backend makes the layer attended.
    ///
    /// This must be called explicitly — a `TestScope` is headless by default and
    /// never auto-approves interactions.
    #[must_use]
    pub fn attended(self, handler: Arc<dyn InteractionHandler>) -> Self {
        self.interaction(handler)
    }

    /// Attaches the [`SubagentHandler`] that serves this layer's `NeedSubagent`.
    #[must_use]
    pub fn subagent(mut self, handler: Arc<dyn SubagentHandler>) -> Self {
        self.subagent = Some(handler);
        self
    }

    /// Attaches the [`ReconfigHandler`] that serves this layer's
    /// `NeedReconfigRegistry`.
    #[must_use]
    pub fn reconfig(mut self, handler: Arc<dyn ReconfigHandler>) -> Self {
        self.reconfig = Some(handler);
        self
    }

    /// Wraps an inner [`HandlerScope`], delegating any family this layer does not
    /// override to it.
    ///
    /// Pass a [`ReferenceScope`](agent_lib::agent::ReferenceScope) or another
    /// [`TestScope`] to reuse its wiring while overriding individual families.
    #[must_use]
    pub fn wrapping(mut self, scope: Arc<dyn HandlerScope>) -> Self {
        self.inner = Some(scope);
        self
    }

    /// Finalises the builder into a [`TestScope`].
    #[must_use]
    pub fn build(self) -> TestScope {
        TestScope {
            llm: self.llm,
            tool: self.tool,
            interaction: self.interaction,
            subagent: self.subagent,
            reconfig: self.reconfig,
            inner: self.inner,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::TestScope;
    use crate::fixtures::{
        agent_spec_with_tools, agent_state, default_machine, root_context, tool_call, user_input,
        weather_tool,
    };
    use crate::handlers::{ScriptedLlmHandler, ScriptedToolHandler};
    use crate::ids::SeqIds;
    use crate::script::{LlmStep, ToolStep};
    use agent_lib::agent::{
        AgentError, AgentErrorKind, ApprovalRequirement, HandlerScope, RequirementKindTag,
        ToolApprovalPolicy, drain,
    };
    use agent_lib::conversation::ToolCallId;
    use agent_lib::model::tool::ToolCall;
    use std::sync::Arc;

    /// Approval policy that guards every tool call, forcing a `NeedInteraction`.
    #[derive(Debug)]
    struct RequireApprovalPolicy;

    impl ToolApprovalPolicy for RequireApprovalPolicy {
        fn approval_requirement(
            &self,
            _call_id: ToolCallId,
            _call: &ToolCall,
        ) -> ApprovalRequirement {
            ApprovalRequirement::required(Some("human approval required".to_owned()))
        }
    }

    #[test]
    fn empty_scope_serves_no_family() {
        let scope = TestScope::empty();

        assert!(scope.llm().is_none());
        assert!(scope.tool().is_none());
        assert!(scope.interaction().is_none());
        assert!(scope.subagent().is_none());
        assert!(scope.reconfig().is_none());
    }

    #[test]
    fn tool_only_scope_serves_only_the_tool_family() {
        let scope = TestScope::builder()
            .tool(Arc::new(ScriptedToolHandler::from_steps([ToolStep::ok(
                "call-weather",
                "sunny",
            )])))
            .build();

        assert!(scope.tool().is_some());
        assert!(scope.llm().is_none());
        assert!(scope.interaction().is_none());
        assert!(scope.subagent().is_none());
        assert!(scope.reconfig().is_none());
    }

    #[test]
    fn wrapping_delegates_unoverridden_families_to_the_inner_scope() {
        // The inner scope serves the tool family; the outer overrides only the
        // llm family, so `tool()` must resolve through the wrapped scope.
        let inner: Arc<dyn HandlerScope> = Arc::new(
            TestScope::builder()
                .tool(Arc::new(ScriptedToolHandler::from_steps([ToolStep::ok(
                    "call-weather",
                    "sunny",
                )])))
                .build(),
        );
        let scope = TestScope::builder()
            .llm(Arc::new(ScriptedLlmHandler::from_steps([LlmStep::text(
                "hi",
            )])))
            .wrapping(inner)
            .build();

        assert!(scope.llm().is_some());
        assert!(scope.tool().is_some());
        assert!(scope.interaction().is_none());
        assert!(scope.reconfig().is_none());
    }

    #[tokio::test]
    async fn headless_top_scope_surfaces_unhandled_interaction() {
        // A guarded tool call makes the machine emit a `NeedInteraction`. The top
        // scope is headless (llm + tool, no interaction), so with no outer layer
        // the approval must surface as a classified `UnhandledRequirement` — never
        // auto-approved — and the guarded tool never runs.
        let ids = SeqIds::new();
        let ctx = root_context(&ids);
        let spec = agent_spec_with_tools(&ids, vec![weather_tool()]);
        let mut machine = default_machine(&ids, agent_state(&ids, spec))
            .with_approval_policy(Arc::new(RequireApprovalPolicy));

        let llm = ScriptedLlmHandler::from_steps([LlmStep::tool_use(vec![tool_call(
            "call-weather",
            "get_weather",
            serde_json::json!({ "city": "SH" }),
        )])]);
        let tool = ScriptedToolHandler::from_steps([ToolStep::ok("call-weather", "sunny")]);
        let tool_log = Arc::clone(tool.log());
        let scope = TestScope::builder()
            .llm(Arc::new(llm))
            .tool(Arc::new(tool))
            .build();

        let error = drain(&mut machine, user_input(&ids, "hi"), &scope, None, &ctx)
            .await
            .expect_err("a headless top scope cannot fulfil the approval");

        assert_eq!(error.kind(), AgentErrorKind::UnhandledRequirement);
        match error {
            AgentError::UnhandledRequirement { kind, .. } => {
                assert_eq!(kind, RequirementKindTag::Interaction);
            }
            other => panic!("expected UnhandledRequirement, got {other:?}"),
        }
        // The approval was neither auto-granted nor skipped: the tool never ran.
        assert_eq!(tool_log.len(), 0);
    }
}
