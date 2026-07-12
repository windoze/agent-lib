# 当前 invocation 执行计划

## 目标与边界

- 以 `TODO.md` 为唯一任务排序与验收依据，识别标题中第一个没有 `[DONE]` 前缀的任务。
- 本次只完成该任务；若出现会阻塞其正确实现的真实前置问题，则按规则在 `TODO.md` 中加入最少的前置任务、提交后停止。
- 不开展与当前任务无关的历史问题扫描，不通过缩小范围、替换建模方式、特例或其他 workaround 绕开规范。
- 保留用户已有改动；若这是一次中断后的续作并最终完成当前任务，则把当前未提交文件一并纳入本次原子提交。

## 执行步骤

1. 读取 `TODO.md`，严格按标题的 `[DONE]` 状态确定第一个未完成任务，并完整阅读其需求、依赖、验收与完成记录。
2. 检查最新一次 Git 提交说明；只把其中与当前任务直接相关且明确未完成的问题纳入范围。随后检查工作区状态，区分已有改动与本次工作，并确认是否属于中断续作。
3. 按当前任务需要，定向阅读 `PLAN.md`、`DESIGN.md`、相关源码、测试和仓库级说明；不做开放式历史缺陷巡检。
4. 在开始实现前，把已识别的任务编号、具体需求、影响文件、测试矩阵及任何新发现更新到本文件。
5. 以小而聚焦的补丁完成实现；每个关键步骤后重新阅读受影响区域，并及时更新本文件的进度与计划变化。
6. 增补或调整测试，覆盖任务规定的正常路径、边界、错误分类、原子性/不变量及公开 API（以任务正文为准）。任何测试失败都按失败策略处理：立即修复，或在 `TODO.md` 中安排位于依赖任务之前的最小前置任务。
7. 按规定顺序验证：
   - `cargo fmt --all`
   - `cargo clippy --all-targets -- -D warnings`
   - `cargo test --all --all-targets`（最长 30 分钟）
   - 若任务或仓库验收要求包含文档，则运行 `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`
   - 运行 `TODO.md` 为当前任务列出的其他定向验证。
8. 验证全部通过后，在 `TODO.md` 的任务标题加 `[DONE]`，填写真实完成记录、改动摘要与验证结果；仅当阶段级依赖、顺序、假设或完成标准改变时才更新 `PLAN.md`。
9. 复查差异和工作区，确认没有无关或遗漏改动、没有秘密信息、没有未调度失败；更新本文件为最终状态。
10. 使用包含任务编号的清晰消息提交所有应纳入当前任务的改动，确认提交成功和工作区状态后立即停止，不开始下一任务。

## 已锁定任务

- 当前任务：`M2-1 [TODO] Tool result 完整状态模型前置修复`。
- 直接依赖：`M1-R [DONE]`，已满足。
- 核心要求：
  - 把 `ContentBlock::ToolResult` 的权威结果从 `is_error: bool` 改为 `ToolStatus`，消除双事实。
  - 建立 `ToolResponse` 与 tool-result block 的明确无损转换。
  - 新序列化只输出权威 `status`；如保留旧 JSON 兼容，则集中接受 `is_error` 并映射到
    `Ok/Error`，同时拒绝 `status` 与 `is_error` 冲突。
  - 类级更新 Anthropic/OpenAI 的 request mapper、response parser、fixtures、examples 与所有
    pattern match；wire 无法证明具体状态时只归一为 `Error`。
  - 覆盖四种 status、转换、旧格式迁移/冲突、两家 adapter request/fixture，并回归非流式、
    流式、tool round-trip 与跨 provider 测试。
- 最新提交：`977513b [M1-R] Review immutable conversation boundary`，提交说明没有 M2-1
  相关未完成问题，不新增前置任务。
- 初始工作区：除本次更新的 `memory/claude_plan.md` 外干净；不是遗留未提交续作。

## 当前状态

- 状态：M2-1 实现与全部验收完成，`TODO.md` 标题已改为 `[DONE]` 并填写完成记录；正在做
  提交前最终 diff/工作区检查。
- 计划变更：无。
- 验证结果：
  - `cargo fmt --all`：通过。
  - `cargo clippy --all-targets -- -D warnings`：通过，无 warning。
  - `cargo test model::`：38 passed，0 failed。
  - `cargo test adapter::`：60 passed，0 failed。
  - `cargo test --all --all-targets`：174 个库单测与 3 个离线集成测试 passed；7 个真实
    endpoint tests ignored；0 failed；examples targets 编译；设置了 1800 秒硬上限。
  - `cargo test --doc`：1 个正向与 6 个 compile-fail passed。
  - `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`：通过。
  - `git diff --check`：代码完成后通过；TODO/本文件更新后将再执行一次。
- 提交：已以 `[M2-1] Preserve complete tool result status` 创建本次原子提交。

## 实现设计与影响面

- `ContentBlock::ToolResult` 将以 `status: ToolStatus` 替换 `is_error`，新 normalized JSON 始终
  输出 `status`，并在序列化时过滤 `extra` 中可能伪造的 `status`/`is_error` 等已建模键。
- 集中式反序列化迁移规则：
  - 只有 `status`：按四态读取；
  - 只有旧 `is_error`：`false -> Ok`、`true -> Error`；
  - 两者都缺失：按旧格式中被省略的 `is_error=false` 迁移为 `Ok`；
  - 两者都有：仅接受 `Ok + false` 或 `Error + true`，其余组合明确报冲突；
  - 字段出现但类型为 null/非预期类型时拒绝，不静默当作缺失。
- 为保证 `ToolResponse <-> ContentBlock::ToolResult` 双向转换连 `extra` 也不丢，给
  `ToolResponse` 增加默认可省略的扩展 map，并实现消费式 `From`/`TryFrom`；非 tool-result
  转换失败时由专门错误归还原 block。
- Anthropic request：`Ok` 映射为省略 `is_error`，`Error/Denied/Cancelled` 映射为
  `is_error=true`；OpenAI Responses request：`Ok -> completed`，其余三态 `-> incomplete`。
  两种 wire 都不会反向虚构 `Denied/Cancelled`。
- 定向影响文件：`model/content*`、`model/tool.rs`、`model/message.rs`、两家 request mapper
  与测试、Conversation fixtures/tests、normalization scenario、`tool_round_trip` example；随后
  用 `rg` 确保不再有模型层 `is_error` pattern/constructor 遗留。

## 决策记录

- 用户要求先写计划再执行命令，因此本文件是本次 invocation 的第一项文件操作。
- 这里记录可审计的计划、事实、判断依据和结果，不记录模型内部的隐式逐字推理。
- `TODO.md` 中首个无 `[DONE]` 标题为 M2-1；此前 M1-1、M1-2、M1-3、M1-R 均明确
  标为 `[DONE]`。
- 最新提交没有显式 unfinished issue，故不改变 TODO 顺序。
- Anthropic/OpenAI 的 assistant response parser 只产生 text/thinking/tool-use，不会收到本地
  tool-result；本任务的 provider 降格发生在 request mapper。旧持久化/normalized JSON 的
  反向归一化统一由 `ContentBlock` serde migration 处理。
- 模型聚焦与两家 adapter 请求聚焦测试均首次通过；尚未观察到测试失败或新的规范缺口。
- 全量测试、rustdoc 与 doctest 均通过，没有失败需要按 Test Failure Policy 新增任务。
- `PLAN.md` 的阶段顺序、依赖和完成标准未改变，按规则不做例行修改。
