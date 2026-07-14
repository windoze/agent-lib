# 当前任务：M6-4 新增 recorded replay suites

## 定位
- `TODO.md` 第一个未完成任务 = **M6-4**（line 1274，标题 `[TODO]`）。M6-1/M6-2/M6-3 已 `[DONE]`（HEAD=969b147）。
- 前置依赖 M6-3 已完成。无阻塞。
- 未追踪文件 `docs/external-agent.md` 与本任务无关（TODO/PLAN 未引用），不纳入本次提交。

## 目标（TODO.md M6-4 + docs/TESTABILITY.md §8.3）
新增 §8.3 命名 recorded replay 套件，默认离线可跑：
- `agent_replay_text`：recorded user -> LLM text -> commit（CassetteLlmHandler）。
- `agent_replay_tool`：recorded LLM tool_use + tool 结果 + final text（cassette llm/tool）。
- `agent_replay_approval`：tool approval request + approve + final response（cassette interaction）。
每个套件断言 final conversation、handler call log、final cursor。
文档说明如何 record/update cassette 及启用环境变量。

## 关键设计
- 复用 M3-4 建立的 cassette 基础设施。保留 M3-4 `crates/agent-testkit/tests/cassette_replay.rs`（不重命名/删除）。
- 为避免与 M3-4 weather happy-path 重复，`agent_replay_tool` 用 **tool error** 往返（tool_use + Error 工具结果 + 恢复文本）。
- 三套件放 `crates/agent-testkit/tests/`，cassette JSON 放 `crates/agent-testkit/tests/cassettes/`。
- 每套件两个测试：`regenerate_*`（CassetteRecorder::update，默认 skip，仅 AGENT_TESTKIT_UPDATE_CASSETTES=1 写盘）
  + `offline_replay_*`（读 committed cassette + CassettePlayer，断言 conversation/log/cursor）。
- cassette 由 recorder to_json_string_pretty 写出（无行尾空白/无末尾换行），满足 git diff --check。
- 环境变量：AGENT_TESTKIT_RECORD_CASSETTES / AGENT_TESTKIT_UPDATE_CASSETTES。

## 三套件
1. agent_replay_text.rs + agent_text_turn.json：agent_spec(无 tools)，llm=[text]。1 entry。
2. agent_replay_tool.rs + agent_tool_error_roundtrip.json：weather_tool，llm=[tool_use,text]，tool=[error]。3 entries。
3. agent_replay_approval.rs + agent_tool_approval_roundtrip.json：RequireApprovalPolicy，llm=[tool_use,text]，tool=[ok]，interaction=approve。

## 校验顺序
fmt → clippy(-D warnings) → 生成 cassette(UPDATE=1 regenerate ×3) → 离线回放 ×3 → 全套 cargo test --all(≤30min)
→ RUSTDOCFLAGS=-D warnings cargo doc → git diff --check → docs/TESTABILITY.md §8.3 record 文档 → TODO.md [DONE] → commit [M6-4]。停止。

## 进度
- [x] 读 cassette mod/record/replay + M3-4 test + interaction basic + fixtures/handlers/script，确认 API
- [x] 写 3 个 tests/*.rs（各 regenerate + replay）
- [x] 生成 3 个 cassette JSON（UPDATE=1）
- [x] 离线回放 + 全套 + fmt/clippy/doc/diff 全绿
- [x] docs/TESTABILITY.md §8.3 record/update 文档
- [x] TODO.md 标 [DONE] + 完成记录
- [ ] commit（[M6-4]）。停止 ← 进行中
