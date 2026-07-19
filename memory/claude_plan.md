# 执行计划

## 当前任务：M3-6 `finish_assistant` 前置块级校验（M-CONV-5）

任务来源：`TODO.md` M3-6。前置状态：M3-5-1~4 已全部完成并提交。

### 问题（来自 TODO.md / docs/review-2026-07.md M-CONV-5）

`finish_assistant`（`src/conversation/pending/turn.rs:195-239`）只抽 tool_use id，不做块级校验：

1. 同一 assistant 消息内重复 tool_use id → `register_tool_calls` 恒 `DuplicateProviderCallId`
   （`pending/turn/tool.rs:126-138`），永久卡死只能 cancel。
2. assistant 消息含 Image/ToolResult 块 → commit 时 `validate_role_sequence` 报
   `InvalidRoleBlock`（`validation/sequence.rs:179-197`），而 `ReadyToCommit` 禁止
   ResumeTurn/CommitTurn cancel（`pending/cancel/prepare.rs:189-193`），只剩 DiscardTurn，
   整轮已冻结的 tool 往返作废。
3. 同类（class-wide）：空 id/空 name 的 tool_use 块 → commit 时 `IncompleteContent`，
   同样只剩 DiscardTurn。

### 方案

**单一来源（规则抽取）** — `src/conversation/validation/sequence.rs`：

- 新增 `pub(crate) const fn block_allowed_for_role(role, block) -> bool`：从
  `inspect_message` 的 role/block allowlist 抽出，`inspect_message` 改调它。
- 新增 `pub(crate) fn incomplete_tool_use_detail(id, name) -> Option<&'static str>`：
  从 `validate_complete_tool_use` 抽出规则本体，原函数包装成 CommitError。
- `validation.rs` re-export 这两个 helper（`pub(crate) use`）。

**预检** — `pending/turn.rs` `finish_assistant`：freeze 成功、`into_parts` 之后、
任何状态突变（usage merge / push message / 改 state）之前，跑
`validate_assistant_blocks(&message, &self.tool_calls)`：

- 非法块类型（Image/ToolResult）→ 新变体 `PendingTurnError::InvalidAssistantBlock { block }`
  （镜像既有 `InvalidUserBlock`）。
- tool_use 空 id/空 name → 新变体 `PendingTurnError::IncompleteToolUse { detail }`。
- 消息内重复 tool_use id，或与本轮已注册（`self.tool_calls`，覆盖此前所有 step 的
  provider id，因状态机保证前序 assistant 的 id 都已注册）重复 → 复用
  `PendingTurnError::DuplicateProviderCallId`（其文案本就是 "duplicated in the pending turn"）。

预检失败时 pending turn 停在 AssistantInProgress（PendingMessage 已 Frozen，内容来自
wire 不可修复，重试必然同样失败），调用方走 DiscardTurn——正是任务要求的
「报错后可正常 DiscardTurn 并继续 feed」。

`register_tool_calls` 内的重复检查保留为防御（不再可达，但不删）。

### 错误变体（src/conversation/error.rs，PendingTurnError）

- `InvalidAssistantBlock { block: ContentBlockKind }`：
  "a pending assistant response cannot contain a {block} block"
- `IncompleteToolUse { detail: &'static str }`：
  "a pending assistant response has an incomplete tool-use block: {detail}"

### 测试（pending/turn/tests/errors/，新增 finish.rs）

1. 重复 tool_use id 在 finish_assistant 即报 `DuplicateProviderCallId`；报错后
   DiscardTurn 成功、可 begin 新 turn 继续 feed。
2. Image 块 / ToolResult 块在 finish_assistant 即报 `InvalidAssistantBlock`。
3. 空 id / 空 name tool_use 即报 `IncompleteToolUse`。
4. 第二 step 的 assistant 重用第一 step 已注册的 provider id → finish 即报
   `DuplicateProviderCallId`（此前要到 register_tool_calls 才报）。
5. 改写既有 `mapping.rs::duplicate_provider_calls_are_rejected_before_open_call_registration`
   ——重复 id 现在在 finish 即被拒（freeze_response 的 expect 会失败，必须更新）。

### 文档

- `finish_assistant`（pending/turn.rs 与 conversation/mod.rs）rustdoc 补预检说明。
- 新错误变体 rustdoc。
- `docs/conversation-core.md` §5 Pending 区补一条 finish 时块级预检说明。
- `docs/review-2026-07.md` M-CONV-5 标注 `✅ 已修复（M3-6）`。

### 验证

- `cargo test -p agent-lib --lib conversation::pending` 全过。
- 全量门禁：fmt → clippy（默认 + external features）→ `cargo test --all --all-targets`
  → cargo doc（-D warnings）。

### 执行步骤

1. sequence.rs 抽 shared helper + validation.rs re-export。
2. error.rs 加两个 PendingTurnError 变体。
3. pending/turn.rs 加 `validate_assistant_blocks` 并接入 finish_assistant + rustdoc。
4. 新增 errors/finish.rs 测试；更新 mapping.rs 既有测试。
5. 门禁 + 文档（conversation-core.md、review 标注）。
6. TODO.md 标 [DONE] + 完成记录；提交 `[M3-6] ...`。

### 进度记录

- [x] 读取 TODO.md 确认首个未完成任务为 M3-6
- [x] 探索代码：finish_assistant / register_tool_calls / validate_role_sequence /
  cancel prepare / 测试结构与 helper
- [x] sequence.rs 抽出 `block_allowed_for_role` + `incomplete_tool_use_detail`，
  validation.rs re-export（单一来源）
- [x] error.rs 新增 `InvalidAssistantBlock` / `IncompleteToolUse` 两个 PendingTurnError 变体
- [x] pending/turn.rs 接入 `validate_assistant_blocks` 预检 + rustdoc
- [x] 新增 errors/finish.rs 4 条测试；更新 mapping.rs（删 register 时重复用例）、
  commit.rs（Image commit 用例移出）、cancel/errors/state.rs（删不可构造的 cancel 重复用例）
- [x] 门禁全过：fmt / clippy×2 / `cargo test --all --all-targets`（exit 0，50 目标）/
  external features 套件（exit 0，48 目标）/ cargo doc（-D warnings）
- [x] 文档：conversation-core.md §5 预检段、review-2026-07.md M-CONV-5 标注 ✅
- [x] TODO.md 标 [DONE] + 完成记录

## 任务完成总结

M3-6 已完成并标 `[DONE]`。核心交付：
1. 块级规则单一来源：sequence.rs 抽 `block_allowed_for_role` /
   `incomplete_tool_use_detail`，commit 与 freeze 两边界共享。
2. `finish_assistant` 前置预检：非法块类型（InvalidAssistantBlock）、不完整 tool_use
   （IncompleteToolUse）、重复 provider call id（DuplicateProviderCallId）尽早失败，
   报错后 turn 可 DiscardTurn 并继续 feed。
3. 4 条新测试 + 3 处既有测试按新边界更新；全量门禁（含 external features）全过。
下一任务：M3-7（`resolved_provider_call_id` 按 claimed 排除语义重推导）。
