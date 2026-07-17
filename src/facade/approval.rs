//! Approval policy for the Agent facade.
//!
//! This module packages the effect-model approval primitives behind two
//! approachable types described in `docs/facade-api.md` §9:
//!
//! - [`Approval`] — the three simple tiers a caller reaches for first:
//!   [`auto_allow`](Approval::auto_allow), [`auto_deny`](Approval::auto_deny),
//!   and [`ask`](Approval::ask) (delegate the decision to a handler).
//! - [`ApprovalPolicy`] — an agent-level builder that maps individual tools onto
//!   those tiers ([`allow_tool`](ApprovalPolicy::allow_tool) /
//!   [`ask_tool`](ApprovalPolicy::ask_tool) / [`deny_tool`](ApprovalPolicy::deny_tool))
//!   and records the external-agent / worktree-write flags of §9.2.
//!
//! # How it maps onto the effect model
//!
//! A facade approval is bridged into [`FacadeApproval`], which implements **both**
//! runtime traits the sans-io machine drives against:
//!
//! - [`ToolApprovalPolicy`] decides, per tool
//!   call, whether execution may proceed unattended
//!   ([`AutoApprove`](crate::agent::ApprovalRequirement::AutoApprove)) or must
//!   pause for a decision
//!   ([`RequireApproval`](crate::agent::ApprovalRequirement::RequireApproval)).
//! - [`InteractionHandler`] answers the paused
//!   [`Approval`](crate::agent::InteractionKind::Approval) interaction: an
//!   [`auto_deny`](Approval::auto_deny) yields a deny, an [`ask`](Approval::ask)
//!   invokes the caller's handler, and a headless `ask` with no handler denies
//!   rather than blocking (§9.2).
//!
//! An [`InteractionKind::Approval`] only
//! carries the framework tool-call id, not the tool name, so the two trait
//! implementations correlate through a shared pending map keyed by
//! [`ToolCallId`]: the machine always consults the policy (which records the
//! resolved decision) before it emits the interaction the handler consumes.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use crate::agent::{
    ApprovalRequirement, ApprovalResponse, Interaction, InteractionHandler, InteractionKind,
    InteractionResponse, PermissionResponse, RequirementResult, RunContext, ToolApprovalPolicy,
};
use crate::conversation::ToolCallId;
use crate::facade::run::ApprovalRequest;
use crate::model::tool::ToolCall;

pub use crate::agent::ApprovalDecision;

/// A user-supplied callback that decides one pending tool approval.
///
/// It receives the facade [`ApprovalRequest`] describing the tool awaiting
/// approval and returns the [`ApprovalDecision`] to apply.
type AskFn = dyn Fn(&ApprovalRequest) -> ApprovalDecision + Send + Sync;

/// One of the three simple approval tiers (`docs/facade-api.md` §9.1).
///
/// Used both as a whole-agent default (`.approval(Approval::auto_allow())`) and
/// as a per-tool override (`Tool::function(..).approval(Approval::ask(..))`).
///
/// ```
/// use agent_lib::facade::{Approval, ApprovalDecision};
///
/// let allow = Approval::auto_allow();
/// let deny = Approval::auto_deny();
/// let ask = Approval::ask(|req| {
///     if req.tool_name == "get_weather" {
///         ApprovalDecision::Approve
///     } else {
///         ApprovalDecision::Deny
///     }
/// });
/// # let _ = (allow, deny, ask);
/// ```
#[derive(Clone)]
pub struct Approval {
    kind: ApprovalKind,
}

/// The internal representation of an [`Approval`] tier.
#[derive(Clone)]
enum ApprovalKind {
    /// Execute the tool without pausing.
    AutoAllow,
    /// Never execute the tool; deny it before it starts.
    AutoDeny,
    /// Pause and resolve through a handler. `None` means "ask, but this tier
    /// carries no handler of its own" — used by
    /// [`ApprovalPolicy::ask_tool`], which defers to the policy default's
    /// handler and otherwise denies in a headless run.
    Ask(Option<Arc<AskFn>>),
}

impl Approval {
    /// Approves every tool call without pausing.
    #[must_use]
    pub fn auto_allow() -> Self {
        Self {
            kind: ApprovalKind::AutoAllow,
        }
    }

    /// Denies every tool call before it executes.
    ///
    /// A denied call never runs; the run surfaces
    /// [`FacadeError::ApprovalDenied`](crate::facade::FacadeError::ApprovalDenied).
    #[must_use]
    pub fn auto_deny() -> Self {
        Self {
            kind: ApprovalKind::AutoDeny,
        }
    }

    /// Delegates each decision to `handler`.
    ///
    /// The handler is called with the [`ApprovalRequest`] for the pending tool
    /// call and returns the [`ApprovalDecision`] to apply. It must be cheap and
    /// non-blocking-friendly (it runs on the drive task).
    #[must_use]
    pub fn ask<F>(handler: F) -> Self
    where
        F: Fn(&ApprovalRequest) -> ApprovalDecision + Send + Sync + 'static,
    {
        Self {
            kind: ApprovalKind::Ask(Some(Arc::new(handler))),
        }
    }

    /// Internal "ask, deferring to the policy default handler" tier.
    fn ask_default() -> Self {
        Self {
            kind: ApprovalKind::Ask(None),
        }
    }
}

impl fmt::Debug for Approval {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let tier = match &self.kind {
            ApprovalKind::AutoAllow => "auto_allow",
            ApprovalKind::AutoDeny => "auto_deny",
            ApprovalKind::Ask(Some(_)) => "ask(handler)",
            ApprovalKind::Ask(None) => "ask(default)",
        };
        formatter.debug_tuple("Approval").field(&tier).finish()
    }
}

/// Agent-level approval configuration (`docs/facade-api.md` §9.1–§9.2).
///
/// A policy pairs a whole-agent [`default`](ApprovalPolicy::new) tier with
/// per-tool overrides plus the two coarse flags from the §9.2 default-permission
/// table. It accepts an [`Approval`] via [`From`], so an agent builder's
/// `.approval(..)` can take either a bare [`Approval`] or a full policy.
///
/// ```
/// use agent_lib::facade::{Approval, ApprovalPolicy};
///
/// let policy = ApprovalPolicy::default()
///     .allow_tool("get_weather")
///     .ask_tool("shell")
///     .ask_external_agents()
///     .ask_worktree_write();
/// assert!(policy.requires_ask_external_agents());
/// # let _ = policy;
/// ```
#[derive(Clone)]
pub struct ApprovalPolicy {
    default: Approval,
    per_tool: BTreeMap<String, Approval>,
    ask_external_agents: bool,
    ask_worktree_write: bool,
}

impl Default for ApprovalPolicy {
    /// A policy whose default tier is [`Approval::auto_allow`].
    ///
    /// Typed tools are ordinary Rust functions the caller wrote, so they run by
    /// default; opt individual tools into approval with
    /// [`ask_tool`](Self::ask_tool) / [`deny_tool`](Self::deny_tool), or set a
    /// stricter default with [`new`](Self::new).
    fn default() -> Self {
        Self::from(Approval::auto_allow())
    }
}

impl ApprovalPolicy {
    /// Creates a policy with an explicit whole-agent default tier.
    #[must_use]
    pub fn new(default: Approval) -> Self {
        Self::from(default)
    }

    /// Auto-allows the named tool.
    #[must_use]
    pub fn allow_tool(mut self, name: impl Into<String>) -> Self {
        self.per_tool.insert(name.into(), Approval::auto_allow());
        self
    }

    /// Requires approval for the named tool.
    ///
    /// The decision is resolved by the policy default's handler when one exists
    /// (`ApprovalPolicy::new(Approval::ask(..))`); in a headless run with no
    /// handler the call is denied rather than left blocking (§9.2).
    #[must_use]
    pub fn ask_tool(mut self, name: impl Into<String>) -> Self {
        self.per_tool.insert(name.into(), Approval::ask_default());
        self
    }

    /// Denies the named tool outright.
    #[must_use]
    pub fn deny_tool(mut self, name: impl Into<String>) -> Self {
        self.per_tool.insert(name.into(), Approval::auto_deny());
        self
    }

    /// Sets an explicit [`Approval`] tier (including an `ask` handler) for one
    /// tool.
    #[must_use]
    pub fn tool(mut self, name: impl Into<String>, approval: Approval) -> Self {
        self.per_tool.insert(name.into(), approval);
        self
    }

    /// Requires approval before a managed external agent runs (§9.2).
    ///
    /// Recorded now; enforced when the managed-external stack lands (Milestone 4).
    #[must_use]
    pub fn ask_external_agents(mut self) -> Self {
        self.ask_external_agents = true;
        self
    }

    /// Requires approval before an agent writes its worktree (§9.2).
    ///
    /// Recorded now; enforced when the managed-external stack lands (Milestone 4).
    #[must_use]
    pub fn ask_worktree_write(mut self) -> Self {
        self.ask_worktree_write = true;
        self
    }

    /// Returns whether managed external agents require approval.
    #[must_use]
    pub const fn requires_ask_external_agents(&self) -> bool {
        self.ask_external_agents
    }

    /// Returns whether worktree writes require approval.
    #[must_use]
    pub const fn requires_ask_worktree_write(&self) -> bool {
        self.ask_worktree_write
    }
}

impl From<Approval> for ApprovalPolicy {
    fn from(default: Approval) -> Self {
        Self {
            default,
            per_tool: BTreeMap::new(),
            ask_external_agents: false,
            ask_worktree_write: false,
        }
    }
}

impl fmt::Debug for ApprovalPolicy {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ApprovalPolicy")
            .field("default", &self.default)
            .field("per_tool", &self.per_tool)
            .field("ask_external_agents", &self.ask_external_agents)
            .field("ask_worktree_write", &self.ask_worktree_write)
            .finish()
    }
}

/// The resolved decision for one pending tool call, recorded by the policy for
/// the interaction handler to consume.
enum PendingDecision {
    /// The call is denied; carries the model-visible message.
    Deny { message: Option<String> },
    /// The call is deferred to a handler.
    Ask {
        request: ApprovalRequest,
        handler: Arc<AskFn>,
    },
}

/// Bridges a facade [`ApprovalPolicy`] (plus tool-level overrides) onto the
/// runtime approval traits.
///
/// One value implements **both** [`ToolApprovalPolicy`] and
/// [`InteractionHandler`]; share it with the machine and the drive scope through
/// a single [`Arc`] so the two roles observe the same pending-decision map. The
/// Agent facade assembles this from the agent policy and each
/// [`Tool`](crate::facade::Tool)'s
/// [`approval_override`](crate::facade::Tool::approval_override) (Milestone 2-3),
/// but it is public so advanced callers wiring the layers by hand can reuse it.
pub struct FacadeApproval {
    default: Approval,
    per_tool: BTreeMap<String, Approval>,
    tool_overrides: BTreeMap<String, Approval>,
    ask_external_agents: bool,
    ask_worktree_write: bool,
    pending: Mutex<HashMap<ToolCallId, PendingDecision>>,
}

impl FacadeApproval {
    /// Builds a bridge from an agent-level [`ApprovalPolicy`].
    #[must_use]
    pub fn new(policy: ApprovalPolicy) -> Self {
        Self {
            default: policy.default,
            per_tool: policy.per_tool,
            tool_overrides: BTreeMap::new(),
            ask_external_agents: policy.ask_external_agents,
            ask_worktree_write: policy.ask_worktree_write,
            pending: Mutex::new(HashMap::new()),
        }
    }

    /// Registers a tool-level [`Approval`] override.
    ///
    /// A tool-level override wins over any agent-level per-tool entry and over
    /// the policy default for the same tool name (`docs/facade-api.md` §9.1).
    #[must_use]
    pub fn with_tool_override(mut self, name: impl Into<String>, approval: Approval) -> Self {
        self.tool_overrides.insert(name.into(), approval);
        self
    }

    /// Returns whether managed external agents require approval (§9.2).
    #[must_use]
    pub const fn asks_external_agents(&self) -> bool {
        self.ask_external_agents
    }

    /// Returns whether worktree writes require approval (§9.2).
    #[must_use]
    pub const fn asks_worktree_write(&self) -> bool {
        self.ask_worktree_write
    }

    /// Resolves the effective tier for `tool_name` (override > per-tool > default).
    fn resolve(&self, tool_name: &str) -> &Approval {
        self.tool_overrides
            .get(tool_name)
            .or_else(|| self.per_tool.get(tool_name))
            .unwrap_or(&self.default)
    }

    /// Records the pending decision for one require-approval call.
    fn record_pending(&self, call_id: ToolCallId, tool_name: &str) {
        let decision = match &self.resolve(tool_name).kind {
            // Never routed here: an auto-allow tool auto-approves in the policy.
            ApprovalKind::AutoAllow => return,
            ApprovalKind::AutoDeny => PendingDecision::Deny {
                message: Some(format!("tool `{tool_name}` denied by approval policy")),
            },
            ApprovalKind::Ask(Some(handler)) => PendingDecision::Ask {
                request: ApprovalRequest {
                    tool_name: tool_name.to_owned(),
                },
                handler: Arc::clone(handler),
            },
            ApprovalKind::Ask(None) => match &self.default.kind {
                ApprovalKind::Ask(Some(handler)) => PendingDecision::Ask {
                    request: ApprovalRequest {
                        tool_name: tool_name.to_owned(),
                    },
                    handler: Arc::clone(handler),
                },
                _ => PendingDecision::Deny {
                    message: Some(format!(
                        "tool `{tool_name}` requires approval but no handler is configured"
                    )),
                },
            },
        };
        self.pending
            .lock()
            .expect("approval pending map poisoned")
            .insert(call_id, decision);
    }
}

impl fmt::Debug for FacadeApproval {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FacadeApproval")
            .field("default", &self.default)
            .field("per_tool", &self.per_tool)
            .field("tool_overrides", &self.tool_overrides)
            .field("ask_external_agents", &self.ask_external_agents)
            .field("ask_worktree_write", &self.ask_worktree_write)
            .finish_non_exhaustive()
    }
}

impl ToolApprovalPolicy for FacadeApproval {
    fn approval_requirement(&self, call_id: ToolCallId, call: &ToolCall) -> ApprovalRequirement {
        if matches!(self.resolve(&call.name).kind, ApprovalKind::AutoAllow) {
            return ApprovalRequirement::AutoApprove;
        }
        self.record_pending(call_id, &call.name);
        ApprovalRequirement::required(Some(format!("approve execution of tool `{}`", call.name)))
    }
}

#[async_trait]
impl InteractionHandler for FacadeApproval {
    async fn fulfill(&self, request: &Interaction, _ctx: &RunContext) -> RequirementResult {
        let response = match request.kind() {
            InteractionKind::Approval { call_id, .. } => {
                let pending = self
                    .pending
                    .lock()
                    .expect("approval pending map poisoned")
                    .remove(call_id);
                let (decision, message) = match pending {
                    Some(PendingDecision::Deny { message }) => (ApprovalDecision::Deny, message),
                    Some(PendingDecision::Ask { request, handler }) => (handler(&request), None),
                    None => (
                        ApprovalDecision::Deny,
                        Some("no pending approval decision for this tool call".to_owned()),
                    ),
                };
                InteractionResponse::Approval(ApprovalResponse::new(
                    request.step_id(),
                    *call_id,
                    decision,
                    message,
                ))
            }
            // The default machine never emits these; answer trivially in-family
            // so the result still type-aligns with the requirement.
            InteractionKind::Question { .. } => InteractionResponse::answer(String::new()),
            InteractionKind::Choice { .. } => InteractionResponse::Choice(0),
            // Permission asks come from external runtimes (Milestone 4); deny by
            // default until a policy opts in (§9.2).
            InteractionKind::Permission { request } => {
                InteractionResponse::Permission(PermissionResponse::deny(
                    request.action_id().to_owned(),
                    Some("permission denied by default facade policy".to_owned()),
                ))
            }
        };
        RequirementResult::Interaction(response)
    }
}

#[cfg(test)]
mod tests {
    use super::{Approval, ApprovalDecision, ApprovalPolicy, FacadeApproval};
    use crate::agent::{
        AgentId, ApprovalRequirement, BudgetLimits, Interaction, InteractionHandler,
        InteractionResponse, PermissionCategory, PermissionRequest, PermissionRisk,
        RequirementResult, RunContext, RunId, StepId, ToolApprovalPolicy, TraceNodeId,
    };
    use crate::conversation::ToolCallId;
    use crate::model::tool::ToolCall;
    use serde_json::json;
    use uuid::Uuid;

    fn uuid(seed: u128) -> Uuid {
        Uuid::from_u128(seed)
    }

    fn step_id() -> StepId {
        StepId::new(uuid(10))
    }

    fn call_id() -> ToolCallId {
        ToolCallId::new(uuid(11))
    }

    fn call(name: &str) -> ToolCall {
        ToolCall {
            id: format!("call-{name}"),
            name: name.to_owned(),
            input: json!({}),
        }
    }

    fn run_ctx() -> RunContext {
        RunContext::new_root(
            RunId::new(uuid(1)),
            BudgetLimits::unbounded(),
            TraceNodeId::new("approval-test"),
        )
    }

    fn approval_interaction(id: ToolCallId) -> Interaction {
        Interaction::approval(
            step_id(),
            id,
            ApprovalRequirement::required(Some("reason".to_owned())),
        )
    }

    async fn decision_for(bridge: &FacadeApproval, id: ToolCallId) -> ApprovalDecision {
        let ctx = run_ctx();
        let RequirementResult::Interaction(InteractionResponse::Approval(response)) =
            bridge.fulfill(&approval_interaction(id), &ctx).await
        else {
            panic!("expected an approval interaction response");
        };
        response.decision()
    }

    #[test]
    fn auto_allow_never_pauses() {
        let bridge = FacadeApproval::new(ApprovalPolicy::default());
        assert_eq!(
            bridge.approval_requirement(call_id(), &call("get_weather")),
            ApprovalRequirement::AutoApprove
        );
    }

    #[tokio::test]
    async fn auto_deny_requires_then_denies() {
        let bridge = FacadeApproval::new(ApprovalPolicy::new(Approval::auto_deny()));
        let id = call_id();
        assert!(matches!(
            bridge.approval_requirement(id, &call("shell")),
            ApprovalRequirement::RequireApproval { .. }
        ));
        assert_eq!(decision_for(&bridge, id).await, ApprovalDecision::Deny);
    }

    #[tokio::test]
    async fn ask_invokes_handler_and_applies_its_decision() {
        let bridge = FacadeApproval::new(ApprovalPolicy::new(Approval::ask(|req| {
            if req.tool_name == "safe" {
                ApprovalDecision::Approve
            } else {
                ApprovalDecision::Deny
            }
        })));

        let allow_id = call_id();
        bridge.approval_requirement(allow_id, &call("safe"));
        assert_eq!(
            decision_for(&bridge, allow_id).await,
            ApprovalDecision::Approve
        );

        let deny_id = ToolCallId::new(uuid(12));
        bridge.approval_requirement(deny_id, &call("danger"));
        assert_eq!(decision_for(&bridge, deny_id).await, ApprovalDecision::Deny);
    }

    #[tokio::test]
    async fn tool_level_override_beats_agent_policy() {
        // Agent policy asks (via a handler that would approve), but the tool-level
        // override denies outright — the override must win.
        let bridge = FacadeApproval::new(
            ApprovalPolicy::default().tool("shell", Approval::ask(|_| ApprovalDecision::Approve)),
        )
        .with_tool_override("shell", Approval::auto_deny());

        let id = call_id();
        assert!(matches!(
            bridge.approval_requirement(id, &call("shell")),
            ApprovalRequirement::RequireApproval { .. }
        ));
        assert_eq!(decision_for(&bridge, id).await, ApprovalDecision::Deny);
    }

    #[tokio::test]
    async fn headless_ask_without_handler_denies_and_does_not_block() {
        // `ask_tool` with a default that has no handler is headless: it must deny
        // rather than wait for input that cannot arrive.
        let bridge = FacadeApproval::new(ApprovalPolicy::default().ask_tool("shell"));
        let id = call_id();
        assert!(matches!(
            bridge.approval_requirement(id, &call("shell")),
            ApprovalRequirement::RequireApproval { .. }
        ));
        assert_eq!(decision_for(&bridge, id).await, ApprovalDecision::Deny);
    }

    #[tokio::test]
    async fn ask_tool_falls_back_to_default_handler() {
        let bridge = FacadeApproval::new(
            ApprovalPolicy::new(Approval::ask(|_| ApprovalDecision::Approve)).ask_tool("shell"),
        );
        let id = call_id();
        bridge.approval_requirement(id, &call("shell"));
        assert_eq!(decision_for(&bridge, id).await, ApprovalDecision::Approve);
    }

    #[tokio::test]
    async fn non_approval_interactions_get_safe_defaults() {
        let bridge = FacadeApproval::new(ApprovalPolicy::default());
        let ctx = run_ctx();

        let question = Interaction::question(step_id(), "which?".to_owned());
        assert!(matches!(
            bridge.fulfill(&question, &ctx).await,
            RequirementResult::Interaction(InteractionResponse::Answer(text)) if text.is_empty()
        ));

        let choice = Interaction::choice(
            step_id(),
            "pick".to_owned(),
            vec!["a".to_owned(), "b".to_owned()],
        );
        assert!(matches!(
            bridge.fulfill(&choice, &ctx).await,
            RequirementResult::Interaction(InteractionResponse::Choice(0))
        ));

        let request = PermissionRequest::new(
            "action-1".to_owned(),
            AgentId::new(uuid(2)),
            PermissionCategory::Shell,
            "rm -rf".to_owned(),
            json!({ "command": "rm -rf" }),
            PermissionRisk::High,
            None,
        );
        let permission = Interaction::permission(step_id(), request);
        assert!(matches!(
            bridge.fulfill(&permission, &ctx).await,
            RequirementResult::Interaction(InteractionResponse::Permission(_))
        ));
    }

    #[test]
    fn policy_from_approval_and_flags() {
        let policy = ApprovalPolicy::from(Approval::auto_deny())
            .ask_external_agents()
            .ask_worktree_write();
        assert!(policy.requires_ask_external_agents());
        assert!(policy.requires_ask_worktree_write());

        let bridge = FacadeApproval::new(policy);
        assert!(bridge.asks_external_agents());
        assert!(bridge.asks_worktree_write());
    }
}
