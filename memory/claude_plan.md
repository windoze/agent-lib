# M2-1 typed function `Tool` + `ToolContext` + internal `ToolRegistry` bridge

## Task (TODO.md M2-1)
Build `src/facade/tool.rs`: facade `Tool` (typed function tool), `ToolContext`, and an
internal adapter bridging a set of facade tools into `agent::ToolRegistry`. Resolve the
schemars R1 hard-decision. Build-time name-conflict check across typed tools + escape-hatch
registry/declarations → `FacadeError`.

## R1 schema decision (chosen)
- `schemars` is **not** a core dependency (only transitive under `external-acp`).
- Add an **off-by-default optional feature `facade-schema`** → `schemars = { version = "1", optional = true }`.
  - With `facade-schema`: `Tool::function(name, desc, handler)` where `Args: DeserializeOwned + JsonSchema`
    derives the JSON schema (matches docs §7.1 exactly). Strip the top-level `$schema` meta key.
  - Always available (no feature): `Tool::function_with_schema(name, desc, input_schema: Value, handler)`
    — explicit-schema degraded path.
- Document in rustdoc + PLAN.md (R1) + TODO.md completion record. Default build pulls no schemars.
- Verified: schemars 1.x `schema_for!(T)` works with a generic type param; blanket
  `impl<T: Serialize> IntoToolResult` + `impl IntoToolResult for ToolResult` (non-Serialize) compiles.

## Design (`src/facade/tool.rs`)
1. `ToolContext { run_id, agent_id, tool_call_id, worktree, cancel, trace }` — all Clone anchors,
   controlled handles only (no mutable Conversation refs).
2. `ToolResult { content, status, extra }` — facade result type. **Not** `Serialize` (keeps
   blanket + explicit impls coherent). Constructors text/blocks/error/with_status + accessors +
   `into_response(call_id)`.
3. `IntoToolResult` trait: blanket `impl<T: Serialize>` (Value::String → raw text, else compact JSON)
   + `impl for ToolResult`. Covers String / Value / impl Serialize / explicit ToolResult.
4. `Tool { name, description, input_schema: Value, executor: Arc<dyn ToolExecutorFn> }`, Clone, manual
   Debug. `Tool::function` (feature) + `Tool::function_with_schema` (always). `declaration()` →
   `model::tool::Tool`.
5. internal `ToolExecutorFn` (async_trait) + `FunctionTool<F, Args>`: deserialize args (invalid →
   structured error), call handler (Err → structured error), `IntoToolResult`.
6. `pub(crate) FacadeToolRegistry` impl `agent::ToolRegistry`: typed tools + optional escape-hatch
   custom registry + extra declarations + context parts. Ctor validates name conflicts →
   `FacadeError::DuplicateTool`. `declarations()` merges all; `execute()` dispatches to typed executor
   (builds ToolContext) or delegates to custom, else `UnknownTool`. Tool failures →
   `ToolRuntimeError::ExecutionFailed` so the loop's `ToolFailurePolicy` governs.

## FacadeError
Add `#[non_exhaustive]` variant `DuplicateTool { name }`.

## Validation
- `cargo fmt --all -- --check`
- `cargo test -p agent-lib facade::tool` (and `--features facade-schema`)
- `cargo clippy --all-targets -- -D warnings` (+ `--features facade-schema`)
- `cargo test --all --all-targets` (≤30 min)
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`
- `git diff --check`

## Status: DONE
- Validation series all green: fmt --check ✅ | facade::tool 10 (default) / 11 (facade-schema) ✅ |
  clippy default + facade-schema 0 warnings ✅ | full suite (152 lib) ✅ | doc default + facade-schema
  + doctests ✅ | git diff --check clean ✅.
- R1 decided & documented (facade-schema off-by-default feature + explicit-schema fallback); no new
  prerequisite task needed. TODO.md M2-1 marked [DONE] with completion record; PLAN.md R1 updated.
