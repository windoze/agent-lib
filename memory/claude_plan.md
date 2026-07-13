# 本次执行计划

## 目标与边界

- 本次调用只完成 `TODO.md` 中按顺序出现的第一个标题未带 `[DONE]` 的任务，然后停止。
- `TODO.md` 是任务顺序、依赖、验收要求与完成记录的唯一依据；只有阶段级计划发生变化时才更新 `PLAN.md`。
- 不进行开放式历史缺陷扫描。只检查当前任务、最新提交直接提及且与当前任务相关的未完事项，以及验证过程中实际暴露的失败。
- 不采用缩小范围、替代表示、临时兼容层或其他规避规范的做法。若出现无法在当前任务内正确解决的具体前置阻塞，则只添加最少的前置任务、提交该调整并停止。
- 本文件记录可复核的计划、事实、决策与进度，不记录模型隐藏的逐字思维链。

## 分步计划

1. 读取 `TODO.md`，按标题顺序找出第一个未带 `[DONE]` 的任务，完整读取其需求、依赖、测试要求和完成记录；同时读取相关的 `PLAN.md` 部分。
2. 检查最新一次 Git 提交说明和当前工作区状态：
   - 判断最新提交是否明确提到与当前任务直接相关的未完问题；
   - 识别是否存在上次中断遗留的未提交改动；
   - 保留用户已有的无关改动，不擅自回退或覆盖。
3. 将识别出的任务编号、验收条件、相关文件和具体实施步骤补充到本文件，再开始实现。
4. 检查当前任务涉及的实现与测试边界，确认没有必须先修复的规范偏差。若有直接阻塞：优先在当前任务内做类级修复；若确实必须成为独立前置任务，则更新 `TODO.md` 的依赖顺序、记录阻塞、提交并停止。
5. 以小而聚焦的补丁完成实现；每个关键步骤后重新读取受影响代码，并同步更新本文件的进度和必要的计划调整。
6. 添加或更新覆盖正常路径、边界条件、错误路径和回归场景的测试。任何新观察到且未被后续任务明确安排的失败，都必须在当前任务中修复或作为最少前置任务写入 `TODO.md`。
7. 按规定顺序验证：
   - `cargo fmt --all`
   - `cargo clippy --all-targets -- -D warnings`
   - `cargo test --all --all-targets`（最长 30 分钟）
   - 按当前任务要求运行额外测试或文档构建；若任务要求完整交付验证，则运行 `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`。
8. 验证全部通过后，在 `TODO.md` 中把当前任务标题显式加上 `[DONE]`，填写包含实现内容和验证结果的完成记录。仅在阶段级依赖或完成标准改变时更新 `PLAN.md`。
9. 复查 diff、任务边界、文档和 Git 状态，确保没有敏感信息、无关改动或未调度失败；若是恢复中断任务，则按要求将所有现存未提交文件纳入同一提交。
10. 使用包含任务编号的清晰提交信息提交全部本次改动。确认提交成功、工作区符合预期后停止，不开始下一个任务。

## 当前进度

- [x] 在运行其他命令前建立本计划文件。
- [x] 识别并完成本次任务：`M3-1 [DONE] 结构共享 raw history 与派生 ToolCallIndex`。
- [x] 完成实现与测试。
- [x] 更新 `TODO.md` 完成记录。
- [ ] 提交并停止。

## 当前任务的验收映射

- 结构共享 history 必须保存全部 raw `Turn`、parent 关系、当前 active lineage 和有效 tip；
  clone/fork 所需路径不得遍历或深拷贝历史节点。
- lineage 改道后，旧 suffix 仍须按 `TurnId` 保留和可读，但不得进入当前有效视图。
- `ToolCallIndex` 只能由 closed turns 与 pending 派生，可按内部 `ToolCallId` 和 provider call
  id 定位 call/result；增量更新必须与全量重建等价，且只反映 active lineage + 当前 pending。
- validator 仍是 history 唯一写入口；Turn/Message/ToolCall 的重复 id 检查必须覆盖所有 retained
  raw 节点，包括隐藏旧分支。
- 测试需覆盖长历史节点共享、append 不改变旧节点、serial/parallel index 定位、增量/重建
  等价，以及隐藏分支不进 index但 raw 节点仍存在。

## 恢复现场记录

- 最新提交 `59dbb24061429ca45e13115efd40db72c222eaa4` 仅完成 `M2-R`，没有明确提及与
  `M3-1` 直接相关的额外未完问题。
- 初始工作区已经有 `M3-1` 相关未提交改动：`src/conversation/history.rs`、
  `src/conversation/history/`、`mod.rs`、pending cancel、Turn 测试和 validation 文件。
  这次按中断续作处理：先审计而不是覆盖，并在任务完成时将所有当前未提交文件一起提交。
- `memory/claude_plan.md` 是本次按要求首先更新的进度文件。

## 接下来实施顺序

1. 完整读取 `PLAN.md` 的 M3 约束、`docs/conversation-core.md` 的 raw/head/index 规范，以及现有
   Conversation/Turn/Pending API；同时审查全部未提交 diff。
2. 对照验收映射列出遗留实现的缺口，重点检查结构共享复杂度、隐藏分支 identity、pending
   index 生命周期、commit/cancel 原子性以及 public API 是否提前泄漏 M3-2 功能。
3. 以小补丁补齐实现与测试；每个关键阶段在本文件记录实际完成情况和计划变化。
4. 按 format → clippy → 聚焦测试 → 全量测试 → rustdoc → diff check 顺序验证。
5. 标记 `M3-1 [DONE]`、写完成记录、复核并提交；不触碰 `M3-2` 的实现。

## 实施进度记录

- 已完整读取 M3 阶段计划和 `docs/conversation-core.md` 中 raw history、parent tree、head、
  fork 与派生 index 约束；确认本任务不提前引入受检 `Boundary` 或公开 revert/fork API。
- 已审计中断遗留实现：`History` 用 `Arc<Lineage>`、`Arc<HistoryNode>` 和持久化 raw entry
  链实现 O(1) clone；active prefix 改道后旧节点仍在 raw scope。commit validator、pending
  identity 和 cancel identity 均已改为扫描 retained raw facts，而非读取派生 index。
- 初次聚焦编译发现 `history/tests/mod.rs` 声明但缺少 `tests/index.rs`，同时全量重建函数仅在
  测试配置使用会产生 dead-code warning。已补齐模块化 index 测试，并将只读
  `ToolCallIndex::rebuild` 作为有 rustdoc 的公共派生构造公开，避免把 index 变成事实来源。
- 已补充 4 类 index 回归：parallel + serial 生命周期逐步对比增量/重建、跨 Turn 重复
  provider id 的有序多结果、缺省 persisted provider id 的受检 content-anchor 解析、cancel
  Resume/Commit/Discard 的 pending suffix 同步。无效 mapping 另断言 index 原子不变。
- 当前聚焦结果：`cargo test conversation::history --no-fail-fast`，6 passed、0 failed；最长
  用例约 0.02 秒总计，满足单测时限。
- 下一步：更新 README/crate 级能力说明，随后按规定从 `cargo fmt --all` 开始正式验证。

## 最终验证进度

- [x] `cargo fmt --all`。
- [x] `cargo clippy --all-targets -- -D warnings`，0 warning。
- [x] `cargo test conversation::history -- --nocapture`：6 passed、0 failed。
- [x] `/opt/homebrew/bin/timeout 1800 cargo test --all --all-targets`：220 个库测试与 3 个
  离线集成测试 passed，7 个需真实凭据的 endpoint 测试 ignored，所有 example targets passed。
- [x] `cargo test --doc`：1 个正向 doctest 与 9 个 compile-fail doctest passed。
- [x] `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`。
- [x] 已更新 README 与 crate 级文档，说明结构共享 raw history 和非事实来源的派生 index；
  `PLAN.md` 的阶段顺序、依赖与完成标准未改变，故不修改。
- [x] 按规模审计把 323 行 index 测试拆为 lifecycle/provider/cancellation 三个聚焦模块，
  并在拆分后重新跑完上述整条验证链。
- [x] 已在 `TODO.md` 标记 `M3-1 [DONE]` 并写完成记录。
- [x] 最终 `git diff --check` 与状态/diff 审计。
- [ ] 使用 `[M3-1] Implement shared history and derived tool-call index` 提交全部恢复现场改动，
  确认工作区后停止。
