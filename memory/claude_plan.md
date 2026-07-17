# M4-3 external approval defaults + restore policy + AgentSnapshot external fields

Task: TODO.md M4-3 (first incomplete). Wire external delegate approval defaults,
add `RestoreExternal` + `Agent::restore().restore_external(..)` (default
`MarkInterrupted`), and extend `AgentSnapshot` with data-only external delegate
fields. Offline only (scripted handlers). No real CLI.

## Design decisions
- **Approval**: external delegate start is gated in the delegation DRIVE
  (`drive_external_delegation`) via `FacadeApproval::resolve_external_start`,
  honoring override > per-tool > (ask_external_agents ? ask-deferred : default).
  Model-routed external `ask_<name>` tools are EXEMPTED from the machine
  approval gate so the drive is the single authority (no double-prompt). On
  denial: record a Failed delegation with `approval_denied=true`, fold a denied
  tool result; `run_full`/`stream` surface `FacadeError::ApprovalDenied`.
- **Retention**: `Agent.last_external_sessions: HashMap<name, RetainedExternalSession>`
  updated after `run_full` drain from the recorder (status/session ref/artifacts).
  Stream path does not retain (future holds &mut machine); documented boundary.
- **Snapshot**: `AgentSnapshot.external_delegates: Vec<ExternalDelegateSnapshot>`
  (data-only: name/runtime/mode/worktree/model/args/permission_mode + last-known
  status/session ref/artifacts). No handle/secret/api key/closure. No raw brief (R5).
- **Restore**: `AgentRestoreBuilder.external_agent(name, mea)` + `.restore_external(policy)`.
  Default `MarkInterrupted` -> restored delegate status=Interrupted. `AttachOrFail`
  requires re-registered handler+resumable session else `FacadeError`. `RestartFromBrief`
  clears session (status=Pending).

## Deliverables / steps
1. external.rs: RestoreExternal, ExternalDelegateStatus, RetainedExternalSession,
   extend ExternalDriveOutcome+RecordingExternalMachine with session ref.
2. approval.rs: external_tools set + exempt in approval_requirement +
   resolve_external_start; update ask_external_agents/ask_worktree_write rustdoc.
3. delegate.rs: RecordedDelegation{approval_denied, session, runtime, status};
   DelegationToolHandler gains approval; gate in drive_external_delegation.
4. agent.rs: Agent.last_external_sessions; build_facade_approval(external names);
   thread approval into DelegationToolHandler (run_full+stream); CollectedTraces
   external_approval_denied + session retention; run_full ApprovalDenied + retain;
   Delegation external tool-name helper.
5. snapshot.rs: AgentSnapshot.external_delegates + ExternalDelegateSnapshot;
   capture; AgentRestoreBuilder external_agent/restore_external + apply policy.
6. mod.rs + prelude: export RestoreExternal, ExternalDelegateSnapshot, ExternalDelegateStatus.
7. Tests (offline) in external.rs/delegate.rs/agent tests + doctest.
8. Docs: facade-api.md notes; M2-2 approval rustdoc "enforced".
9. Validation seq 1-6 (+ 4 ext features clippy). Mark [DONE] + commit.

## Status
- [x] step 1 external.rs (RestoreExternal, ExternalDelegateStatus, RetainedExternalSession, ExternalDriveOutcome.session + capture, from_restored_parts)
- [x] step 2 approval.rs (external_tools exempt + resolve_external_start + rustdoc)
- [x] step 3 delegate.rs (RecordedDelegation.approval_denied/session; DelegationToolHandler.approval + gate; external_tool_names helper)
- [x] step 4 agent.rs (last_external_sessions; build_facade_approval(external names); thread approval run_full+stream; CollectedTraces flag+sessions; run_full ApprovalDenied + retain)
- [x] step 5 snapshot.rs (AgentSnapshot.external_delegates + ExternalDelegateSnapshot + capture; restore external_agent/restore_external + policy + reconstruct recipes)
- [x] step 6 mod.rs exports (RestoreExternal, ExternalDelegateSnapshot, ExternalDelegateStatus)
- [x] step 7 tests (6 offline external approval/restore/snapshot tests pass) + RestoreExternal doctest
- [x] step 8 docs (facade-api.md §9.2/§15.2–§15.3 already describe behavior; only rustdoc needed — no spec edit)
- [x] step 9 validation (fmt ✓, clippy default ✓ + ext features clippy ✓, full test suite ✓, rustdoc -D warnings ✓, git diff --check ✓) + TODO.md M4-3 [DONE] + completion record + commit
