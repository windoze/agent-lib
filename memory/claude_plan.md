# 执行计划：M3-1 禁止在 reverted head 上 compaction（H-STATE-1）

## 任务识别
- `TODO.md` 第一个未完成任务（无 `[DONE]`）：**M3-1**（M1/M2 全部已完成）。
- 工作树干净，最近提交为 M2-8，与本任务无冲突前置。

## 问题理解
- `src/conversation/projection/compaction.rs` 的 `apply_compaction` / `validate_compaction_plan_header`
  只校验 `plan.head_turn_count == active_len`，不校验 head 是否处于 lineage 末尾。
- 破坏路径：revert_to(3) → compact(head=3) → 投影只覆盖 0..3 → redo revert_to(5) 成功 →
  `effective_view()` 静默丢 turn 3..5，且无法自愈（IncompleteProjection / SpanGap）。

## 执行步骤
1. 阅读 `src/conversation/projection/compaction.rs`（validate/apply/测试）与 `CompactionError` 定义，
   弄清 lineage_len 在调用点如何取得。
2. 在 `validate_compaction_plan_header`（或 `apply_compaction` 入口）增加校验：
   `active_len == lineage_len` 才允许 compaction；否则返回明确错误
   （新增或复用合适的 `CompactionError` 变体），错误消息说明
   "reverted head 上不可 compaction，先 redo 到 lineage 末尾"。
3. 回归测试（精确复现报告路径）：5 turn + compact 0..5 → revert_to(3) →
   apply_compaction 返回新错误而非成功；redo revert_to(5) 后 `effective_view()` 仍含全部 5 turn。
4. `docs/conversation-core.md` compaction 节补充该约束。
5. `docs/review-2026-07.md` H-STATE-1 标注 `✅ 已修复（M3-1）`。
6. 验证（按序）：`cargo fmt --all` → `cargo clippy --all-targets -- -D warnings` →
   `cargo test -p agent-lib --lib conversation::projection` → `cargo test --all --all-targets`
   → `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`。
7. `TODO.md` M3-1 标 `[DONE]` + 完成记录；git 提交 `[M3-1] ...`；停止。

## 进度
- [x] 读取 TODO.md，识别任务 M3-1
- [x] 阅读 compaction 相关代码
- [x] 实现校验 + 错误变体（`ProjectionError::CompactionOnRevertedHead { head, lineage_len }`，
      在 `validate_compaction_plan_header` 的 head 校验之后检查 `active_len == lineage_len`）
- [x] 回归测试 `apply_compaction_rejects_a_reverted_head_and_redo_keeps_every_turn`（26 条 projection 测试全过）
- [x] 文档同步（conversation-core.md §6.1、review-2026-07.md H-STATE-1 ✅）
- [x] 全量验证（fmt/clippy/test --all/doc/external-features clippy 全部通过）
- [x] TODO.md 完成记录 + 提交
