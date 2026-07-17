# M3-3 Plan — `Delegation` config + multi-delegate + delegate/pending snapshot

Task: **M3-3** in `TODO.md` (first incomplete). Docs: `docs/facade-api.md`
§10.2, §13.1, §15.2; `PLAN.md` R5 (task brief not persisted).

## Deliverables
1. `Delegation` config type (`src/facade/delegate.rs`):
   - `model_routed()` (default; one `ask_<name>` tool per delegate) with
     `.expose_subagents_as_tools()` / `.expose_as_tools()` (idempotent refiners).
   - `single_tool(name)` (unified `<name>(agent, task)` tool that routes by the
     `agent` argument).
   - `AgentBuilder::delegation(..)` wiring; stored on `Agent`.
2. Multiple subagents: each exposes an independent tool (model-routed) or the
   unified tool routes by `agent` (single-tool). **Name collisions rejected at
   build time** (`FacadeError::DuplicateTool`).
3. Extend `AgentSnapshot`:
   - `delegates: Vec<DelegateSnapshot>` — data-only spec (name, description,
     spec, tools, inherit_model); approval omitted (runtime handle).
   - `pending_delegations: Vec<DelegationSnapshot>` — in-progress child
     `ConversationSnapshot` + delegate name (capturable + restorable; the
     synchronous one-shot drive leaves none at a committed point, so it is empty
     in normal capture, documented).
   - persist the `Delegation` mode so restore re-routes correctly.
   - `restore()` rebuilds delegates (and can rebuild child machine from a pending
     snapshot). Task brief is never added to the persistent snapshot (R5).
4. rustdoc complete; update §15.2 doc to match the extended snapshot.

## Key facts
- `DefaultAgentMachine` only emits `NeedTool`; delegation intercepted at the
  `NeedTool` boundary by `DelegationToolHandler`. Refactor its `delegates` map to
  a `DelegationRoute` enum knowing the mode.
- Delegation tool declarations are baked into the supervisor `AgentSpec` tool set
  at build time (LLM tools come from machine state), so build appends per mode.
- `AgentSpec`/`ToolSetRef`/`ConversationSnapshot` are Serialize; `AgentSpec` is
  not `Eq` (f32 temperature) → `DelegateSnapshot` is PartialEq only.
- `LocalSubagent` is not Serialize (approval handler); snapshot uses its public
  accessors + a `pub(crate)` `from_parts` constructor for rebuild.

## Steps
1. delegate.rs: `Delegation`/`DelegationMode` (serde), `expose_*`,
   `delegation_single_tool_declaration`, `DelegationRoute` + builder,
   `LocalSubagent::from_parts`. Refactor `DelegationToolHandler` to route.
2. tool.rs: `ensure_unique_declaration_names(&[ToolDecl])`.
3. agent.rs: `AgentBuilder.delegation` field + `.delegation(..)`; build appends
   per mode + collision check; `Agent.delegation` field + accessor;
   `delegation_route()` replaces `delegate_table()`; pass to handler in run_full;
   `into_parts`/snapshot pass delegates+delegation.
4. agent/snapshot.rs: populate `DelegateSnapshot`/`DelegationSnapshot`; persist
   `delegation`; `capture(state, delegates, delegation)`; restore rebuilds
   delegates + delegation; optional `.subagent(..)` override on restore builder.
5. agent/stream.rs: use `delegation_route()`.
6. mod.rs/prelude: export `Delegation`.
7. Tests (delegate.rs offline): two subagents each callable; single_tool routes
   by arg; duplicate-name build error; snapshot carries delegates + no brief;
   restore re-delegates; DelegationSnapshot round-trip rebuilds child.
8. Validate seq 1-6, mark [DONE], commit, STOP.

## Status
- [x] Implement (all files wired; library + tests compile clean)
- [x] Tests + validation seq 1-6 (fmt; clippy default + external features clean;
      `cargo test -p agent-lib --lib facade::delegate` 16 passed incl. 6 new;
      `cargo test --all --all-targets` all green, lib 753 passed;
      `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` clean;
      `git diff --check` clean)
- [x] Mark [DONE] in TODO.md + completion record; commit next, then STOP
