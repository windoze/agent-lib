# 执行计划

## 当前任务：M3-5-4 rows 代次模型文档同步（纯文档任务）

任务来源：`TODO.md` M3-5-4。前置状态：M3-5-1（schema v3：三类演进行 + artifact 的
generation 列）、M3-5-2（insert set 最大代次选取重组 + merge）、M3-5-3（diff key 代次化）
均已完成。

### 结构修复（前置）

M3-5-4 的标题行 `#### M3-5-4 [TODO] rows 代次模型文档同步` 在 M3-5-3 提交（0dd60f9）中
被误删，其「实现要求/验证条件」正文成为孤儿段落（TODO.md:810-818）。经 git 历史核对
（7938f25 中标题完好、正文未变），先恢复标题行再执行任务。

### 任务要求（来自 TODO.md）

1. `rows.rs` 模块文档与 `ConversationRowInsertSet` 文档改写为代次模型描述：
   事实表 insert-only；演进表按代次版本化；"当前状态 = 最大代次"；
   `structural_version` 即代次。
2. `docs/conversation-core.md` 持久化节同步；DESIGN.md §10 如有相关描述一并核对。
3. 文档中给出演进时序示例：commit → gen 1 行集；revert → gen 2 行集；
   查询当前状态取 gen 2。

### 验证条件

- 文档与 M3-5-1~3 实现一致（以代码为准核对措辞）。
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` 通过。
- 纯文档/rustdoc 注释改动，不影响编译输出 → 按既定政策跳过全量测试套件
  （沿用 M3-5-3 的绿色结果），但 rustdoc 改动必须跑 cargo doc 与 fmt/clippy 快检。

### 执行步骤

1. 恢复 TODO.md 中 M3-5-4 标题行（结构修复）。
2. 读 `src/conversation/persistence/rows.rs` 模块文档与 `ConversationRowInsertSet` 现状。
3. 读 `docs/conversation-core.md` 持久化节与 `DESIGN.md` §10。
4. 改写 rows.rs 两处 rustdoc 为代次模型（含演进时序示例）。
5. 同步 `docs/conversation-core.md`；DESIGN.md §10 按需核对。
6. 门禁：fmt → clippy（默认 + external features）→ cargo doc（-D warnings）。
7. 更新 TODO.md（M3-5-4 标 [DONE] + 完成记录）。
8. 提交：`[M3-5-4] ...`，含本计划文件。

### 进度记录

- [x] 读取 TODO.md，发现 M3-5-4 标题行在 M3-5-3 提交中被误删（正文孤儿化）
- [x] git 历史核对：7938f25 中标题与正文完好，0dd60f9 误删标题行
- [x] 恢复 TODO.md 中 M3-5-4 标题行（结构修复）
- [x] rows.rs 模块文档新增「Generation model (insert-only evolution)」节（两类行、structural_version 即代次、当前状态 = 最大代次、时序示例）
- [x] `ConversationRowInsertSet` rustdoc 补代次键口径
- [x] `docs/conversation-core.md` §10 新增代次模型条目；DESIGN.md 核对（无 §10、无 rows 描述，无需更新）
- [x] 门禁：fmt / clippy（默认 + external features）/ cargo doc（-D warnings）全过；全量测试套件按政策跳过（纯文档/rustdoc 注释变更）
- [x] 更新 TODO.md（M3-5-4 标 [DONE] + 完成记录）

## 任务完成总结

M3-5-4 已完成并标 `[DONE]`。核心交付：
1. 结构修复：恢复被 M3-5-3 提交误删的 M3-5-4 标题行。
2. rows.rs 模块文档 + `ConversationRowInsertSet` rustdoc 改写为代次模型描述（含演进时序示例）。
3. `docs/conversation-core.md` §10 同步代次模型条目；DESIGN.md 无需更新。
4. 门禁（fmt/clippy×2/doc）全过；测试套件按纯文档政策跳过。
至此 M3-5-1~4 全部落地，M-CONV-3 的标注留待 M3-9 review。下一任务：M3-6（finish_assistant 前置块级校验）。
