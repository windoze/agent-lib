# `opencode` runtime cassettes

Recorded **offline** OpenCode runtime cassettes for `opencode run --format json`
stream-decoder regression.

The feature-gated OpenCode launch config and capability probe landed in M8-1
(`external-opencode`:
[`OpenCodeConfig`](../../../../src/agent/external/opencode/config.rs),
[`opencode_probe`](../../../../src/agent/external/opencode/probe.rs)); those are
exercised by in-module offline unit tests and need no cassette.

`full_session.json` is the M8-2 stream cassette: a redacted, hand-tuned recording
of `opencode run --format json` output frames (two turns — a completing turn and a
failing turn) that pins the [`OpenCodeStreamDecoder`](../../../../src/agent/external/opencode/decoder.rs)
against parser drift. It is regenerated only via
`AGENT_LIB_UPDATE_EXTERNAL_CASSETTES=1 cargo test --features external-opencode
--test agent_opencode_cassette opencode_cassette_regenerate_fixture`, and replayed
offline (no real `opencode` binary) by
[`tests/agent_opencode_cassette.rs`](../../../agent_opencode_cassette.rs). The live
session adapter and on-device e2e land in M8-3.

Every committed cassette must pass the redaction scan
(`ExternalRuntimeCassette::assert_no_secrets`): no `API_KEY`, `AUTH_TOKEN`,
`sk-…`, bearer tokens, or private-key material.
