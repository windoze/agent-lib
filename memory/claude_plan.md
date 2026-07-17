# M6-3 — Fix external cassette key-order drift under serde_json/preserve_order

Task: TODO.md → first incomplete = **M6-3** (before M6-R).

## Root cause (reproduced)
- All-features build (`external-acp`) unifies `serde_json/preserve_order` → `Value` objects
  become insertion-order `IndexMap` instead of sorted `BTreeMap`.
- External cassette test helpers `fn frame(v) { CassetteFrame::stdout(v.to_string()) }` freeze
  the payload STRING into the committed fixture. Under preserve_order the string reorders keys
  (insertion order) and drifts from the frozen (sorted) fixture.
- Confirmed via `claude_code_cassette_matches_in_code_builder`: fixture sorted vs builder insertion.
- Default + single-feature builds: `Value` is BTreeMap → always sorted → green.

## Fix (deterministic, class-wide)
1. agent-testkit `external/cassette.rs`:
   - Add recursive `sort_json_keys(&Value) -> Value` (object keys sorted; arrays/scalars recursed).
   - Add `CassetteFrame::stdout_json(&Value)` / `stderr_json(&Value)` constructors that serialize
     the canonicalized value. Payload string is then identical under BTreeMap or IndexMap.
   - Unit test: build a Value with keys in reverse order via serde_json::Map, assert stdout_json
     payload is fully sorted (meaningful under the preserve_order/all-features build).
2. Update the 4 fixture-building test helpers to `CassetteFrame::stdout_json(&value)`:
   - tests/agent_claude_code_cassette.rs
   - tests/agent_codex_cassette.rs
   - tests/agent_opencode_cassette.rs
   - tests/agent_acp_cassette.rs (its `update_frame` wraps `frame`, so covered)
   `agent_external_cassette.rs` already uses byte-literals (deterministic) — no change.
3. Do NOT regenerate fixtures in insertion order (would reverse-break default builds). Canonical
   output equals the existing sorted fixtures → no fixture rewrite needed.

## Validation (1–6 + external clippy)
1 `cargo fmt --all`
2 `cargo clippy --all-targets -- -D warnings`
3 `cargo clippy --all-targets --features "external-claude-code external-codex external-opencode external-acp" -- -D warnings`
4 `cargo test --features "external-claude-code external-codex external-opencode external-acp"` (all green)
5 `cargo test --features external-claude-code --test agent_claude_code_cassette` (green)
6 `cargo test --all --all-targets` (green, <30min); `RUSTDOCFLAGS=-D warnings cargo doc --no-deps --workspace`

## Status: DONE
- Harness-level canonicalization (stdout_json/stderr_json + sort_json_keys) landed; 4 frame() helpers switched; ACP fixture re-frozen to canonical sorted order.
- All validation 1-6 + external clippy green (all-features 1094/0, default 1071/0).
- M6-3 marked [DONE]; PLAN.md unchanged (no phase-level change).
