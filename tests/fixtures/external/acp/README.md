# `acp` runtime cassettes

Committed Agent Client Protocol (ACP) runtime cassettes
([`ExternalRuntimeCassette`](../../../../crates/agent-testkit/src/external/cassette.rs)).

- `full_session.json` — the **synthetic** M10-2 decoder fixture. It freezes a
  two-turn ACP session (`Start` → completion, `Continue`/resume → completion)
  whose raw JSON-RPC frames are replayed through the connection-private
  [`AcpStreamDecoder`](../../../../src/agent/external/acp/decoder.rs) by
  `tests/agent_acp_cassette.rs` to prove the decoder reproduces the frozen
  observation stream, per-turn decision, and cached client request. It covers a
  `session/new` result establishing the session id, assistant text
  (`agent_message_chunk`), a plan/todo update (`plan`), a non-command
  `tool_call` and its `tool_call_update` completion, a file edit reported as a
  `diff`, a cached `session/request_permission`, and a `session/prompt` result
  ending the turn (`stopReason`). It uses invented, non-secret frames — no real
  ACP agent output. Regenerate it from the in-code builder with
  `AGENT_LIB_UPDATE_EXTERNAL_CASSETTES=1`.

The frame schema mirrors the official `agent-client-protocol` wire format: the
agent streams `session/update` notifications (tagged by `sessionUpdate`) plus
JSON-RPC responses to the client's `session/new` and `session/prompt` requests,
and may issue client-serviced requests (`session/request_permission`, `fs/*`,
`terminal/*`). M10-2 is decoder-only: it normalizes these into neutral
observations and per-turn decisions and *caches* the client requests without
answering them — the live `AcpAdapter` that actually drives the host-pausable
permission bridge and services `fs`/`terminal` lands in M10-3. Redacted
recordings of *real* agent output (and the ignored real e2e suite) land with
that adapter.

No official-crate protocol type is stored on the wire or leaks into the public
API: the fixture holds only raw JSON-RPC frame strings and the neutral
`ExternalObservedEvent` / `AcpDecision` vocabulary.

Every committed cassette must pass the redaction scan
(`ExternalRuntimeCassette::assert_no_secrets`): no `API_KEY`, `AUTH_TOKEN`,
`sk-…`, bearer tokens, or private-key material.
