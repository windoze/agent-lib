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
    InteractionResponse, PermissionRequest, PermissionResponse, RequirementResult, RunContext,
    ToolApprovalPolicy,
};
use crate::conversation::ToolCallId;
use crate::facade::run::ApprovalRequest;
use crate::model::tool::ToolCall;
use serde_json::Value;

pub use crate::agent::ApprovalDecision;

/// Key fragments that mark a tool-argument value as a likely credential.
///
/// Matched case-insensitively as substrings so `apiKey`, `AUTH_TOKEN`, and
/// `user_password` all redact. Kept deliberately broad: an over-redacted
/// summary is safe, an under-redacted one leaks secrets.
const SENSITIVE_KEY_FRAGMENTS: &[&str] = &[
    "token",
    "secret",
    "password",
    "passwd",
    "api_key",
    "apikey",
    "authorization",
    "auth",
    "credential",
    "bearer",
    "private_key",
    "access_key",
    "session",
    "cookie",
];

/// Upper bound (in bytes) on a rendered tool-input summary.
///
/// Bounds the size shipped into a [`RunEvent`](crate::facade::RunEvent) so a
/// large payload never rides along; the summary is truncated at a UTF-8
/// boundary and marked with an ellipsis when it would exceed this.
const MAX_INPUT_SUMMARY_LEN: usize = 512;

/// Builds a compact, redaction-safe summary of one tool call's arguments.
///
/// Returns `None` for a call that carried no arguments (`null`, `{}`, or `[]`).
/// Otherwise the value is rendered as compact JSON after redacting every object
/// value whose key looks like a credential (see [`SENSITIVE_KEY_FRAGMENTS`]) and
/// truncating to [`MAX_INPUT_SUMMARY_LEN`]. The result is intended for display
/// or logging, so a truncated summary may not be valid JSON.
fn summarize_tool_input(input: &Value) -> Option<String> {
    match input {
        Value::Null => return None,
        Value::Object(map) if map.is_empty() => return None,
        Value::Array(items) if items.is_empty() => return None,
        _ => {}
    }
    let mut summary = redact_value(input).to_string();
    if summary.len() > MAX_INPUT_SUMMARY_LEN {
        let mut end = MAX_INPUT_SUMMARY_LEN;
        while !summary.is_char_boundary(end) {
            end -= 1;
        }
        summary.truncate(end);
        summary.push('…');
    }
    Some(summary)
}

/// Recursively replaces credential-looking object values with `<redacted>`.
fn redact_value(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, val)| {
                    let redacted = if is_sensitive_key(key) {
                        Value::String("<redacted>".to_owned())
                    } else {
                        redact_value(val)
                    };
                    (key.clone(), redacted)
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(redact_value).collect()),
        other => other.clone(),
    }
}

/// Returns whether `key` names a likely-credential argument (case-insensitive).
fn is_sensitive_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    SENSITIVE_KEY_FRAGMENTS
        .iter()
        .any(|fragment| lower.contains(fragment))
}

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

    /// Denies every typed-tool call before it executes.
    ///
    /// A denied typed-tool call never runs; the agent feeds a denied tool result
    /// back to the model and continues the turn. The run-level
    /// [`FacadeError::ApprovalDenied`](crate::facade::FacadeError::ApprovalDenied)
    /// is reserved for managed external delegate starts refused by the approval
    /// policy.
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

/// A host-injected decider for an [`InteractionKind::Permission`] ask.
///
/// This is the formal seam for **AI-based permission** (`docs/facade-api.md`
/// §19): the facade never decides a privileged action itself, it only forwards
/// the [`PermissionRequest`] to the injected decider, which returns the
/// [`PermissionResponse`]. Installed through
/// [`ApprovalPolicy::on_permission`]; when absent the facade denies every
/// permission ask by default (§9.2). Because it is a runtime handler it is
/// shared behind an [`Arc`] and, like an `ask` approval handler, is dropped on
/// snapshot.
type PermissionDecider = Arc<dyn Fn(&PermissionRequest) -> PermissionResponse + Send + Sync>;

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
    permission_decider: Option<PermissionDecider>,
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
    /// When set, a managed external delegate is gated before it starts: the
    /// Agent facade defers to the policy default `ask` handler (denying headless)
    /// unless a tool-level override or per-tool entry for that delegate's tool
    /// name applies. A denial surfaces as
    /// [`FacadeError::ApprovalDenied`](crate::facade::FacadeError::ApprovalDenied).
    #[must_use]
    pub fn ask_external_agents(mut self) -> Self {
        self.ask_external_agents = true;
        self
    }

    /// Requires approval before an agent writes its worktree (§9.2).
    ///
    /// Recorded for host inspection; managed external delegates already run in an
    /// isolated throwaway worktree (`docs/managed-external-agent.md` §16), so this
    /// flag is advisory for the managed path.
    #[must_use]
    pub fn ask_worktree_write(mut self) -> Self {
        self.ask_worktree_write = true;
        self
    }

    /// Injects a decider for [`Permission`](crate::agent::InteractionKind::Permission)
    /// asks — the AI-based permission seam of `docs/facade-api.md` §19.
    ///
    /// A managed external runtime (Milestone 4) can pause on a
    /// [`PermissionRequest`] for a privileged action (a shell command, a file
    /// write, spawning a child, …). By default the facade **denies** every such
    /// ask (§9.2); installing a decider lets the host answer them — returning a
    /// [`PermissionResponse`] built with [`approve`](PermissionResponse::approve),
    /// [`deny`](PermissionResponse::deny), or [`cancel`](PermissionResponse::cancel).
    /// The facade re-stamps the response with the request's `action_id`, so a
    /// decider may build its response from any convenient id. The facade itself
    /// implements no decision logic; it only forwards the request.
    ///
    /// The decider only applies when no whole-agent interaction handler is
    /// injected through
    /// [`AgentBuilder::interaction_handler`](crate::facade::AgentBuilder::interaction_handler),
    /// which — when present — is the sole authority for every paused interaction.
    ///
    /// ```
    /// use agent_lib::agent::PermissionResponse;
    /// use agent_lib::facade::{Approval, ApprovalPolicy};
    ///
    /// // Approve read-only permission asks, deny everything riskier.
    /// let policy = ApprovalPolicy::new(Approval::auto_allow()).on_permission(|request| {
    ///     use agent_lib::agent::PermissionRisk;
    ///     if request.risk() <= PermissionRisk::Low {
    ///         PermissionResponse::approve(request.action_id().to_owned())
    ///     } else {
    ///         PermissionResponse::deny(request.action_id().to_owned(), None)
    ///     }
    /// });
    /// # let _ = policy;
    /// ```
    #[must_use]
    pub fn on_permission<F>(mut self, decider: F) -> Self
    where
        F: Fn(&PermissionRequest) -> PermissionResponse + Send + Sync + 'static,
    {
        self.permission_decider = Some(Arc::new(decider));
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
            permission_decider: None,
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
            .field("has_permission_decider", &self.permission_decider.is_some())
            .finish()
    }
}

/// The resolved decision for one pending tool call, recorded by the policy for
/// the interaction handler to consume.
///
/// Both variants carry the full [`ApprovalRequest`] (tool name, call id, reason,
/// and a redacted input summary) so the streaming path and any `ask` handler
/// observe the same enriched request the policy assembled.
enum PendingDecision {
    /// The call is denied; carries the request and the model-visible message.
    Deny {
        request: ApprovalRequest,
        message: Option<String>,
    },
    /// The call is deferred to a handler.
    Ask {
        request: ApprovalRequest,
        handler: Arc<AskFn>,
    },
}

impl PendingDecision {
    /// Returns the full request this pending decision concerns.
    fn request(&self) -> &ApprovalRequest {
        match self {
            Self::Deny { request, .. } | Self::Ask { request, .. } => request,
        }
    }

    /// Returns the tool name this pending decision concerns.
    fn tool_name(&self) -> &str {
        &self.request().tool_name
    }
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
    external_tools: std::collections::BTreeSet<String>,
    permission_decider: Option<PermissionDecider>,
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
            external_tools: std::collections::BTreeSet::new(),
            permission_decider: policy.permission_decider,
            pending: Mutex::new(HashMap::new()),
        }
    }

    /// Registers the model-routed tool names that start a managed external
    /// delegate (`ask_<name>`), exempting them from the machine-level approval
    /// gate (§9.2).
    ///
    /// The Agent facade drives external-delegate approval at the **drive** layer
    /// through [`resolve_external_start`](Self::resolve_external_start), so the
    /// machine gate must not double-prompt on the same call. Names registered
    /// here therefore always report [`ApprovalRequirement::AutoApprove`] from the
    /// machine's perspective; the sole authority is the drive gate.
    #[must_use]
    pub fn with_external_tools<I, S>(mut self, names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.external_tools = names.into_iter().map(Into::into).collect();
        self
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

    /// Decides synchronously whether a managed external delegate may start,
    /// applying the §9.2 default-permission table.
    ///
    /// External-delegate start is gated at the drive layer (not the machine tool
    /// gate) so a single call resolves to exactly one decision. The effective
    /// tier is chosen as:
    ///
    /// 1. an explicit tool-level override or agent per-tool entry for
    ///    `tool_name`, otherwise
    /// 2. an *ask-deferred* tier when [`ask_external_agents`] is set (defer to
    ///    the policy default handler, denying headless), otherwise
    /// 3. the policy default tier.
    ///
    /// Any [`ask`](Approval::ask) handler is invoked synchronously here. Returns
    /// `true` when the delegate may start and `false` when it is denied; a denial
    /// surfaces as
    /// [`FacadeError::ApprovalDenied`](crate::facade::FacadeError::ApprovalDenied).
    ///
    /// [`ask_external_agents`]: ApprovalPolicy::ask_external_agents
    #[must_use]
    pub fn resolve_external_start(&self, tool_name: &str) -> bool {
        let explicit = self
            .tool_overrides
            .get(tool_name)
            .or_else(|| self.per_tool.get(tool_name));
        match explicit {
            Some(approval) => self.decide_tier(tool_name, &approval.kind),
            None if self.ask_external_agents => self.decide_ask_deferred(tool_name),
            None => self.decide_tier(tool_name, &self.default.kind),
        }
    }

    /// Resolves one approval tier to an allow/deny decision for an external start.
    fn decide_tier(&self, tool_name: &str, kind: &ApprovalKind) -> bool {
        match kind {
            ApprovalKind::AutoAllow => true,
            ApprovalKind::AutoDeny => false,
            ApprovalKind::Ask(Some(handler)) => {
                let request = ApprovalRequest::for_tool(tool_name);
                handler(&request) == ApprovalDecision::Approve
            }
            ApprovalKind::Ask(None) => self.decide_ask_deferred(tool_name),
        }
    }

    /// Resolves an ask-deferred tier by deferring to the policy default handler,
    /// denying when the run is headless (no default handler).
    fn decide_ask_deferred(&self, tool_name: &str) -> bool {
        match &self.default.kind {
            ApprovalKind::Ask(Some(handler)) => {
                let request = ApprovalRequest::for_tool(tool_name);
                handler(&request) == ApprovalDecision::Approve
            }
            _ => false,
        }
    }

    /// Records the pending decision for one require-approval call.
    ///
    /// Builds the enriched [`ApprovalRequest`] (tool name, stringified
    /// `call_id`, `reason`, and a redacted input summary) once here so both the
    /// streaming emit and any `ask` handler observe the same request.
    fn record_pending(&self, call_id: ToolCallId, call: &ToolCall, reason: Option<String>) {
        let request = ApprovalRequest {
            tool_name: call.name.clone(),
            call_id: call_id.to_string(),
            reason,
            input: summarize_tool_input(&call.input),
        };
        let decision = match &self.resolve(&call.name).kind {
            // Never routed here: an auto-allow tool auto-approves in the policy.
            ApprovalKind::AutoAllow => return,
            ApprovalKind::AutoDeny => PendingDecision::Deny {
                message: Some(format!("tool `{}` denied by approval policy", call.name)),
                request,
            },
            ApprovalKind::Ask(Some(handler)) => PendingDecision::Ask {
                request,
                handler: Arc::clone(handler),
            },
            ApprovalKind::Ask(None) => match &self.default.kind {
                ApprovalKind::Ask(Some(handler)) => PendingDecision::Ask {
                    request,
                    handler: Arc::clone(handler),
                },
                _ => PendingDecision::Deny {
                    message: Some(format!(
                        "tool `{}` requires approval but no handler is configured",
                        call.name
                    )),
                    request,
                },
            },
        };
        self.pending
            .lock()
            .expect("approval pending map poisoned")
            .insert(call_id, decision);
    }

    /// Peeks the tool name recorded for a pending require-approval `call_id`.
    ///
    /// The Agent facade's streaming path uses this to label an
    /// [`ApprovalRequest`] event: an [`Approval`](InteractionKind::Approval)
    /// interaction only carries the framework [`ToolCallId`], so the tool name is
    /// recovered from the decision the policy already recorded. Returns `None`
    /// when no pending approval exists for `call_id` (for example an auto-approved
    /// call that never routed through the interaction handler).
    #[must_use]
    pub fn pending_tool_name(&self, call_id: ToolCallId) -> Option<String> {
        self.pending
            .lock()
            .expect("approval pending map poisoned")
            .get(&call_id)
            .map(|decision| decision.tool_name().to_owned())
    }

    /// Peeks the full enriched [`ApprovalRequest`] recorded for a pending
    /// require-approval `call_id`.
    ///
    /// Like [`pending_tool_name`](Self::pending_tool_name) but returns the whole
    /// request the policy assembled (tool name, `call_id`, `reason`, and a
    /// redacted input summary) so the streaming path can emit a fully populated
    /// [`RunEvent::ApprovalRequested`](crate::facade::RunEvent::ApprovalRequested).
    /// Returns `None` when no pending approval exists for `call_id`.
    #[must_use]
    pub fn pending_request(&self, call_id: ToolCallId) -> Option<ApprovalRequest> {
        self.pending
            .lock()
            .expect("approval pending map poisoned")
            .get(&call_id)
            .map(|decision| decision.request().clone())
    }
}

/// Builds the enriched [`ApprovalRequest`] a paused approval interaction should
/// surface, shared by the Agent facade's streaming tap handler and its
/// non-streaming approval recorder so the `call_id` / `reason` field mapping
/// lives in exactly one place (§9).
///
/// The tool name and redacted input summary are recovered from the pending
/// decision the [`ToolApprovalPolicy`] already recorded — peeked via
/// [`pending_request`](FacadeApproval::pending_request), *not* consumed, so the
/// fallback [`InteractionHandler`] can still remove it — while the `call_id` and
/// `reason` are re-bound from the machine-carried interaction so the request
/// reflects exactly what the machine paused on. This holds even under a
/// host-injected handler that never touches the pending map, because the machine
/// gate stays [`FacadeApproval`] and records the decision regardless of which
/// handler answers.
pub(crate) fn enriched_approval_request(
    approval: &FacadeApproval,
    call_id: ToolCallId,
    requirement: &ApprovalRequirement,
) -> ApprovalRequest {
    let mut request = approval
        .pending_request(call_id)
        .unwrap_or_else(|| ApprovalRequest::for_tool(String::new()));
    request.call_id = call_id.to_string();
    request.reason = requirement.reason().map(ToOwned::to_owned);
    request
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
            .field("external_tools", &self.external_tools)
            .finish_non_exhaustive()
    }
}

impl ToolApprovalPolicy for FacadeApproval {
    fn approval_requirement(&self, call_id: ToolCallId, call: &ToolCall) -> ApprovalRequirement {
        // External-delegate start tools are gated at the drive layer
        // (`resolve_external_start`); exempt them from the machine gate so the
        // same start is never double-prompted (§9.2).
        if self.external_tools.contains(&call.name) {
            return ApprovalRequirement::AutoApprove;
        }
        if matches!(self.resolve(&call.name).kind, ApprovalKind::AutoAllow) {
            return ApprovalRequirement::AutoApprove;
        }
        let reason = format!("approve execution of tool `{}`", call.name);
        self.record_pending(call_id, call, Some(reason.clone()));
        ApprovalRequirement::required(Some(reason))
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
                    Some(PendingDecision::Deny { message, .. }) => {
                        (ApprovalDecision::Deny, message)
                    }
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
            // Permission asks come from external runtimes (Milestone 4). A
            // host-injected decider answers them (the AI-based permission seam,
            // §19); absent one the facade denies by default until a policy opts
            // in (§9.2). The response is re-stamped with the request's
            // `action_id` so correlation always holds regardless of the id the
            // decider built its response with.
            InteractionKind::Permission { request } => {
                let response = match &self.permission_decider {
                    Some(decider) => {
                        let decided = decider(request);
                        PermissionResponse::new(
                            request.action_id().to_owned(),
                            decided.decision().clone(),
                        )
                    }
                    None => PermissionResponse::deny(
                        request.action_id().to_owned(),
                        Some("permission denied by default facade policy".to_owned()),
                    ),
                };
                InteractionResponse::Permission(response)
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

    #[tokio::test]
    async fn permission_decider_answers_permission_asks() {
        use crate::agent::{PermissionDecision, PermissionResponse};

        // A host-injected decider (the AI-based permission seam, §19) answers a
        // Permission ask instead of the default deny: here it approves low-risk
        // actions and denies the rest, keyed off the request's risk.
        let bridge = FacadeApproval::new(ApprovalPolicy::default().on_permission(|request| {
            if request.risk() <= PermissionRisk::Low {
                PermissionResponse::approve(request.action_id().to_owned())
            } else {
                PermissionResponse::deny(request.action_id().to_owned(), None)
            }
        }));
        let ctx = run_ctx();

        let low = PermissionRequest::new(
            "allow-me".to_owned(),
            AgentId::new(uuid(2)),
            PermissionCategory::Shell,
            "ls".to_owned(),
            json!({ "command": "ls" }),
            PermissionRisk::Low,
            None,
        );
        let RequirementResult::Interaction(InteractionResponse::Permission(response)) = bridge
            .fulfill(&Interaction::permission(step_id(), low), &ctx)
            .await
        else {
            panic!("expected a permission interaction response");
        };
        // The response is re-stamped with the request's action_id for correlation.
        assert_eq!(response.action_id(), "allow-me");
        assert_eq!(response.decision(), &PermissionDecision::Approve);

        let high = PermissionRequest::new(
            "deny-me".to_owned(),
            AgentId::new(uuid(2)),
            PermissionCategory::Shell,
            "rm -rf /".to_owned(),
            json!({ "command": "rm -rf /" }),
            PermissionRisk::High,
            None,
        );
        let RequirementResult::Interaction(InteractionResponse::Permission(response)) = bridge
            .fulfill(&Interaction::permission(step_id(), high), &ctx)
            .await
        else {
            panic!("expected a permission interaction response");
        };
        assert_eq!(response.action_id(), "deny-me");
        assert!(matches!(
            response.decision(),
            PermissionDecision::Deny { .. }
        ));
    }

    #[tokio::test]
    async fn permission_without_decider_denies_by_default() {
        use crate::agent::PermissionDecision;

        // Absent an injected decider, the facade keeps the default-deny behavior
        // for external Permission asks (§9.2), byte-for-byte with Milestone 4.
        let bridge = FacadeApproval::new(ApprovalPolicy::default());
        let ctx = run_ctx();
        let request = PermissionRequest::new(
            "action-1".to_owned(),
            AgentId::new(uuid(2)),
            PermissionCategory::Shell,
            "rm -rf".to_owned(),
            json!({ "command": "rm -rf" }),
            PermissionRisk::High,
            None,
        );
        let RequirementResult::Interaction(InteractionResponse::Permission(response)) = bridge
            .fulfill(&Interaction::permission(step_id(), request), &ctx)
            .await
        else {
            panic!("expected a permission interaction response");
        };
        assert_eq!(response.action_id(), "action-1");
        assert!(matches!(
            response.decision(),
            PermissionDecision::Deny { .. }
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

    #[test]
    fn pending_request_carries_enriched_fields_and_redacts_secrets() {
        let bridge = FacadeApproval::new(ApprovalPolicy::new(Approval::auto_deny()));
        let id = call_id();
        let call = ToolCall {
            id: "call-x".to_owned(),
            name: "deploy".to_owned(),
            input: json!({ "region": "us", "api_key": "sk-secret", "token": "abc" }),
        };
        bridge.approval_requirement(id, &call);

        let request = bridge
            .pending_request(id)
            .expect("a pending require-approval decision was recorded");
        assert_eq!(request.tool_name, "deploy");
        assert_eq!(request.call_id, id.to_string());
        assert_eq!(
            request.reason.as_deref(),
            Some("approve execution of tool `deploy`")
        );

        let input = request.input.expect("a non-empty input is summarized");
        assert!(
            input.contains("\"region\":\"us\""),
            "keeps benign args: {input}"
        );
        assert!(
            !input.contains("sk-secret"),
            "redacts credential values: {input}"
        );
        assert!(!input.contains("abc"), "redacts token values: {input}");
        assert!(
            input.matches("<redacted>").count() == 2,
            "both sensitive keys are redacted: {input}"
        );
    }

    #[test]
    fn pending_request_omits_input_for_argless_call() {
        let bridge = FacadeApproval::new(ApprovalPolicy::new(Approval::auto_deny()));
        let id = call_id();
        bridge.approval_requirement(id, &call("noop"));

        let request = bridge
            .pending_request(id)
            .expect("a pending decision was recorded");
        assert_eq!(
            request.input, None,
            "an empty `{{}}` input summarizes to None"
        );
        assert_eq!(bridge.pending_tool_name(id).as_deref(), Some("noop"));
    }

    #[test]
    fn input_summary_is_size_bounded() {
        let bridge = FacadeApproval::new(ApprovalPolicy::new(Approval::auto_deny()));
        let id = call_id();
        let big = "x".repeat(4_096);
        let call = ToolCall {
            id: "call-big".to_owned(),
            name: "write".to_owned(),
            input: json!({ "blob": big }),
        };
        bridge.approval_requirement(id, &call);

        let input = bridge
            .pending_request(id)
            .and_then(|request| request.input)
            .expect("a non-empty input is summarized");
        assert!(
            input.chars().count() <= super::MAX_INPUT_SUMMARY_LEN + 1,
            "the summary is truncated to a bounded length (plus one ellipsis char): {}",
            input.chars().count()
        );
        assert!(
            input.ends_with('…'),
            "a truncated summary is marked: {input}"
        );
    }
}
