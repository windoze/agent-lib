# M8-2 实现 OpenCode stream decoder cassette 测试

**当前任务 = TODO.md 第一个未完成任务 = M8-2**（TODO.md 行 2452 `### [TODO] M8-2`）。
M8-1 已 `[DONE]`（config+probe），M8-2 = 私有 `opencode run --format json` decoder + cassette。

## 真实 OpenCode wire 格式（已核对 sst/opencode 源码，非硬编码假设）
`opencode run --format json` 逐行输出信封：
`{ "type": <emit-type>, "timestamp": <ms>, "sessionID": <id>, ...data }`
（源：packages/opencode/src/cli/cmd/run.ts `emit()`；part schema：packages/sdk/js/src/gen/types.gen.ts）

emit 类型仅 6 种（run.ts loop 只对这些调用 emit）：
- `text` → `{ part: TextPart }`（仅 part.time.end 时）→ TextDelta(part.text)
- `reasoning` → `{ part: ReasoningPart }`（仅 thinking，默认关）→ 容忍
- `tool_use` → `{ part: ToolPart }`（仅 state.status ∈ {completed,error}）
  - ToolPart = { type:"tool", callID, tool:<name>, state }
  - tool id "bash"（input.command, metadata.exit, metadata.output/output, title=command）
  - "edit"/"write"/"apply_patch"（title=filePath 或 input.filePath, metadata.diff）
  - "task"（子代理：input.description/prompt/subagent_type）
  - 其他（grep/read/glob/webfetch...）
  - state error 且是权限拒绝 → opencode 稳定错误串 "The user rejected permission to use..."/
    "prevents you from using this specific tool call" → 信息型 PermissionRequested（run 模式自动裁决，
    不回灌 host，等价 Codex declined 处理）
- `step_start` → `{ part: StepStartPart }` → 容忍（边界）
- `step_finish` → `{ part: StepFinishPart }`：{ reason, cost, tokens:{input,output,reasoning,cache:{read,write}} }
  - reason=="tool-calls" → 继续（无决策）；其余（"stop"/"length"...）→ 终结 Completed
  - usage 跨 turn 内所有 step_finish 累加为 turn 总量
- `error` → `{ error: ApiError }`（session.error，{name, data?:{message}}）→ Failed(Runtime)

SessionStarted：无独立 init 帧，从首个带 sessionID 的帧惰性发一次。
权限：run --format json **不**输出 permission.asked（--auto 自动批准/否则自动拒绝），
故 decoder 自主（Completed/Failed only，同 Codex），无 host-pausable 决策。

## 决策类型 OpenCodeDecision = { Completed{output}, Failed{error} }（同 Codex，无 Paused 臂）

## 交付物
1. src/agent/external/opencode/decoder.rs：OpenCodeDecodeContext / OpenCodeDecision / OpenCodeStreamDecoder
   （push_line/take_observations/session_id；防御式 serde_json::Value 解析；私有 schema 不外泄）
2. opencode/mod.rs：mod decoder + pub use
3. external/mod.rs：feature-gated 追加 decoder 三型 re-export
4. tests/agent_opencode_cassette.rs：#![cfg(feature="external-opencode")] cassette 套件
   （regenerate/matches/secret-free/decode-full/tolerance/malformed/error 各 test）
5. tests/fixtures/external/opencode/full_session.json：AGENT_LIB_UPDATE_EXTERNAL_CASSETTES=1 生成
   覆盖 text/command/patch/permission/tool/subtask/completion/error
6. 更新 fixtures README、docs/managed-external-agent.md §14、docs/capability-matrix.md
7. TODO.md M8-2 → [DONE] + 完成记录

## 验证序列
1. cargo fmt --all -- --check
2. cargo test -p agent-lib --features external-opencode opencode（lib）
3. AGENT_LIB_UPDATE_EXTERNAL_CASSETTES=1 生成 fixture，再 cargo test --features external-opencode --test agent_opencode_cassette
4. cargo clippy --all-targets -- -D warnings（off）+ --features external-opencode
5. cargo test --all --all-targets（off，<=30min）
6. RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --features external-opencode
7. git diff --check

## 进度
- [x] 读 TODO/PLAN/源码/真实 opencode schema
- [x] 写 decoder.rs
- [x] 挂 mod + re-export
- [x] 写 cassette test + 生成 fixture
- [x] 更新 docs（managed-external-agent.md §14、capability-matrix.md、fixtures README）
- [x] 跑验证序列 1-6 全过（fmt / clippy off+on / lib opencode 13 + cassette 7 / full suite / doc / diff --check）
- [x] TODO.md M8-2 DONE + 完成记录 → commit
