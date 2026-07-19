# 执行计划：M3-3 修复 restore 派生索引的空校验（M-CONV-1）

## 任务定位

- TODO.md 首个未完成任务：**M3-3**（M3-1、M3-2 已 DONE，M3-3 行 654 起）。
- 问题：`src/conversation/persistence/snapshot.rs:566-575` 在 restore 时对同一纯函数
  `ToolCallIndex::rebuild(turns, None)` 调两次再比较相等，`RestoreError::DerivedIndexMismatch`
  结构性不可达 —— 空校验，无检测价值。

## 选型（任务推荐 (a)）

采用方案 (a)：**删除该校验与不可达错误变体**，restore 直接 `rebuild`。
理由：纯函数重建对同一输入必得同一输出，比较无校验价值；若选 (b)（增量 vs 全量比较）
需要论证增量路径是生产实际使用的路径，而 restore 本身就是全量重建路径，无增量对照意义。

## 执行步骤

1. 阅读 `src/conversation/persistence/snapshot.rs`（重点 566-575 及 `RestoreError` 定义/使用点）。
2. 检查 `RestoreError` 是否 `#[non_exhaustive]`，确认删除变体的 breaking 面并记录。
3. grep `DerivedIndexMismatch` 全部使用点（构造、match、测试、文档）。
4. 删除空校验代码，restore 直接 `rebuild`；删除错误变体并同步文档注释。
5. 运行验证：
   - `cargo test -p agent-lib --lib conversation::persistence`
   - `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`
   - `cargo clippy --all-targets --features "external-claude-code external-codex external-opencode external-acp" -- -D warnings`
   - `cargo test --all --all-targets`
   - `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`
6. 更新 `docs/review-2026-07.md` M-CONV-1 条目标注 `✅ 已修复（M3-3）`。
7. TODO.md：M3-3 标题加 `[DONE]`，写完成记录（含 breaking change 说明）。
8. 提交 git commit：`[M3-3] ...`。

## 进度

- [x] 读取 TODO.md 定位任务（M3-3）
- [x] 阅读 snapshot.rs 相关代码，确认空校验与不可达变体
- [x] 实施修复：删 `rebuild_tool_call_index` 包装 + `DerivedIndexMismatch` 变体，restore 直接 `rebuild`，文档同步
- [x] 验证全过：fmt、clippy（默认 + external features）、conversation::persistence 19 条、全量测试 exit 0、cargo doc
- [x] `docs/review-2026-07.md` M-CONV-1 标注 ✅ 已修复（M3-3）；TODO.md M3-3 标 [DONE] + 完成记录（含 breaking change 说明）
- [ ] 提交
