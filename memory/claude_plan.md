# 当前调用执行计划

> 本文件记录可审计的执行计划、关键判断依据、进度与验证结果；不记录模型的私有逐字推理。

## 目标与约束

- 以 `TODO.md` 为唯一任务顺序与验收来源，只处理标题中第一个未带 `[DONE]` 的任务。
- 在选择该任务后，仅检查与该任务直接相关的最新提交、实现、测试和文档；不进行开放式历史缺陷扫描。
- 若发现直接阻塞当前任务的既有缺陷或未建模前置条件，按要求在 `TODO.md` 中插入最少的前置任务、保持当前任务未完成、提交任务表调整后停止。
- 若无阻塞，则完整实现、测试、更新完成记录、提交；不继续下一个任务。
- 尊重现有未提交改动，先辨认其归属，不覆盖或丢弃用户改动；若这是上次中断留下的同一任务工作，最终提交需包含所有未提交文件。

## 分步计划

1. 读取 `TODO.md`，从上到下识别首个标题未带 `[DONE]` 的任务，完整摘取其需求、依赖、验收命令和完成记录格式。
2. 查看工作树状态和最新提交摘要；只判断未提交内容及最新提交是否与当前任务直接相关，并据此确认是全新执行、恢复执行，还是存在必须先处理的前置问题。
3. 阅读 `PLAN.md` 中与该任务所属阶段直接相关的部分，以及任务点名的代码、测试和文档；建立需求到实现与测试的对应关系。
4. 在不缩窄规范、不引入临时兼容层的前提下，按小而聚焦的补丁实现任务；每完成关键实现或计划发生变化，立即更新本文件。
5. 增补或调整覆盖正常路径、边界条件、错误分类、原子性/不变量（若任务涉及）的测试；先运行聚焦测试以快速定位问题。
6. 按规定顺序验证：`cargo fmt --all`，然后 `cargo clippy --all-targets -- -D warnings`，最后在不超过 30 分钟的超时约束下运行 `cargo test --all --all-targets`；再执行任务要求的其他验证（例如文档构建）。任何未被后续任务明确安排的失败都必须在本次修复或转化为排在当前任务前的最小前置任务。
7. 验证通过后，在 `TODO.md` 的任务标题前添加 `[DONE]`，填写准确的实现与验证记录。仅当阶段级顺序、依赖、假设或完成标准确实变化时才更新 `PLAN.md`。
8. 复查差异、任务范围、格式与工作树，确认没有秘密、构建产物或无关改动；用包含任务编号的清晰信息创建一次 Git 提交。若属于恢复执行，按要求将当前所有未提交文件纳入同一提交。
9. 记录最终提交哈希与完成状态，然后停止，不读取或执行下一个任务。

## 当前进度

- [x] 在任何检查、构建或代码命令前创建本计划文件。
- [x] 识别首个未完成任务：`M3-2 受检 Boundary、version 与 stale/ABA 防护`。
- [x] 确认工作树与直接相关上下文：当前为 `M3-2` 中断恢复，已有同任务未提交改动。
- [x] 完成实现与聚焦测试。
- [x] 完成格式化、严格 lint、完整测试及任务专属验证。
- [x] 更新 `TODO.md` 完成记录并标记 `M3-2 [DONE]`。
- [x] 完成最终差异审计；本文件将随全部 `M3-2` 恢复现场纳入任务提交，然后停止。

## 计划变更与关键结果

- `TODO.md` 中从上到下首个未带 `[DONE]` 的标题位于第 599 行：`M3-2 [TODO]`。
- 当前任务要求新增私有字段的 `Boundary` token、合法边界枚举与按 Turn 查询、统一 owner /
  structural version / anchor / range / pending 校验，以及稳定分类的 stale、ABA、跨会话、未知
  Turn、fork ceiling 和伪造 serde token 负例。
- `M3-3` 才负责真正的 head/revert/redo 操作，因此本次只建立边界模型与统一校验基础；测试所需
  的 future suffix / fork ceiling 应在当前 history 内部能力上验证，不能提前公开或实现下一
  任务的完整状态转换。
- 最新提交 `72506520dfbc5998b06087cb61533ea79921afea` 是已完成的 `M3-1`，提交说明没有
  M3-2 直接相关的未完成问题。
- 工作树已有 `README.md`、`docs/conversation-core.md`、Conversation error/history/module、
  crate root 及新 `boundary` 模块的未提交修改，形状与 M3-2 完全一致；`TODO.md` 尚未修改。
  因此按“恢复同一任务”处理：逐项审计、修正、验证，并在最终提交中包含当前全部未提交文件。
- 已对照 `PLAN.md` 与 `docs/conversation-core.md` §9 审计现有实现：`Boundary` 私有字段绑定
  owner、`turn_count`、稳定 `after_turn` anchor 与 version；公开签发 API 覆盖 zero、每个
  addressable Turn、revert 后 redo suffix；统一 resolver 按 owner/version/pending/range/
  fork ceiling/anchor 校验，serde 仅恢复声明。
- `History` 将 active head、addressable lineage ceiling 与共享 backing allocation 分离，既能让
  root 的 future suffix 继续可寻址，也能让测试中的 fork child 精确拒绝 ceiling 以上位置；
  真正的公开 head/revert/fork 转换仍留在 M3-3/M3-4，没有扩大本任务范围。
- 现有正负测试已经覆盖任务列出的 empty/multi/zero/head/future redo、cross-owner、stale/ABA、
  pending、unknown/detached、fork ceiling、伪造 range/anchor 与 serde round-trip，并对所有拒绝
  路径快照比较 state 不变。现在按规定进入 format → clippy → 聚焦 → 全量 → rustdoc 验证链。
- 正式验证全部通过：`cargo fmt --all`；`cargo clippy --all-targets -- -D warnings`；Boundary
  聚焦测试 12 passed；1800 秒硬上限内全量测试为 232 个库测试与 3 个离线集成测试 passed、
  7 ignored、0 failed，全部 example targets passed；doctest 为 1 个正向与 10 个
  compile-fail passed；`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`；首次
  `git diff --check`。
- 已在 `TODO.md` 把标题更新为 `M3-2 [DONE]` 并填写实现、错误边界、测试矩阵与验证记录；
  `PLAN.md` 的阶段顺序、依赖、假设和完成标准没有变化，按规则未修改。完成记录和本进度文件
  是验证后的纯 Markdown 变更，无需重复运行编译测试；最终仍会重跑 diff check 并审计暂存内容。
- 最终暂存审计包含 13 个文件，均属于 Boundary 实现、测试、相关 history/error 接线、规范/
  README、TODO 与恢复进度；没有无关文件或构建产物，`git diff --cached --check` 通过。
