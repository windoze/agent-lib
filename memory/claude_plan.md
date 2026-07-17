# M7-3 — Enrich `ApprovalRequest`

TODO.md first incomplete task = **M7-3** (line 1901). M7-1/M7-2 are `[DONE]`.

## Goal
Give facade `ApprovalRequest` enough to render a meaningful approval box:
`call_id`, `reason: Option<String>`, and a redaction-safe tool-input summary.
Populate them in `TapInteractionHandler` from the underlying
`InteractionKind::Approval { call_id, requirement }` + the pending tool call.

## Design decisions
- Add fields to `ApprovalRequest` (`#[non_exhaustive]`, additive):
  - `pub call_id: String`
  - `pub reason: Option<String>`
  - `pub input: Option<String>` — compact, **redacted, size-bounded** summary.
- Use `Option<String>` (spec's "精简摘要" option), NOT `serde_json::Value`, to
  avoid dropping `Eq` from 5 public types (`ApprovalRequest`/`RunEvent`/
  `RunOutput`/`WireRunEvent`/`WireRunOutput`) — Value isn't `Eq`. String summary
  is also inherently redaction-friendly.
- `FacadeApproval::record_pending` now builds the full `ApprovalRequest`
  (tool_name + call_id + reason + redacted input summary) from `&ToolCall`, and
  stores it in `PendingDecision` for both `Deny` and `Ask`. The `ask` handler
  (`AskFn`) thus also receives the richer request — a bonus, single source.
- New peek `FacadeApproval::pending_request(call_id) -> Option<ApprovalRequest>`;
  keep `pending_tool_name` (public, delegates to stored request).
- `TapInteractionHandler` emits `pending_request(call_id)`, overriding `call_id`
  from the interaction and `reason` from `requirement.reason()` (authoritative).
- Redaction helper `summarize_tool_input(&Value) -> Option<String>`: drops
  null/empty, redacts sensitive keys (token/secret/password/api_key/auth/…),
  compact JSON, truncated at char boundary to a max length.
- `ApprovalRequest::for_tool(name)` constructor for the synchronous
  external-start sites (`decide_tier`/`decide_ask_deferred`) that lack a call.

## Edits
1. src/facade/run.rs: add fields + `for_tool` + rustdoc (redaction note).
2. src/facade/approval.rs: redaction helper; `PendingDecision` holds request;
   `record_pending(call_id, &ToolCall, reason)`; `pending_request`;
   update `for_tool` construction sites; keep resolution/message logic.
3. src/facade/agent/stream.rs: enrich the emitted `ApprovalRequest`.
4. src/facade/run/tests.rs: update `ApprovalRequest {..}` construction (2 sites).
5. src/facade/agent/tests.rs: extend approval test to assert enriched fields
   (call_id non-empty, reason names tool, input summary has arg).
6. src/facade/approval.rs tests: assert redaction of sensitive keys.

## Validation (1-6; external adapter untouched)
1 cargo fmt --all
2 cargo test -p agent-lib facade::agent  (+ facade::approval facade::run)
3 cargo clippy --all-targets -- -D warnings
4 cargo test --all --all-targets (<=30min)
5 RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
6 git diff --check

## Status: DONE
- All edits landed; validation 1-6 green (161 focused + full suite no failures + doc). TODO.md M7-3 marked [DONE].
