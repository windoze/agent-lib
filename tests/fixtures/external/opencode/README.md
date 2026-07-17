# `opencode` runtime cassettes

Reserved for **recorded** OpenCode runtime cassettes
([`ExternalRuntimeCassette`](../../../../crates/agent-testkit/src/external/cassette.rs)).

The feature-gated OpenCode launch config and capability probe already landed in
M8-1 (`external-opencode`:
[`OpenCodeConfig`](../../../../src/agent/external/opencode/config.rs),
[`opencode_probe`](../../../../src/agent/external/opencode/probe.rs)); those are
exercised by in-module offline unit tests and need no cassette.

No stream cassettes live here yet: the concrete OpenCode stream decoder lands in
M8-2 and the live adapter in M8-3. When they do, redacted recordings of real
`opencode run --format json` output frames plus the expected observed-event
stream and decision point go in this directory, and are replayed offline through
`CassetteRuntimeExternalSessionHandler` for parser-drift regression.

Every committed cassette must pass the redaction scan
(`ExternalRuntimeCassette::assert_no_secrets`): no `API_KEY`, `AUTH_TOKEN`,
`sk-…`, bearer tokens, or private-key material.
