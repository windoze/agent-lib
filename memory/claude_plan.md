# M6-2 实现 Claude Code stream decoder cassette 测试

**当前执行 = TODO.md 第一个未完成任务 = M6-2**（M1..M5 全 `[DONE]`，M6-1 `[DONE]`）。

## 任务理解
M6-1 已落地 `external-claude-code` feature 下的 `ClaudeCodeConfig` + capability probe。
M6-2 要实现 **Claude Code 私有 stream-json 帧解码器**：消费原始 CLI 帧，产出中立的
`ExternalObservedEvent` 观测流 + 决策点（`Completed` / `PausedForToolCalls` /
`PausedForInteraction` / `Failed`）。raw schema 不得暴露为 public DTO。并加 cassette
fixture + parser 测试（覆盖 text / permission / tool / patch / completion）。

## 真实 Claude stream-json 帧模型（JSONL，逐行一个 JSON 对象）
- `{"type":"system","subtype":"init","session_id","cwd","tools","model"}` → SessionStarted
- `{"type":"assistant","message":{"id","role","content":[blocks],"usage"}}`
  - text block → TextDelta
  - tool_use Bash → CommandStarted；Edit/Write/... → FilePatch；其它内建 → ToolStarted
  - tool_use `mcp__*`（宿主桥接工具）→ 累积成 PausedForToolCalls 批次
- `{"type":"user","message":{"content":[tool_result...]}}`
  - 关联到内建 Bash → CommandFinished；其它内建 → ToolFinished
- `{"type":"control_request","request_id","request":{"subtype":"can_use_tool","tool_name","input"}}`
  → PermissionRequested 观测 + PausedForInteraction 决策
- `{"type":"result","subtype":"success","result","usage","total_cost_usd",...}`
  → SessionCompleted 观测 + Completed 决策；error 子类型 → Failed(LimitExceeded/Runtime)

## 未知/异常帧策略（稳定）
- 非法 JSON / 非对象 / 缺 `type` 字符串 / 已知类型但内层结构缺失 → `ExternalAgentError::Protocol`
- 已知类型但**未知子结构块**（未知 content block）/ **未知 `type`** / 空行 → 容忍（忽略）

## 设计
- 新增 feature-gated 私有模块 `src/agent/external/claude_code/decoder.rs`：
  - `ClaudeDecodeContext { step_id, actor }`（宿主提供，用于构造 permission Interaction）
  - `ClaudeStreamDecoder`：有状态，跨 turn 单调 `seq`，`push_line -> Result<Option<ClaudeDecision>,_>`，
    `take_observations()`；私有 raw 解析走 `serde_json::Value` 防御式导航（容忍未知）。
  - `ClaudeDecision`（crate 私有）：Completed / PausedForInteraction / PausedForToolCalls / Failed。
  - raw 帧结构不暴露；`mod.rs` 以 `pub(crate) use` 挂载 decoder，不进 public API。
- cassette fixture：`tests/fixtures/external/claude_code/full_session.json`（3 turn：
  Start→PausedForToolCalls，RespondToolResults→PausedForInteraction，RespondInteraction→Completed），
  覆盖 text/command/patch/tool/permission/completion。使用 `ExternalRuntimeCassette` 格式。
- 测试放 decoder.rs 内联 `#[cfg(test)] mod tests`（名字含 `claude_code_cassette`），
  用 dev-dep `agent_testkit::prelude::ExternalRuntimeCassette` 加载 fixture，逐 turn 喂
  `input_frames` 给解码器，断言观测 == `expected_events` 且决策匹配 `decision`；
  加 in-code builder + `AGENT_LIB_UPDATE_EXTERNAL_CASSETTES=1` regenerate 守卫 + round-trip +
  `assert_no_secrets`。再加内联 raw-frame 单测覆盖未知帧容忍 / 空行 / 非法JSON→Protocol /
  缺 type→Protocol / error result→Failed。

## 验证条件（TODO.md）
- `cargo test -p agent-lib claude_code_cassette`（未启 feature → 0 test）
- 真正验证：`cargo test -p agent-lib --features external-claude-code claude_code_cassette`
- 完整序列：fmt → clippy(`--all-targets -D warnings`，含 feature) → `cargo test --all --all-targets`
  （未启 feature）→ feature-enabled 测试 → doc。
- parser 不需真实 Claude Code；fixture 无 secret。

## 执行计划
1. [x] 写 memory plan。
2. [x] 实现 decoder.rs + mod.rs 挂载（feature-gated `pub`,非 `pub(crate)`,见下方修订）。
3. [x] 建 fixture full_session.json（in-code builder + `AGENT_LIB_UPDATE_EXTERNAL_CASSETTES=1` 生成）。
4. [x] 测试：改放 feature-gated 集成测试 `tests/agent_claude_code_cassette.rs`（cassette 回归 + raw-frame 边界）。
5. [x] 更新 docs（managed-external-agent.md §12.2 实现状态 / capability-matrix / fixture README）。
6. [x] 验证序列全过。
7. [x] TODO.md 标 [DONE] + 完成记录。
8. [x] 提交 `[M6-2] ...` 并停止。

## 设计修订（落地时）
- decoder 改为 **feature-gated `pub`** 并 re-export（非 `pub(crate)`）：`agent-testkit` 依赖 `agent-lib`
  且 `agent-lib` dev-dep `agent-testkit`,内联 `#[cfg(test)]` 用 `agent_testkit` 会产生两个 agent-lib
  实例导致类型不一致。故测试改放 feature-gated 集成测试 `tests/agent_claude_code_cassette.rs`
  （agent-lib 只链一次;未启 feature 时该文件为空 → 0 test,符合 `cargo test -p agent-lib claude_code_cassette` 约束）。
- 同步小-`Ok` decoder helper 触发 `clippy::result_large_err`（`ExternalAgentError` ≥136B,未装箱,
  是模块 canonical 错误）：以 decoder 模块级 `#![allow(clippy::result_large_err)]` + 说明注释显式接受,
  与 `adapter.rs`/`registry.rs`/`probe.rs` 保持一致,不装箱、不改共享错误类型。

## 状态：已完成（M6-2 [DONE]，已提交）
