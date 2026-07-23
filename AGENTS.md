# AGENTS.md

Operational guide for working in **agent-lib**. It complements the design docs
under [`docs/`](docs/) and the archived task ledgers (most recent:
[`PLAN.md`](docs/archive/2026-07-20-mag-gaps/PLAN.md),
[`TODO.md`](docs/archive/2026-07-20-mag-gaps/TODO.md)); read those for
architecture and roadmap. This file is the short "how to build, test, and run
things" reference.

## Repository layout

- `src/` — the `agent-lib` crate: `client/` (provider-neutral client
  contracts), `adapter/` (LLM wire adapters — Anthropic Messages / OpenAI Responses /
  OpenAI Chat/Completions; shared HTTP/SSE/request helpers live in `adapter/common/`),
  `conversation/` (Conversation core), `model/`
  (normalized data model + escape hatches), and `agent/` (sans-io machines +
  effect handlers, including `agent/external/` for the managed external-runtime
  stack and `agent/external/process/` for shared CLI child-process plumbing).
- `crates/agent-testkit/` — dev-only test harness (`TestScope`, `SeqIds`,
  scripted/cassette handlers, fixtures, assertions). It is a dev-dependency, so
  it is available to tests, benches, and **examples** but never to the library
  build.
- `examples/` — runnable examples; shared example-only helpers live in
  `examples/support/`.
- `tests/` — integration tests. Real endpoint / real CLI tests are `#[ignore]`
  and skip cleanly when unconfigured.
- `docs/` — design and reference docs. `docs/managed-external-agent.md` and
  `docs/capability-matrix.md` are the sources of truth for the managed external
  agent.

## Build, lint, and test

Run these before finishing a change, in this order (cheap → expensive):

```bash
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test --all --all-targets
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
```

The default build pulls in **no** managed external-runtime machinery: the three
CLI adapters and the ACP adapter are behind off-by-default features. The CLI
features add only the unix-only `libc` crate for process-group signalling; ACP
adds its optional protocol crates. Run their clippy pass separately when you
touch them:

```bash
cargo clippy --all-targets \
  --features "external-claude-code external-codex external-opencode external-acp" -- -D warnings
```

Every test must finish in well under a minute; a test that hangs is a bug to fix
immediately, not to wait out.

## Feature flags

| Feature | Enables |
|---|---|
| `external-claude-code` | Managed Claude Code adapter (config + probe + decoder + live session) |
| `external-codex` | Managed Codex adapter |
| `external-opencode` | Managed OpenCode adapter |
| `external-acp` | Managed ACP adapter (Agent Client Protocol runtime + schema crates) |
| `facade-schema` | JSON-schema derivation for typed facade tools |

All are **off by default**. The three CLI features are intentionally light;
`external-acp` and `facade-schema` pull their named optional crates only when
requested.

## Managed external agents

The managed path drives a real coding-agent CLI through an
`ExternalAgentMachine` (sans-io) and a scoped, registry-backed
`ExternalSessionHandler` — never by calling the adapter directly. See
`docs/managed-external-agent.md` for the design and `docs/capability-matrix.md`
for the capability model.

### Examples

Each example is gated by `required-features`, so the default
`cargo check --examples` skips them:

```bash
cargo run --example managed_claude_code --features external-claude-code
cargo run --example managed_codex        --features external-codex
cargo run --example managed_opencode     --features external-opencode
cargo run --example managed_mixed        --features "external-claude-code external-codex"
```

Shared wiring: [`examples/support/managed.rs`](examples/support/managed.rs).

### Required environment

The managed examples need **no** secrets: each spawned CLI uses its own stored
login, inherited from the process environment. Only optional overrides are read
(and never printed):

| Variable | Effect | Default |
|---|---|---|
| `CLAUDE_CODE_BIN` / `CODEX_BIN` / `OPENCODE_BIN` | Path to the CLI binary | `claude` / `codex` / `opencode` on `PATH` |
| `CLAUDE_CODE_MODEL` / `CODEX_MODEL` / `OPENCODE_MODEL` | Pin a cheaper model | adapter default |

The mixed multi-agent e2e additionally needs a `DEEPSEEK_API_KEY` (optional
`DEEPSEEK_BASE_URL` / `DEEPSEEK_MODEL`) for its coordinator LLM; it is read from
the environment or a `.envrc` and never logged.

The OpenAI Chat/Completions real-endpoint regression
([`tests/integration_openai_chat.rs`](tests/integration_openai_chat.rs), `#[ignore]`,
skips cleanly when unset) reads its own endpoint vars, also never printed:

| Variable | Effect | Default |
|---|---|---|
| `OPENAI_CHAT_BASE_URL` / `OPENAI_CHAT_API_KEY` | facade adapter endpoint + Bearer token (no-auth `AuthScheme::None` when key absent) | none (skips) |
| `DEEPSEEK_API_KEY` | DeepSeek dialect (`DEEPSEEK_BASE_URL` / `DEEPSEEK_MODEL` optional) | none (skips) |
| `VLLM_BASE_URL` | vLLM dialect (`VLLM_API_KEY` optional → no-auth; `VLLM_MODEL` optional) | none (skips) |

### Ignored real e2e commands

Structured real-CLI regressions are `#[ignore]` and skip when the binary/login
is absent:

```bash
cargo test --features external-claude-code --test external_claude_code -- --ignored --nocapture
cargo test --features external-codex        --test external_codex        -- --ignored --nocapture
cargo test --features external-opencode     --test external_opencode     -- --ignored --nocapture

# Multi-agent managed path: DeepSeek coordinator fans out to Claude Code + Codex children.
cargo test --features "external-claude-code external-codex" \
  --test agent_external_managed_real_e2e -- --ignored --nocapture
```

### Safety properties

- **Worktree isolation** — `ExternalSessionPolicy.isolation` is applied inside
  the library by `ExternalSessionRegistry` through its `WorktreeManager`
  (default `GitWorktreeManager`, M2-7): the prepared path becomes the session's
  working directory, and cleanup after a drive removes an ephemeral worktree on
  a clean close while retaining a dirty one for inspection. Facade-managed
  drives declare `EphemeralGitWorktree`, so their children run in a per-session
  throwaway linked worktree under the OS temp dir; the examples build their own
  `git init` worktree and declare `Shared` (the host owns that directory). Either
  way, a child that writes files never touches the checkout it launched from
  (`docs/managed-external-agent.md` §16).
- **Process-group kill** — on unix every managed child leads its own process
  group, and a force-close signals the whole group (SIGTERM, then SIGKILL), so
  grandchildren a CLI spawned (builds, dev servers) cannot outlive the session
  (`docs/managed-external-agent.md` §16). Windows has no process-group
  semantics and kills only the direct child.
- **Secret redaction** — credentials are never read into logs or printed; cassette
  fixtures are scrubbed and asserted secret-free.
- **Prompt argv exposure** — Codex/OpenCode prompts are passed as CLI positional
  arguments guarded by a `--` separator (Claude Code uses stdin frames); argv is
  visible to same-host `ps`, so prompts must not carry secrets
  (`docs/managed-external-agent.md` §16).
- **CLI environment inheritance** — Claude Code, Codex, and OpenCode children
  inherit the host process environment, then apply adapter-specific overrides;
  this preserves each CLI's login, PATH, HOME, and tool configuration but also
  passes unrelated host secrets to the child process. Run `agent-lib` under a
  pruned environment/container when that is not acceptable, or use the ACP
  adapter's explicit `inherit_env`/`env_clear` controls.
- **Unsupported-capability fallback** — a missing CLI or a failed capability
  probe becomes a non-secret **skip** (exit 0), and a request for a capability
  the runtime has not opted into (for example host tools) is rejected with an
  explicit `UnsupportedCapability{..}` error rather than silently degraded.

## Conventions

- Keep `ExternalAgentMachine` (and the other machines) sans-io: all IO lives in
  handlers/adapters.
- Prefer small, targeted patches; re-read a section between edits.
- Recover poisoned standard-library locks in library code with
  `unwrap_or_else(|poison| poison.into_inner())` unless the protected data has a
  documented invariant that a panic can corrupt. Keep true invariant panics rare
  and give them specific context, or prefer `debug_assert!` plus a defensive
  error branch.
- Update the doc that owns a behavior when you change it; the managed docs above
  are kept in sync with the code as of milestone M9-4.
