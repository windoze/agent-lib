# M7-R — Review: host-embedding surface correctness + doc consistency

TODO.md first incomplete task = **M7-R** (line 2110). M7-1..M7-5 are `[DONE]`.
M7-R is the LAST task → on completion, all tasks done → final review + `endtag`.

## What M7-R requires
1. Verify consistency with docs/facade-api.md §19 + PLAN.md Milestone 7:
   - still assembly layer, NO new effect family, no change to lower-layer semantics.
   - each injection seam has a default preserving M1–M6 behavior; no compat break
     (`#[non_exhaustive]` add-field, un-injected fallback).
2. Access-surface acceptance: confirm each of the "5 gaps" (= M7-1..M7-5 subgoals) is
   solvable **via facade injection**, host never descends to agent layer to self-assemble
   a HandlerScope+drain. Check whether `prelude` needs new public types (WireRunEvent etc.).
3. Summarize leftover gaps as follow-up tasks; confirm NO unscheduled failing tests.

## Validation
- Full sequence 1–6 all green + all-external-features clippy.
- Produce comparison table: 5 gaps vs M7 impl (solved via facade / needs descent + reason).

## Findings / actions
- prelude.rs currently exports NO M7 host-embedding types. facade::mod re-exports
  WireRunEvent/WireRunOutput/ApprovalRequest but prelude omits them.
  → ACTION: add host-embedding types to prelude (WireRunEvent, WireRunOutput,
    ApprovalRequest) + update prelude rustdoc mentioning M7.
- §19 design constraints: M7-5 hooks are #[serde(skip)] (dropped on snapshot) → consistent
  with "Snapshot 不保存 secret、闭包、client". No new effect family. Verify in code.

## Progress
- [ ] Verify each of 5 gaps in code (interaction_handler / WireRunEvent / ApprovalRequest /
      external session handler / dispatcher+permission seams)
- [ ] Decide + apply prelude additions
- [ ] Run validation sequence 1–6 + external clippy
- [ ] Fill completion record (comparison table), mark M7-R [DONE]
- [ ] Final review + git tag endtag (all tasks done)

## Progress (updated — M7-R COMPLETE)
- [x] Verified all 5 gaps solved via facade injection (interaction_handler / to_wire /
      enriched ApprovalRequest / default_external_session_handler / dispatcher+on_permission)
- [x] §19 constraints all hold (assembly layer, no new effect family, snapshot drops hooks,
      non_exhaustive add-field compat)
- [x] prelude fix: added WireRunEvent, WireRunOutput, ApprovalRequest + refreshed rustdoc
- [x] Validation 1–7 green: fmt / clippy default / clippy all-external-features /
      test --all --all-targets (exit 0) / doc -D warnings / doctest (12) / git diff --check
- [x] No leftover gaps, no unscheduled failing tests
- [x] Completion record + comparison table in TODO.md, M7-R [DONE]
- [ ] Commit + git tag endtag (all tasks done → project complete)
