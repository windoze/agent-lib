# `codex` runtime cassettes

Committed Codex (`codex exec --json`) runtime cassettes
([`ExternalRuntimeCassette`](../../../../crates/agent-testkit/src/external/cassette.rs)).

- `full_session.json` — the **synthetic** M7-2 decoder fixture. It freezes a
  two-turn session (`Start` → completion, `Continue`/resume → failed turn) whose
  raw `codex exec --json` `ThreadEvent` frames are replayed through the
  adapter-private
  [`CodexStreamDecoder`](../../../../src/agent/external/codex/decoder.rs) by
  `tests/agent_codex_cassette.rs` to prove the decoder reproduces the frozen
  observation stream and per-turn decision. It covers assistant text, a shell
  command, a file patch, an MCP tool call, a policy-declined (permission)
  command, turn completion (with usage), and a failed turn. It uses invented,
  non-secret frames — no real CLI output. Regenerate it from the in-code builder
  with `AGENT_LIB_UPDATE_EXTERNAL_CASSETTES=1`.

The frame schema mirrors the current Codex CLI (`codex exec --json` emits
newline-delimited `ThreadEvent`s: `thread.started`, `turn.started` /
`turn.completed` / `turn.failed`, `item.started` / `item.updated` /
`item.completed` wrapping a typed item, and a top-level `error` notice).
`codex exec --json` runs autonomously, so a turn only ever settles on completion
or failure — there is no host-pausable tool-call or permission frame, and a
policy-declined action surfaces as a `command_execution` with a `declined`
status. Redacted recordings of *real* CLI output land alongside the live session
adapter in a later milestone (M7-3).

Every committed cassette must pass the redaction scan
(`ExternalRuntimeCassette::assert_no_secrets`): no `API_KEY`, `AUTH_TOKEN`,
`sk-…`, bearer tokens, or private-key material.
