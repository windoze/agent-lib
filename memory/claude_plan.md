# M7-3 实现 Codex session adapter 与 ignored real e2e

**当前执行 = TODO.md 第一个未完成任务 = M7-3**（第一个 `[TODO]` heading = 行 2234 `### [TODO] M7-3`；
M8-1 虽 `[DONE]` 但排在 M7-3/M7-4 之后，故 M7-3 仍是首个未完成任务）。

## 任务范围（TODO.md M7-3，行 2234-2260）
在 feature `external-codex` 下，把 M7-1 config + M7-2 decoder 接进 M5 的
`ExternalRuntimeAdapter`/`ExternalRuntimeSession` 抽象：
- start/resume/advance/cleanup。
- `ExternalPermissionMode` → Codex sandbox/approval 参数（复用 M7-1 config，已映射）。
- 接入 live sink + observations buffering（复用 decoder）。
- tool bridge：Codex `exec --json` 自主运行，流里没有 host-pausable tool/approval 帧（M7-2 结论）。
  故 host_tools/host_subagents/permission_bridge = false，`RespondToolResults`/`RespondSubagent`/
  `RespondInteraction` → `UnsupportedCapability`；declared tools → 拒绝。
- ignored e2e：临时 worktree，验证一个 Codex 完成的小任务。

## 关键实测（本机 codex-cli 0.144.1，已验证）
- 非交互入口 = `codex exec <PROMPT>`（prompt 为位置参数，不是 stdin 帧）。stdin 必须 = null，
  否则 codex 打印 "Reading additional input from stdin..."（到 stderr）。stderr 丢弃。
- fresh 顺序（复用 frozen `base_exec_args()` + prompt，已验证运行）：
  `codex -a <approval> exec --json -s <sandbox> --skip-git-repo-check [--model M][--profile P] <prompt>`
- resume：`codex exec resume [OPTIONS] [SESSION_ID] [PROMPT]`；resume 子命令无 `-s/--sandbox`、
  `-p/--profile`（只有 `--json`/`--skip-git-repo-check`/`-m`）。故 resume 把全局 flag 放顶层：
  `codex -a <approval> -s <sandbox> [--model M][--profile P] exec resume --json --skip-git-repo-check <id> <msg>`
  （已用 bogus id 实测该顺序被 CLI 接受，报 "no rollout found"，即通过 arg parse）。
  → 新增 `CodexConfig::base_resume_args(session_id)`（additive，不改 frozen `base_exec_args`）。
- 真机 `codex exec --json` 实测帧 = `thread.started{thread_id}` / `turn.started` /
  `item.completed{agent_message,text}` / `turn.completed{usage}` —— 与 M7-2 decoder 完全对齐。
- 每 turn 一个独立进程（与 Claude 长驻进程 + stdin 帧不同）：advance 每个新 turn spawn 新进程。

## 设计（mirror M6-3 ClaudeCodeAdapter，但一次性进程）
- `codex/adapter.rs`：
  - `CodexLauncher`(trait) + `CodexTurnStream`(trait)：抽象「spawn 一个 turn 进程 + 逐行读 stdout」，
    生产 `SystemCodexLauncher`(持 config，tokio::process，stdin=null/stderr=null/kill_on_drop/read timeout)，
    单测 `FakeLauncher` 回放固定帧 + 捕获每 turn 的 `CodexTurnSpec`。
  - `CodexTurnSpec::{Fresh{prompt}, Resume{session_id,message}}` → launcher 由 config 构造 args。
  - `CodexSession<L>`(私有，`ExternalRuntimeSession`)：持 launcher + 跨全程单调 seq 的 `CodexStreamDecoder`；
    begin/advance/shutdown（详见代码）。
  - `CodexAdapter`(pub, `ExternalRuntimeAdapter`)：new/with_probed_capabilities/kind/capabilities/start/resume。
  - implemented caps：streaming/resume/artifacts/usage/graceful=true；host_tools/host_subagents/
    permission_bridge=false（exec 无 host-answerable pause，忠实反映 M7-2 能力差异）。
- `codex/mod.rs` + `external/mod.rs` re-export `CodexAdapter`。
- `tests/external_codex.rs`：`#[ignore]` real e2e，mirror `external_claude_code.rs`。

## 验证序列（TODO 1-6）
1. cargo fmt --all -- --check
2. cargo test -p agent-lib --features external-codex --lib codex + codex_cassette
3. cargo clippy --all-targets -- -D warnings（feature on + off）
4. cargo test --all --all-targets（feature off，<=30min）
5. RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace --features external-codex
6. git diff --check
+ 真机 ignored e2e：尝试运行并记录结果。

## 进度
- [x] config.rs base_resume_args + test
- [x] codex/adapter.rs（launcher/turnstream/session/adapter + 单测，16 inline tests）
- [x] codex/mod.rs + external/mod.rs re-export CodexAdapter
- [x] tests/external_codex.rs ignored e2e
- [x] docs（managed §13.3 + capability-matrix Codex + fixtures README）
- [x] 验证 1-6 + 真机 e2e（codex-cli 0.144.1 实跑通过，5 事件、READY.txt、优雅关闭、~51s）
- [x] TODO.md [DONE] + 完成记录
- [x] commit
