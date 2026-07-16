# M2-2 — 将 `PausedForToolCalls` 折成 `NeedTool` batch

**当前执行 = TODO.md 第一个未完成任务 = M2-2**(M1-1..M1-4、M2-1 已 `[DONE]`)。
M2-1 已落地数据结构脚手架:`ExternalAgentCursor::AwaitingTool { batch_id, requirements }`、
machine 非序列化 scratch `PendingExternalToolBatch`(带 `#[expect(dead_code)]` staging)。
M2-2 = **producer**:在 `fold_session_result` 新增 `PausedForToolCalls` 分支,构造 batch + 发 `NeedTool`。
M2-3 = consumer:`resume_tool` 收齐 result 回灌 `RespondToolResults`(不在本次)。

## 关键事实(已核对)
- `RequirementKind::NeedTool { call_id: ToolCallId, call: ToolCall }`;`RequirementResult::Tool(Result<ToolResponse,ToolRuntimeError>)`。
- `ExternalToolCall::to_tool_call()` 保留 `provider_call_id` 作为 `ToolCall::id`。
- `ToolExecutionIds::tool_call_id(&ToolCall) -> Result<ToolCallId, ToolRuntimeError>`;默认 `NoToolExecutionIds` 返回 `IdUnavailable`。
- `LoopCursor::awaiting_tool(step_id, Vec<ToolCallId>, Some(ToolWaitRequirements::root(ids)))` → 校验 keys 与 call_ids 完全一致;空集 → `EmptyToolWait` err。
- `pending_requirement_ids()` 对 `AwaitingTool` 返回 requirements.ids().values()。
- 内部对照实现:`src/agent/machine/default/tools.rs::emit_tool_batch`。

## dead_code 处理(已实测 rustc edition2024)
- machine 字段 `pending_tool_batch`:一旦 M2-2 写入 `Some(构造值)`,字段不再被判 dead → **移除该字段上的 `#[expect(dead_code)]`**(留着会 unfulfilled 报错)。
- `PendingExternalToolBatch` struct:字段在 M2-2 只构造不读取 → dead_code 仍触发 → **保留 struct 级 `#[expect(dead_code)]`**,reason 改为「drained by M2-3 RespondToolResults collection」。

## 实现步骤(machine.rs)
- [ ] imports 增加:`agent::{NoToolExecutionIds, ToolExecutionIds, ToolWaitRequirements}`、`conversation::ToolCallId`、`model::tool::ToolCall`。
- [ ] `ExternalAgentMachine` 新增字段 `tool_ids: Arc<dyn ToolExecutionIds>`;`new` 默认 `Arc::new(NoToolExecutionIds)`;新增 builder `with_tool_execution_ids`(兼容,不改 `new` 签名)。
- [ ] 移除 `pending_tool_batch` 字段上的 `#[expect(dead_code)]`;更新字段 doc。
- [ ] `PendingExternalToolBatch` struct 的 `#[expect(dead_code)]` reason 改为仅指向 M2-3 drain;更新 struct doc(fold 已实现,仅 drain 待做)。
- [ ] `fold_session_result` 的 `PausedForToolCalls` 分支改为解构 `{session, batch_id, calls, observations}` → observe → `pause_for_tool_calls(...)`。
- [ ] 新增 `pause_for_tool_calls`:要求 in_flight;`set_session`;逐 call 分配 `ToolCallId`+`RequirementId` 发 `NeedTool`;失败(无 tool ids / id 不可用 / cursor 构建失败)→ `fail_with(..., notifications)`;成功 → 写 scratch + settle `AwaitingTool` + `LoopCursor::awaiting_tool`。不写 Conversation。
- [ ] `abandon` / `fail_with` 增加 `self.pending_tool_batch = None;` 清空(class-wide 正确性)。
- [ ] 模块 doc 增加 tool-pause 覆盖 bullet。

## 测试(machine/tests.rs)
- [ ] 新增 `SeqToolIds`(impl ToolExecutionIds:`tool_call_id` 从池按序发,其余方法返回 IdUnavailable/不被调用)+ helper `machine_with_tool_ids`、`paused_for_tools(batch, calls)`。
- [ ] `external_tool_pause_emits_need_tool_batch`:requirements.len==calls.len;每个 NeedTool.call.id==provider_call_id;`cursor().pending_requirement_ids()` 含全部;cursor kind==AwaitingTool;pending Conversation 打开未提交;session 已记录。
- [ ] `external_tool_pause_without_tool_ids_fails`:默认 machine(NoToolExecutionIds)→ 收到 PausedForToolCalls → cursor kind==Error;pending turn discard(pending().is_none())。

## 验证序列 1-6
1. `cargo fmt --all -- --check`
2. 聚焦:`cargo test -p agent-lib external_tool_pause_emits_need_tool_batch`
3. 聚焦:`cargo test -p agent-lib external_tool_pause_without_tool_ids_fails`
4. `cargo clippy --all-targets -- -D warnings`(含 expect 兑现检查)
5. `cargo test --all --all-targets`(≤30min)
6. `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` + `git diff --check`

## 状态:已完成(2026-07-17)
machine.rs 落地 `tool_ids` 字段 + `with_tool_execution_ids` builder + `pause_for_tool_calls`;
`fold_session_result` PausedForToolCalls 分支改为发 NeedTool batch;abandon/fail 清 pending_tool_batch;
dead_code:移除 machine 字段 expect、保留 struct expect。tests 新增 SeqToolIds + 2 test。
验证 1-6 全过:fmt OK / 聚焦 2 passed / clippy 0 / 全量 791 passed 0 failed / doc -D warnings 绿 / diff clean。
M2-2 已在 TODO.md 标 [DONE] + 完成记录。待 commit。
