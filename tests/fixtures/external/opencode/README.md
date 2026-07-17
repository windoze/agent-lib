# `opencode` runtime cassettes

Reserved for **recorded** OpenCode runtime cassettes
([`ExternalRuntimeCassette`](../../../../crates/agent-testkit/src/external/cassette.rs)).

No real cassettes live here yet: the concrete OpenCode adapter and its stream
parser land in a later milestone (M8). When they do, redacted recordings of real
CLI output frames plus the expected observed-event stream and decision point go
in this directory, and are replayed offline through
`CassetteRuntimeExternalSessionHandler` for parser-drift regression.

Every committed cassette must pass the redaction scan
(`ExternalRuntimeCassette::assert_no_secrets`): no `API_KEY`, `AUTH_TOKEN`,
`sk-…`, bearer tokens, or private-key material.
