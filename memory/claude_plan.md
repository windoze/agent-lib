# 执行计划

## 当前任务：M3-5-2 `into_snapshot` 重组：按最大代次选取演进行

任务来源：`TODO.md` M3-5-2（M3-5-1 已完成：三类行已带 `generation` 字段，schema v2，
`into_snapshot` 目前用 `validate_generations()` 要求单代次一致——本任务把它改为按最大代次选取）。

### 任务要求（来自 TODO.md）

1. `ConversationRowInsertSet::into_snapshot` 重组规则改为：
   - conversation 行：取 `generation` 最大者；行集必须恰好含该 conversation 至少一行，
     多行时代次必须可确定唯一最大值。
   - lineage 关联/projection span：只取 `generation == conversation 行最大代次` 的行，
     按 `lineage_sequence`/`span_sequence` 排序重组；低代次行忽略（历史版本）。
   - 校验：选中代次的 lineage/span 序列必须稠密从 0 开始（复用/扩展现有
     `validate_row_owners` 类校验），owner 校验不变。
2. 顺带评估放宽 `insert_set_against` 对 existing 的限制（审查 L-3：existing 必须恰好是
   单一 conversation 完整行集）：重组改为按 owner 过滤后，existing 可以是多 conversation
   行集的子集查询结果。若改动超范围，记录为后续项。

### 验证条件

- 单元测试：同一 conversation 两个代次的行混合的行集，`into_snapshot` 选取最大代次
  重组出正确 snapshot。
- 单元测试：代次稀疏/缺行（如只有 gen 1,3 无 2 的当前代次行）报明确 `InvalidRow`。

### 执行步骤

1. 读 `src/conversation/persistence/rows.rs` 现状：`into_snapshot`、`validate_generations`、
   `validate_row_owners`、相关测试（M3-5-1 的 `rows_reject_inconsistent_generations` 需要
   改写——混合代次现在合法，只有"选中代次缺行/稀疏"或"conversation 代次不自洽"才报错）。
2. 设计并实现：
   - conversation 行选取最大 generation（多行同代次内容不同 → 明确 `InvalidRow`；
     注意 `ConversationRowInsertSet` 内存形状此前只持单个 conversation 行——M3-5-1 完成记录
     提到这一点，需要看结构体字段是否为 Vec 还是单值，可能涉及形状调整或现有形状已支持多行）。
   - lineage/span 按 `generation == max_gen` 过滤，按 sequence 排序，校验稠密从 0 开始。
   - 低代次行忽略。
   - 更新 `validate_generations`（改名或改写为"选取+校验"）。
3. 评估 L-3 放宽：视 `insert_set_against`/`diff_single_conversation` 的 owner 过滤现状决定
   本任务内做还是记录为后续项（倾向：若 diff 已按 owner 过滤则低成本纳入，否则记录）。
4. 测试：新增两条验证条件测试 + 更新 M3-5-1 的 `rows_reject_inconsistent_generations`
   （混合代次不再一律报错，改为"选中代次自洽但低代次存在 → 取最大代次"；真正非法形状
   ——选中代次稀疏/conversation 行缺失——仍报错）。
5. 门禁：`cargo fmt --all` → `cargo clippy --all-targets -- -D warnings` →
   external features clippy → `cargo test -p agent-lib --lib conversation::persistence` →
   `cargo test --all --all-targets` → `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`。
6. 更新 `TODO.md`（M3-5-2 标 [DONE] + 完成记录）；M-CONV-3 在 review 文档的标注按
   TODO 口径留给 M3-9（M3-5-1~4 全部落地后）。
7. 提交：`[M3-5-2] ...`，含本计划文件更新。

### 关键设计结论（代码探索后修正）

1. **落点**：任务文本写 `ConversationRowInsertSet::into_snapshot`，但现有 `into_snapshot`
   在 `ConversationRows` 上（单 conversation 行形状，无法表达多代次）。`ConversationRowInsertSet`
   全是 `Vec` 字段且目前**没有** `into_snapshot`。方案：在 `ConversationRowInsertSet` 上新增
   `into_snapshot`（多代次选取），委托给现有 `ConversationRows::into_snapshot`（严格单代次路径
   保持不变）。另加 `From<ConversationRows>` 与 `merge(&mut self, other)` 便于合并导出。
2. **同类问题发现（class-wide fix）**：`ArtifactRecord` 的成员资格同样随代次演进——
   `retained_current_artifacts`（compaction.rs:542）在 revert+recompact 后会丢弃不再匹配
   active head 的 artifact，且 artifact_sequence 会重排。无 generation 列时，合并行集的
   artifact 稠密校验会误报（两个代次各有 seq 0）、sequence 重排会误判 InsertConflict。
   因此给 `ArtifactRecord` 也加 `generation` 列（与三类行同构），`CONVERSATION_ROW_SCHEMA_VERSION`
   升 2 → 3，`insert_set_against` 的 artifact diff key 同步改为 `cid#gen#artifact_id`
   （不改会引入回归： retained artifact 跨代次导出必冲突）。TODO.md 的 M3-5 决策与 M3-5-3
   任务文本同步更新。
3. **重组规则**：conversation 行取最大 generation（同 id+gen 内容冲突 → DuplicatePrimaryKey）；
   lineage/span/artifact 过滤到最大代次，低代次忽略，**高于**最大代次 → InvalidRow（存贮读取不全）；
   全表先按主键去重（相同行折叠、冲突行 DuplicatePrimaryKey）；raw 非空但选中代次
   lineage/span 为空 → 明确 InvalidRow；稠密性/FK/owner 校验复用委托路径。

### 进度记录

- [x] 读取 TODO.md，确定当前任务为 M3-5-2
- [x] 阅读 rows.rs 现状代码，确定设计（含 ArtifactRecord 代次扩展）
- [x] 实现 rows.rs 改动（insert set into_snapshot + dedup/select helpers + ArtifactRecord generation + schema v3 + artifact diff key）
- [x] 新增/更新测试（4 新增 + 2 扩展，persistence 30 条全过）
- [x] 全量门禁（fmt/clippy/external clippy/test --all/doc 全过）
- [x] 更新 TODO.md（M3-5-2 标 DONE + M3-5 决策修正 + M3-5-3 文本同步）
- [x] 提交（7938f25）

## 任务完成总结

M3-5-2 已完成并标 `[DONE]`。核心交付：
1. `ConversationRowInsertSet::into_snapshot`——多代次行集按最大代次重组（选取/过滤/去重后委托严格单代次路径）。
2. 同类修复：`ArtifactRecord` 增加 `generation` 列（row schema v3），修正审查时「artifact 是 append-only 事实」的误判——revert+recompact 会丢弃/重排 artifact 成员。
3. 4 条新测试 + 2 条扩展，全量门禁（fmt/clippy/external-clippy/test/doc）全过。
4. L-3 评估记录为 M3-5-3 连带项。下一任务：M3-5-3（diff 代次键 + 演进场景测试）。
