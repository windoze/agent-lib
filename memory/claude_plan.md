# M2-2 `Approval` three-tier + `ApprovalPolicy` -> `ToolApprovalPolicy`/`InteractionHandler`

## Task (TODO.md M2-2)
Build `src/facade/approval.rs`: three-tier `Approval` (auto_allow/auto_deny/ask) +
`ApprovalPolicy` builder. Bridge into adapters implementing `agent::ToolApprovalPolicy`
(produces `ApprovalRequirement` per tool name/policy) and `agent::InteractionHandler`
(auto_allow/auto_deny produce `ApprovalResponse` directly; `ask` calls the user handler;
non-Approval `InteractionKind` -> reasonable default or deny). headless (ask but no handler)
-> deny (surfaces as `FacadeError::ApprovalDenied`), never hangs. Add tool-level override
`Tool::function(..).approval(..)` with priority over agent-level. Full rustdoc.

## Anchors (verified)
- `agent::{ToolApprovalPolicy, ApprovalRequirement, ApprovalResponse, ApprovalDecision,
  Interaction, InteractionHandler, InteractionKind, InteractionResponse, PermissionResponse,
  RequirementResult, RunContext}`. Reference: `ApprovalInteractionHandler`, example
  `RequireApproval`/`StdinApproval` in examples/agent_chat.rs.
- `ToolApprovalPolicy::approval_requirement(&self, ToolCallId, &ToolCall) -> ApprovalRequirement`
  (requires `fmt::Debug`). `InteractionHandler::fulfill(&self, &Interaction, &RunContext)
  -> RequirementResult` (`#[async_trait]`). `InteractionKind::Approval { call_id, requirement }`
  carries NO tool name -> correlate via shared pending map keyed by `ToolCallId`.
- Machine always calls the policy BEFORE the interaction handler for the same call_id
  (tool-use -> approval_requirement -> RequireApproval -> NeedInteraction -> fulfill). So the
  policy populates the pending map and the interaction handler consumes it.
- `facade::run::ApprovalRequest { tool_name }` (non_exhaustive; constructible in-crate).

## Design (`src/facade/approval.rs`)
1. `pub use crate::agent::ApprovalDecision;`
2. `Approval` (Clone, manual Debug): enum kind AutoAllow / AutoDeny / Ask(Arc<AskFn>).
   `AskFn = dyn Fn(&ApprovalRequest) -> ApprovalDecision + Send + Sync`. ctors
   `auto_allow()/auto_deny()/ask(handler)`.
3. `ApprovalPolicy` (Clone, manual Debug): { default: Approval, per_tool: BTreeMap<String,
   Approval>, ask_external_agents: bool, ask_worktree_write: bool }. `Default` -> default =
   auto_allow (typed tools are user code, trusted). Builder: `new(Approval)`, `allow_tool`,
   `ask_tool` (Ask with no handler -> falls back to default handler else headless deny),
   `deny_tool`, `tool(name, Approval)`, `ask_external_agents`, `ask_worktree_write`. Getters
   for the two flags. `impl From<Approval> for ApprovalPolicy` (so AgentBuilder `.approval(..)`
   accepts both `Approval` and `ApprovalPolicy` via `Into`).
4. `FacadeApproval` implements BOTH `ToolApprovalPolicy` and `InteractionHandler`, shared via
   `Arc`. Holds resolved config (default + per_tool + tool_overrides + flags) and
   `Mutex<HashMap<ToolCallId, PendingDecision>>`. Ctor `new(ApprovalPolicy)` +
   `with_tool_override(name, Approval)` (tool-level, highest priority; M2-3 feeds Tool.approval).
   - resolve(name) = tool_overrides > per_tool > default.
   - `approval_requirement`: AutoAllow -> AutoApprove; else RequireApproval{reason} and store
     PendingDecision (Deny{msg} for auto_deny / headless-ask, Ask{request,handler} otherwise).
   - `fulfill`: Approval -> pop PendingDecision, produce ApprovalResponse (deny/ask handler);
     Question -> empty answer; Choice -> index 0; Permission -> deny (safe default, M4 refines).
   - Manual Debug (closures not Debug).
5. `Tool` (tool.rs): add `approval: Option<Approval>`, `.approval(self, Approval) -> Self`,
   `approval_override(&self) -> Option<&Approval>`. Update Debug to include has_approval.
6. `FacadeError`: add unit variants `ApprovalDenied`, `PermissionDenied` (spec §16). Add a
   helper to classify a denied approval decision (used by M2-3 run path).
7. Exports in facade/mod.rs + module rustdoc note.

## Tests (offline, in src/facade/approval.rs)
- auto_allow -> AutoApprove requirement.
- auto_deny -> RequireApproval + fulfill yields Deny response.
- ask -> RequireApproval + fulfill calls handler, returns its decision (approve & deny cases).
- tool-level override beats agent per_tool (with_tool_override wins).
- ask_tool with no handler and default auto_allow -> headless deny (fulfill returns Deny, no hang).
- ask_tool with default = ask(handler) -> uses default handler.
- non-Approval kinds: Question -> empty answer; Choice -> 0; Permission -> deny.
- ApprovalPolicy::from(Approval) sets default; flags stored/readable.

## Validation
1. cargo fmt --all
2. cargo clippy --all-targets -- -D warnings  (+ --features facade-schema for the CLI-free set touched)
3. cargo test -p agent-lib facade::approval  (focused) then cargo test --all --all-targets (<=30 min)
4. RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
5. git diff --check

## Status: DONE
- All validation green: fmt; clippy default + facade-schema (0 warnings); facade::approval 8 tests;
  full suite (agent-lib lib 720); doc default + facade-schema; doctests 14/15; git diff --check clean.
- TODO.md M2-2 marked [DONE] with completion record. No PLAN.md phase change; no new prerequisite tasks.
