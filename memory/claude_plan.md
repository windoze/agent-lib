# 当前任务计划

## 任务：M1-3 `Usage` 算术溢出改饱和/错误化（H-SEC-3、facade 报告 M2）——已完成

### 执行结果
- `src/model/usage.rs`：`merge` 与 `total_computed` 的全部 u32 加法改 `saturating_add`，删除 panic helper `checked_add`；rustdoc 注明饱和行为与理由（多报而非少报，预算安全方向）。
- `extra` 合并核实为 `Map::extend` 覆盖语义，无数值加法，无 panic 路径。
- 新增 4 条测试：merge 饱和、total_computed 饱和、Accumulator push 大计数、collect 大计数，全部通过。
- 全量门禁（fmt / clippy 默认+external features / test --all --all-targets / doc）全部通过。
- `docs/review-2026-07.md` H-SEC-3 标注 ✅ 已修复（M1-3）。
- TODO.md M1-3 标记 [DONE] + 完成记录。
- 无 breaking change。

### 下一步
- 提交 commit `[M1-3] ...`，然后停止（每个 invocation 只完成一个任务）。
