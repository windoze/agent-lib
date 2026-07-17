# M5-1 rules-routed delegation — DONE

## Task
First incomplete task in TODO.md: **M5-1 rules-routed delegation** (docs/facade-api.md §13.2).
Extend `Delegation` with `rules()` + `when_task_contains(keywords, delegate)`; the facade
routes a whole task to a matching delegate (local or external) WITHOUT exposing any delegate
as a model tool. rustdoc + offline tests + full validation sequence.

## Design decisions
- New internal `DelegationMode::Rules { rules: Vec<RoutingRule> }`.
- Matching: case-insensitive substring `contains`; ANY keyword hits; FIRST rule (registration
  order) wins = priority. No match → falls through to normal supervisor drive.
- Rules-routed bypasses the supervisor LLM entirely: supervisor usage = 0, and the routed turn
  is NOT folded into the supervisor `Conversation` (keeps DefaultAgentMachine sans-io
  encapsulation intact). The delegation is reported fully via RunOutput + trace + events.
- Reuse: `fulfill_rules_routed` synthesizes an `ask_<name>(task)` ToolCall keyed by a fresh
  framework call id, then dispatches to the existing `drive_delegation` /
  `drive_external_delegation`, so recorder/usage/artifacts/§9.2 approval gate are identical.
- External approval-denial → `Err(FacadeError::ApprovalDenied)`; a failed (non-denied)
  delegation → Ok(RunOutput) with a Failed trace + DelegationFailed event.
- Build-time validation: a rule naming an unregistered delegate → `FacadeError::Config`.

## Edits
- ids.rs: `fresh_tool_call_id()` (renamed to avoid clash with the `ToolExecutionIds::tool_call_id`
  trait method).
- delegate.rs: RoutingRule, DelegationMode::Rules, builders (rules/when_task_contains/route_task/
  is_rules_routed/first_unknown_rule_delegate), empty declarations/route/external_tool_names for
  Rules, RulesRoutedTarget, fulfill_rules_routed, synthetic_delegation_call. +4 unit tests, +6
  drive/stream tests.
- agent.rs: run_full rules branch, build_delegation_handler, resolve_rules_target,
  run_rules_routed, free fns drive_rules_routed / build_rules_routed_output / rules_routed_summary
  / user_message_text; build-time validation in AgentBuilder::build.
- agent/stream.rs: start() rules branch → start_rules_routed (drives delegate, replays events into
  sink, yields Done).

## Validation — ALL GREEN
1. cargo fmt --all ✓
2. cargo clippy --all-targets -- -D warnings ✓ (and with the three external features ✓)
3. cargo test -p agent-lib facade::delegate ✓ (35 passed)
4. cargo test --all --all-targets ✓ (50 test-result groups, no failures)
5. RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace ✓
6. git diff --check ✓

## Status: COMPLETE — TODO.md M5-1 marked [DONE]. Committing, then STOP.
