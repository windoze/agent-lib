# 执行计划

## 当前任务：M3-5-3 `insert_set_against` 代次键 diff + 演进场景测试

任务来源：`TODO.md` M3-5-3。前置状态：M3-5-1（三类行 generation + schema v2）、
M3-5-2（insert set into_snapshot 最大代次选取 + ArtifactRecord generation + schema v3，
artifact diff key 已落地为 `cid#gen#artifact_id`）均已完成。

### 任务要求（来自 TODO.md）

1. `diff_single_conversation`：key 改为 `(conversation_id, generation)`——同 conversation
   不同代次不再冲突，作为新行插入；同代次内容不同仍 `InsertConflict`。
2. `diff_rows` 的 lineage/span key 闭包加入 generation：
   `conversation_id#generation#lineage_sequence` / `conversation_id#generation#span_sequence`。
3. artifact diff key 已在 M3-5-2 落地，本任务确认行为即可。
4. L-3（放宽 `insert_set_against` 的 existing 为多 conversation 子集查询结果）
   与本任务 diff 改动同源，实施时一并评估；若超范围记录为后续项。
5. 事实表 diff key 不变。

### 验证条件（每条一个测试）

- commit 演进：导出 gen N → 再 commit → 导出 gen N+1 → `insert_set_against` 成功，
  insert set 只含新 conversation 行 + 新 lineage 行 + 新 turn/message 事实行。
- revert 演进：导出 → revert → 导出 → 不冲突，新 lineage 行以新代次共存。
- compaction 演进：导出 → apply_compaction → 导出 → 不冲突，新 span 行以新代次共存。
- 同代次篡改：手工改同 generation 行的内容 → 仍 `InsertConflict`。
- round-trip：两次导出的行集合并后 `into_snapshot` 得到最新状态（与 M3-5-2 联动）。
- `cargo test -p agent-lib --lib conversation::persistence` 全过。

### 执行步骤

1. 读 `src/conversation/persistence/rows.rs` 现状：`insert_set_against`、
   `diff_single_conversation`、`diff_rows` 及 key 闭包、现有 diff 测试。
2. 修改 diff key：
   - conversation 行 key：`conversation_id` → `(conversation_id, generation)`。
   - lineage/span key 闭包加 generation 段。
   - 事实表 key 不动。
3. 评估 L-3：`insert_set_against(existing: ConversationRows, ...)` 的 existing 形状
   是否需要放宽；若签名改动超范围，记录为后续项。
4. 新增 5 条验证条件测试（commit/revert/compaction 演进、同代次篡改、合并 round-trip）。
5. 门禁：`cargo fmt --all` → `cargo clippy --all-targets -- -D warnings` →
   external features clippy → `cargo test -p agent-lib --lib conversation::persistence` →
   `cargo test --all --all-targets` → `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`。
6. 更新 `TODO.md`（M3-5-3 标 [DONE] + 完成记录）。
7. 提交：`[M3-5-3] ...`，含本计划文件更新。

### 进度记录

- [x] 读取 TODO.md，确定当前任务为 M3-5-3
- [x] 阅读 rows.rs diff 代码现状
- [x] 实现 diff key 代次化（conversation key 加 generation；lineage/span key 闭包加 generation；artifact 已在 M3-5-2 就位；`insert_set_against` rustdoc 补代次演进说明）
- [x] 新增演进场景测试（5 条：commit/revert/compaction 演进、同代次篡改+分叉克隆冲突、insert-set 合并 round-trip；persistence 35 条全过）
  - 测试中发现：无 compaction 的导出也有 projection span 行（raw span 集合），断言已按此修正；`Conversation` 不实现 Clone，分叉场景改用确定性构造函数 `conversation(seed)` 双实例重放
- [x] L-3 评估结论记录（放宽 existing 形状 = 签名级 breaking，列入 M9-2 后续项）
- [x] 全量门禁（fmt/clippy/external clippy/test --all/doc 全过）
- [x] 更新 TODO.md（M3-5-3 标 DONE + M9-2 增 L-3 后续项）
- [ ] 提交

## 任务完成总结

M3-5-3 已完成并标 `[DONE]`。核心交付：
1. `diff_single_conversation` 主键改 `(conversation_id, generation)`；lineage/span diff key 加 generation 段，与 M3-5-2 的 artifact key 同构——同 conversation 演进后重导出从必然 `InsertConflict` 变为合法 insert-only。
2. 5 条演进场景测试（commit/revert/compaction 演进、同代次篡改+分叉克隆冲突、insert-set 合并 round-trip），persistence 35 条全过。
3. L-3（放宽 `insert_set_against` existing 形状）评估为签名级 breaking，已列入 M9-2。
4. 全量门禁（fmt/clippy/external-clippy/test/doc）全过。
下一任务：M3-5-4（rows 代次模型文档同步，纯文档任务）。
