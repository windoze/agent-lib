# 当前执行计划

## 目标与边界

- 本次调用只处理 `TODO.md` 中按顺序出现的第一个标题未带 `[DONE]` 的任务；完成并提交后立即停止。
- `TODO.md` 是任务顺序、需求、依赖、验证标准和完成记录的唯一权威来源；只有阶段级计划确实变化时才更新 `PLAN.md`。
- 不做开放式历史缺陷巡检。只检查最新提交是否明确提到与当前任务直接相关的未完成问题，以及实现/验证当前任务所必需的代码路径。
- 若发现阻塞当前任务的真实前置缺陷，优先完整修复；若本次无法正确落地，则以最少数量在 `TODO.md` 中插入前置任务、明确依赖，提交任务编排变更后停止。
- 不通过缩小表示范围、特例、shim 或调整测试形状绕过规范问题。

## 分步执行

1. 读取 `TODO.md`，仅根据任务标题是否有 `[DONE]` 识别第一个未完成任务，完整摘录其需求、依赖、验收命令和完成记录要求。
2. 检查工作树状态与最近一次提交说明：
   - 保留所有既有用户改动，不回退、不覆盖无关内容；
   - 判断是否属于上次中断后遗留的同一任务；若是，本次完成时将全部未提交文件纳入同一个任务提交；
   - 只把最新提交明确指出且直接影响当前任务的未完成问题纳入范围。
3. 阅读当前任务直接涉及的设计、实现和测试文件，建立需求到代码/测试的对应关系；不进行无关的广泛排查。
4. 若任务规模本身较大，仍按既有任务单元完整实施；只有出现无法绕开的、未跟踪的具体前置条件时才调整 `TODO.md` 任务序列。
5. 采用多个小而聚焦的补丁实施：每个关键修改后重读相关片段，补齐同一根因影响的整类情况，并增加或更新针对性测试与必要文档。
6. 先运行最小范围测试以快速验证行为。任何失败都按测试失败政策处理：修复，或确认已有明确后续任务；不得把未安排的失败当作噪声。
7. 代码稳定后依次执行最终验证：
   - `cargo fmt --all`
   - `cargo clippy --all-targets -- -D warnings`
   - `cargo test --all --all-targets`（最长 30 分钟）
   - 若任务要求或改动影响公开文档，再执行 `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`
   若本次只有文档变化且可复用最近一次绿色全量结果，则按规则跳过全量测试并在完成记录中说明。
8. 验证通过后更新 `TODO.md`：给当前任务标题加 `[DONE]`，写清实现、测试命令及结果、关键取舍和提交信息占位。仅当阶段依赖/完成标准变化时更新 `PLAN.md`。
9. 再检查 diff、任务边界、格式和工作树，确保没有秘密、生成物、无关修改或遗漏文件；若 `PROMPT.md` 意外变化，按用户要求一并纳入提交且不擅自回退。
10. 使用清晰且包含任务编号的消息创建 Git 提交。提交后核对 `git status` 与提交摘要，并把最终提交哈希补入 `TODO.md` 的完成记录；如果补哈希会产生新的未提交变更，则采用不会制造自指未提交状态的记录方式（例如记录提交命令/消息，最终答复提供实际哈希）。
11. 向用户报告当前任务、主要实现、验证结果和提交哈希，然后停止，不开始下一个任务。

## 进度日志

- 已建立本计划。
- 已读取 `TODO.md` 并确认首个未完成任务为 **M3-3：逻辑 `head`、revert/redo 与 revert 后分支**；前置 M3-2 已标记完成。
- 本次必须实现：受检 `revert_to(Boundary)`、old/new head outcome、结构 version 推进、有效工具索引随 head 重建、同 active lineage redo、revert 后提交形成新 parent suffix、detached raw suffix 保留且不泄漏，以及 raw/current-lineage 只读查询。
- 指定验证重点：多次 revert/redo、zero、revert 后分叉提交、再次 revert、tool index clipping、旧 boundary stale、新 boundary 可用、raw id/payload 不变、detached branch 隔离和两条 parent path 可重建。
- 已检查工作树：存在 `boundary/head.rs`、`boundary/tests/revert/` 及 history/error/docs 等同一 M3-3 范围的未提交改动，判定为上次意外中断后的恢复现场；完成时必须把当前全部未提交文件原子纳入本任务提交。
- 最新提交 `0f41f647` 是 `[M3-2] Implement checked boundary tokens`，没有在提交说明中明确提出与 M3-3 相关的额外未完成前置问题。
- 已审阅现有 diff、规范 §7--§9、commit/identity/index 路径：实现以 logical `active_len` 裁剪有效视图，保留 addressable lineage redo suffix 和 append-only raw branch；真实移动在写状态前完成 Boundary 校验、version 溢出检查与 index 重建，revert 后 commit 以当前 `tip_id` 作为新 parent，raw identity 检查覆盖 detached branch。
- 验证已通过：`cargo fmt --all`；`cargo clippy --all-targets -- -D warnings`；`cargo test conversation::boundary -- --nocapture`（19 passed）；`timeout 1800 cargo test --all --all-targets`（239 个库测试与 3 个离线集成测试 passed，7 ignored，examples passed）。
- 后续验证也已通过：`cargo test --doc`（1 个正向与 10 个 compile-fail passed）、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`、`git diff --check`。
- 已将 `TODO.md` 的 M3-3 标题更新为 `[DONE]`，并记录实现、错误原子性、分支/raw/index 行为与完整验证结果；阶段顺序和完成标准未变化，未修改 `PLAN.md`。
- 最终暂存审计包含 17 个文件：head/revert 实现、7 个聚焦回归及 fixture 调整、history/error/API 接线、README/crate docs、TODO 和本进度文件；均属于 M3-3 恢复现场，没有构建产物、凭据、`PLAN.md` 或 `PROMPT.md` 变化，`git diff --cached --check` 通过。
- 下一步重新暂存本条进度更新，创建一个 `[M3-3]` 提交，核对提交后工作树并停止。

## 风险与处理原则

- 若工作树已有改动与当前任务重叠，先辨认归属并在原基础上安全推进；无法避免覆盖风险时停止并说明。
- 若任何单测超过一分钟或疑似卡死，立即终止并调查，不等待其自然超时。
- 若完整测试发现未在后续任务明确安排的失败，不得把当前任务标为 `[DONE]`，必须先修复或安排最小前置/后续任务。
- 最终只提交当前任务所需内容；若这是一次同任务恢复，则遵循要求提交当前全部未提交文件。
