## Execution Plan — M3-2：流式状态机 normalizer.rs（文本/reasoning/工具增量/终态）

TODO.md 第一个未完成任务：**M3-2**（`[TODO]`）。把 `stream/normalizer.rs` 从 M3-1 哨兵骨架
升级为完整 chunk→`Vec<StreamEvent>` 状态机。设计文档 §4.4。

### 关键事实（已核对）

- **SSE 契约**（`common/sse.rs`）：`translate` 返回事件追加进 pending；`is_terminal()` 为 true 后流立即
  `None` 结束（**无法再发事件**）；EOF 未 terminal → `incomplete_error()`。
- **accumulator 约束**（`stream/accumulator/mod.rs`）：必须发 `MessageStart`、`MessageStop`；
  **`MessageStop` 之后不能有任何事件**；每个打开的 block 必须 `BlockStop`；`Usage` 加性只要在 `MessageStop` 前。
- **wire 顺序矛盾**：`finish_reason` 在末个内容 chunk，usage 在其后（空 choices 独立 chunk）。若
  `finish_reason` 时即发 `MessageStop`，后续 `Usage` 会落在 `MessageStop` 后 → 违反 accumulator。

### 核心设计决策

1. **延迟 `MessageStop` 到 `[DONE]`**：`finish_reason` chunk 只缓存 `stop_reason`（复用 M2-1
   `normalize_finish_reason`）；`Usage` 到达即发；`[DONE]` 时 flush：关闭所有打开 block + 发 `MessageStop`。
   - `Usage`（finish_reason 后到达）< `MessageStop`（[DONE] 时），满足 accumulator ✓；
   - stop_reason 仍来自 finish_reason，符合 §4.4.4 语义 ✓。
2. **每 kind 一个活跃 block**：`active_text`/`active_reasoning: Option<BlockId>`；content 续接同一 text
   block，reasoning 续接同一 reasoning block（DeepSeek 实际流 reasoning 全→content 全，各开一次）。
   block id：`text`/`reasoning`/`tool-call-{wire_index}`（位置派生稳定 id）。统一在 `[DONE]` 关闭
   （固定序 reasoning→text→tools by index），不在中途关。
3. **不发 `ToolInputAvailable`**：严格遵循 §4.4.2「`BlockStart(ToolInput)+Delta::Json+BlockStop`，绝不
   中途解析 JSON」。适配器零 JSON 解析，accumulator 在 `BlockStop` 时自己解析 accumulated arguments
   （合法 fixture 下与非流式 ToolUse 一致）。— 与 openai_resp 不同（Responses 有 `arguments.done` 显式
   边界才发 ToolInputAvailable），chat/completions 无此边界。
4. **tool_call 首片**：index 首次出现 = 首片，必须带 `id`+`function.name`（§4.4.2 只在首 chunk），
   开 `BlockStart(ToolInput)`，缺则 `ClientError::Protocol`；后续片只发 `Delta::Json`（非空时）。
5. **role**：固定 `Role::Assistant`，但读 `delta.role` 字段验证若存在须为 `"assistant"`（移除 wire.rs
   role 的 dead_code allow，对齐 M2-1 convert_message role 验证）。

### finish_reason 复用

`normalize_finish_reason` 在 `response/convert.rs` 当前 `pub(super)`。改为 **`pub(crate)`**
（crate-private，任务要求），stream 经全路径引用。最小改动，convert.rs 本就是映射的家。

### 文件改动

| 文件 | 动作 |
|---|---|
| `response/convert.rs` | `normalize_finish_reason`：`pub(super)`→`pub(crate)`。 |
| `stream/wire.rs` | 移除 5 个 struct 过渡 `#[allow(dead_code)]`；更新模块文档。 |
| `stream/normalizer.rs`（核心） | 完整状态机，替换 M3-1 桩。 |
| `stream/tests/mod.rs` | 重写 M3-1 两骨架测试为完整状态机测试（decode_fixture 端到端断言精确事件序列）。 |

### 执行步骤

1. [x] 上下文读取（TODO/PLAN/设计文档 §4.4 + common/sse + accumulator + openai_resp normalizer
   terminal/item MessageStart/finish_arguments + anthropic block_id 先例 + wire/tests/response 现状）。
2. [x] `response/convert.rs`：normalize_finish_reason → pub(crate)（+ `response.rs` `mod convert`→`pub(crate) mod convert`）。
3. [x] `stream/wire.rs`：移除 5 个 struct allow + 更新文档。
4. [x] `stream/normalizer.rs`：完整状态机（MessageStop 延迟 [DONE] / 每 kind 活跃 block / 不发 ToolInputAvailable / tool 首片 id+name / role 验证）。
5. [x] `stream/tests/mod.rs`：12 测试（6 场景 + finish_reason 全表 + role/空流/[DONE]/event:message/wire），decode_fixture 改 impl AsRef<str>。
6. [x] 门禁全绿：fmt 无 diff / 默认+external clippy exit 0 / test -p openai_chat 40 通过 / test --all lib 1094→1102 无 failed / doc exit 0。
7. [x] TODO M3-2 [TODO]→[DONE] + 完成记录。
8. [ ] git commit + stop。

### 进度日志

- [x] 上下文读取 + 设计决策定稿（MessageStop 延迟解决 wire 顺序矛盾是关键洞察）
- [x] convert.rs / response.rs 可见性放宽（finish_reason 复用）
- [x] wire.rs 移除过渡 allow + 文档
- [x] normalizer.rs 完整状态机
- [x] tests/mod.rs 12 测试（修复 clippy needless_borrow：decode_fixture 改 impl AsRef<str>）
- [x] 门禁全绿
- [x] TODO M3-2 [DONE] + 完成记录
- [ ] git commit + stop
