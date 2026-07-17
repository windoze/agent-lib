# M6-R — Review: Collaboration convenience + overall facade acceptance

Task: TODO.md → first incomplete = **M6-R** (capstone review for facade M1–M6).
Review task (`*R`): do NOT decompose unless the entry itself is structurally wrong.

## What to verify
1. §14 consistency: topology auto-enable correct; external collab bridged to lib
   primitives (no runtime-private protocol leak into public facade API, §14/§19);
   only promise landed capabilities (R8 honesty — no silent "pretend support").
2. Overall acceptance vs §2/§18/§19:
   - progressive usage (Chat → ChatSession → Agent → subagent → external)
   - strong invariants preserved (internal Conversation + DefaultAgentMachine +
     Requirement/HandlerScope/drain/Pop; no bypass state machine)
   - default-usable, recoverable (snapshot has NO secret), observable (RunOutput
     full dimensions), escape hatches clear
   - prelude vs §3 list consistency
   - README / docs: is a facade getting-started example needed?
3. Summarize milestone leftover gaps → follow-up tasks in TODO.md (if any).
   Confirm NO unscheduled failing tests (Test Failure Policy).

## Acceptance deliverable
- Comparison table: facade implemented vs docs/facade-api.md §2–§17 promises;
  uncovered items explicitly recorded as follow-up tasks or confirmed non-goals.

## Validation (full sequence 1–6 + all-external clippy)
1 cargo fmt --all -- --check
2 focused: cargo test -p agent-lib facade::collab
3 cargo clippy --all-targets -- -D warnings
   + clippy --features "external-claude-code external-codex external-opencode external-acp"
4 cargo test --all --all-targets (<=30min)
5 RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
6 git diff --check

## Status: IN PROGRESS
- Reading docs §2/§3/§14/§18/§19 + facade sources; running validation.

---

## M6-R Status: DONE
- §14 collab convenience verified faithful (derive_default table + CollabBridge
  provider-neutral, no runtime-private types in public API, R8 honesty).
- §2–§17 acceptance comparison table recorded in TODO.md M6-R completion record;
  all facade promises (M1–M6) landed, no uncovered §2–§17 item needs a follow-up.
- prelude vs §3: only diff is `AgentSession` (resolved open-question §20 #2 →
  unified stateful `Agent`); not a gap.
- README gap FIXED (doc-only): added Facade layer to intro list + module table,
  new "快速开始：Facade 层" getting-started (Chat + tool Agent, from verified
  doctests), and docs/facade-api.md reference link.
- Validation ALL GREEN: fmt-check; facade::collab 17; clippy default + all-external;
  default suite 1071/0 (10 ignored e2e); rustdoc -D warnings; git diff --check;
  all-external suite 1094/0 (18 ignored e2e). No unscheduled failing tests.
- endtag NOT applied: Milestone 7 still has open tasks.
