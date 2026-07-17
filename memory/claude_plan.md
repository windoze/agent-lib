# M7-2 — `RunEvent` serializable projection (`WireRunEvent`)

TODO.md first incomplete task = **M7-2** (line 1839). M7-1 is `[DONE]`.

## Goal
Provide an official, serializable projection of `RunEvent` so cross-process
hosts don't each re-map `RunEvent` -> their own wire enum. Keep R7 intact:
`RunEvent` itself stays non-serde; the projection is explicit, one-way, lossy
(Raw variants degrade to opaque markers).

## Design
- New `WireRunEvent` (`Serialize + Deserialize`), adjacently tagged
  (`tag = "type", content = "data", rename_all = "snake_case"`, matching
  `Notification`).
- Normalized variants forward their (already-serde) payloads verbatim.
- `Done(Box<RunOutput>)` -> `Done(WireRunOutput)` because `RunOutput` holds
  `events: Vec<RunEvent>` (non-serde). New `WireRunOutput` mirrors `RunOutput`
  but projects `events` to `Vec<WireRunEvent>`; `response: Option<Response>` is
  already serde, kept verbatim.
- `RawStream`/`RawNotification` -> `WireRunEvent::Raw(RawEventKind)` opaque
  marker (`Stream`/`Notification`), no payload.
- `RunEvent::to_wire`, `RunOutput::to_wire`.
- rustdoc: lossless normalized variants vs opaque Raw.

## Edits
1. src/facade/run.rs: RawEventKind, WireRunEvent, WireRunOutput; to_wire; doc.
2. src/facade/mod.rs: re-export + doc.
3. src/facade/run/tests.rs: round-trip tests; Raw markers; Done nested.

## Validation (1-6; no external adapter touched)
1 cargo fmt --all -- --check
2 cargo test -p agent-lib facade::run
3 cargo clippy --all-targets -- -D warnings
4 cargo test --all --all-targets (<=30min)
5 RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
6 git diff --check

## Status: DONE
- All edits landed; validation 1-6 green (9 focused, full suite no failures, doc + doctests green). TODO.md M7-2 marked [DONE].
