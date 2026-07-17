# `claude_code` runtime cassettes

Committed Claude Code runtime cassettes
([`ExternalRuntimeCassette`](../../../../crates/agent-testkit/src/external/cassette.rs)).

- `full_session.json` — the **synthetic** M6-2 decoder fixture. It freezes a
  three-turn session (`Start` → host tool-call pause, `RespondToolResults` →
  permission pause, `RespondInteraction` → completion) whose raw `stream-json`
  frames are replayed through the adapter-private
  [`ClaudeStreamDecoder`](../../../../src/agent/external/claude_code/decoder.rs)
  by `tests/agent_claude_code_cassette.rs` to prove the decoder reproduces the
  frozen observation stream and per-turn decision. It uses invented, non-secret
  frames — no real CLI output. Regenerate it from the in-code builder with
  `AGENT_LIB_UPDATE_EXTERNAL_CASSETTES=1`.

Redacted recordings of *real* CLI output land alongside the live session adapter
in a later milestone (M6-3).

Every committed cassette must pass the redaction scan
(`ExternalRuntimeCassette::assert_no_secrets`): no `API_KEY`, `AUTH_TOKEN`,
`sk-…`, bearer tokens, or private-key material.
