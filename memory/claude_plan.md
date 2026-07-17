# Task: M2-4 `Agent::stream` + `snapshot`/`restore` + `into_parts`

First incomplete task in `TODO.md` (M1-*, M2-1..M2-3 all `[DONE]`).

## Requirements (TODO.md §M2-4, docs/facade-api.md §8.2, §15.2)

- `Agent::stream(&mut self, input) -> Result<AgentRunStream, FacadeError>`:
  drive streaming, forward `TextDelta`/`ToolStarted`/`ToolFinished`/
  `ApprovalRequested`, end with `Done(RunOutput)`.
- `snapshot() -> Result<AgentSnapshot, FacadeError>` (M2 scope: supervisor
  `ConversationSnapshot` + `AgentStateSnapshot`; delegates/pending/mailbox/
  blackboard/plan/artifacts left empty/None).
- `restore() -> AgentRestoreBuilder` (re-inject provider/client/tools/approval).
- `into_parts() -> AgentParts` escape hatch.
- Full rustdoc + validation sequence 1-6.

## Key findings

- Reference `LlmClientHandler` in Streaming mode folds the stream internally and
  surfaces nothing incrementally. The only way for the facade to surface deltas
  is a custom tapping `LlmHandler`. Keep machine NonStreaming; the tap handler
  always calls `chat_stream`, folds via `Accumulator`, forwards `TextDelta`.
- `drain` returns notifications only at the end. Real-time tool/approval events
  come from wrapping the Tool/Interaction handlers.
- Approval interaction carries only `call_id`. Add `tool_name` to
  `PendingDecision::Deny` + `FacadeApproval::pending_tool_name(call_id)` peek so
  the interaction tap can emit `ApprovalRequested { tool_name }`.
- `AgentState` is Serialize/Deserialize but NOT Clone/PartialEq; `AgentStateSnapshot`
  = `#[serde(transparent)]` newtype over `serde_json::Value` (holds serialized state).
- Restore approach A: deserialize AgentState authoritatively, re-inject
  client/tools/approval; `ids = FacadeIds::continuing_after(conversation)`.
- `AgentRunStream<'a>` holds `Pin<Box<dyn Future<Output=Result<RunOutput,
  FacadeError>> + 'a>>` (borrows `&mut machine`) + `Arc<Mutex<VecDeque<RunEvent>>>`
  sink. poll: drain sink first, then poll future; on Ready store RunOutput,
  drain remaining sink, then emit `Done`.

## Steps
1. [done] Explore.
2. [done] approval.rs: tool_name on Deny + pending_tool_name.
3. [done] snapshot.rs: AgentSnapshot/AgentStateSnapshot/placeholders/AgentParts/AgentRestoreBuilder.
4. [done] stream.rs: AgentRunStream + tap handlers + scope.
5. [done] agent.rs: submodules + methods stream/snapshot/restore/into_parts (+ extracted build_facade_approval / assemble_machine helpers).
6. [done] mod.rs exports.
7. [done] tests: 8 new offline tests (stream==run_full, tool events, approval event, snapshot round-trip + restore, into_parts, restore guards).
8. [done] validate: fmt clean, clippy -D warnings clean, focused facade::agent 15/15 pass; full suite + rustdoc running.
9. [done] TODO.md [DONE] + commit.

## Status: done (all validation green; committing)