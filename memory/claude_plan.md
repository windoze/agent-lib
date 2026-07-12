# 当前任务执行计划

> 本文件记录可审计的决策依据、执行步骤、关键进展与验证结果；不记录私有的逐字思维链。

## 初始目标

- 以 `TODO.md` 为唯一任务顺序与完成状态来源。
- 找出标题中第一个没有 `[DONE]` 前缀的任务。
- 在本次调用中完整实现、验证、记录并提交该任务，然后停止，不处理后续任务。

## 执行步骤

1. 首先完整读取 `TODO.md`，只为确定首个未完成任务及其要求、依赖和指定验证。
2. 检查最新提交是否明确提到与该任务直接相关的未完成问题，并检查工作区是否存在上次中断遗留的未提交改动；不进行开放式历史缺陷扫描。
3. 读取该任务直接涉及的设计文档、源码与测试，确认实现边界；如发现会阻塞该任务的真实前置缺陷，按规则在 `TODO.md` 中插入最小前置任务、提交并停止。
4. 若无阻塞，按任务原定执行单元完整实现；采用小而集中的补丁，并在关键步骤后回读相关代码。
5. 补充或更新测试，先运行针对性验证并修复所有相关失败。
6. 按规定顺序运行最终验证：`cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets`（完整测试最长 30 分钟），以及任务明确要求的其他检查。
7. 更新 `TODO.md`：仅在全部要求和验证通过后，为当前任务标题加 `[DONE]` 并填写完成记录；只有阶段级计划确实变化时才更新 `PLAN.md`。
8. 检查最终 diff 和状态，确保没有遗漏本次任务或恢复任务留下的未提交文件；用清晰的任务编号提交全部应提交改动。
9. 提交后确认工作区状态与提交内容，然后停止。

## 当前状态

- 已建立初始执行计划。
- 已读取 `TODO.md` 并确认首个未完成任务为 `M2-3 PendingTurn 事务推进与多轮 tool 记账`；直接依赖 `M2-2` 已标记完成。
- 本次范围固定为：`begin_turn`、assistant 冻结与 ToolCallId 映射、open-call 记账、tool response 追加、ready-to-commit、usage/meta 汇总，以及复用 M1 validator 的原子 `commit_pending`。
- 任务要求的正向场景包括纯文本、串行两轮工具、parallel 分批结果、stream/non-stream 混合与 usage 汇总；负向场景包括重复 begin、未知/重复 result、缺映射、未闭合时推进/提交、终态错误和 id 冲突。
- 下一步只检查最新提交是否明确留下与 M2-3 直接相关的问题、工作区是否有恢复任务遗留，以及现有 pending/commit API；不做开放式历史缺陷扫描。

## 设计确认（M2-3）

- 最新提交为已完成的 `[M2-2] Implement pending message freeze boundary`，没有明确留下与 M2-3 直接相关的未完成问题；初始工作区除本进度文件外干净。
- `Conversation` 将新增唯一的 `Option<PendingTurn>`；`PendingMessage` 继续保持不可克隆、不可 serde，因此不会用共享可变指针伪造 `Conversation: Clone`。现有只针对 committed 原子性的测试将改为比较可观察 committed 全结构。
- `PendingTurn` 只公开只读状态：已冻结消息、phase、tool-call 记账、usage 与 response metadata；所有推进都经 `Conversation` 方法完成，不公开 mutable getter 或 raw push。
- 状态机采用：`AwaitingAssistant → AssistantInProgress → AwaitingToolCallMappings → AwaitingToolResults → AwaitingAssistant`，无 tool-use 的 assistant 直接进入 `ReadyToCommit`。
- assistant 冻结和 ToolCallId 映射分成两个显式步骤。冻结后 immutable assistant message 留在 pending；映射必须完整、一一对应且 conversation-wide ToolCallId 唯一。错误映射不修改记账，可重试。
- tool result 追加前检查 provider call 是否已登记且仍 open、MessageId 是否唯一、block 是否为完整合法 tool-result；parallel result 可逐条追加，全部闭合后才允许下一条 assistant。
- `commit_pending(meta)` 从 ready pending 克隆 data-only draft，复用唯一 M1 validator；失败保留 pending 和 committed history，成功后才清空 pending。
- 每次 assistant 的 usage 自动聚合进最终 `TurnMeta`；stop reason 与 response-level provider metadata 以 typed per-response metadata 随 TurnMeta 保存，避免多轮响应元数据互相覆盖或丢失。

## 实施与验证进展

- 已实现唯一 `PendingTurn`：受检 `begin_turn`、stream/non-stream assistant 启动与冻结、精确 ToolCallId 映射、open-call 只读记账、完整 tool result 追加、多轮往返、ready-to-commit 与原子 `commit_pending`。
- 已新增 typed `TurnResponseMeta` 并接入 `TurnMeta` serde；每条 assistant 的 stop reason/provider extra 按消息保留，所有 response usage 聚合到 Turn metadata。
- 已拆分为 `pending/turn.rs`（生命周期/assistant/commit draft）与 `pending/turn/tool.rs`（mapping/open call/result），没有公开 mutable pending、raw push 或第二个 validator。
- `PendingMessage` 继续不可克隆；因此 `Conversation` 不再提供会复制 active accumulator 的 `Clone/Eq`。旧 validator 原子性测试改为完整 committed-state snapshot 比较，未削弱断言。
- 新增 12 个 M2-3 聚焦测试，覆盖纯文本、串行两轮工具、parallel 分批结果、stream/non-stream 混合、usage/metadata、四类 mapping 错误、未知/重复/非法 result、阶段门、identity 冲突、validator 失败原子性和错误后成功继续推进。
- 验证已通过：`cargo fmt --all`；`cargo clippy --all-targets -- -D warnings`；`cargo test conversation::pending::turn`（12 passed）；1800 秒硬上限内 `cargo test --all --all-targets`（195 个库测试与 3 个离线集成测试 passed、7 ignored、0 failed，全部 example targets passed）；`cargo test --doc`（1 个正向与 9 个 compile-fail passed）；`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`。
- 完整测试后只修正了两处 rustdoc 链接文本；compiled output 未变化，因此按任务规则不重复完整 suite，并已重新通过 format、严格 clippy、doctest 与 rustdoc。
- 已完成完整 diff 审查，并把过长的负向测试按 begin/mapping/results/identity/commit 拆分；拆分后的严格 clippy、12 项聚焦测试、1800 秒上限完整 suite、doctest 与 rustdoc 已再次全部通过。
- `TODO.md` 中 `M2-3` 已标记 `[DONE]` 并写入实际完成记录；`PLAN.md` 的阶段顺序/依赖/完成标准没有变化，因此未修改。
- 最终 `git diff --check`、状态与 staged 内容审查均已通过；`[M2-3] Implement pending turn transaction` 提交已完成。本次调用到此停止，不进入 `M2-4`。
