# M5-3 执行计划：Review 完整逃生出口

## 任务性质
Review 任务。复核 M5-1（扩展 `AgentParts` 覆盖 external/协作/交互状态）与
M5-2（into_parts/snapshot/builder 文档对齐）的落地是否正确、一致、无回归。
非纯文档任务：若发现代码/文档缺口须修复并可能重跑测试。

## 检查范围（来自 TODO M5-3）
1. `AgentParts` 是否覆盖当前 `Agent` 中所有有语义的字段。
2. 是否有 public API 泄漏了不该稳定承诺的内部实现细节。
3. `into_parts`、snapshot、builder 的用途边界是否清楚。
4. M3 协作 snapshot 修复与本阶段 `into_parts` 扩展是否互相一致。

## 验证命令（TODO M5-3）
- cargo fmt --all
- cargo clippy --all-targets -- -D warnings
- cargo test -p agent-lib --lib facade::agent::
- RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
- 手工复核 docs/refine.md "Agent::into_parts 状态覆盖不完整" 条目状态。

## 步骤
- [ ] 读 `src/facade/agent.rs` 的 `Agent` 结构体字段全集，逐字段核对是否被 into_parts 交出或有意省略。
- [ ] 读 `src/facade/agent/snapshot.rs` 的 `AgentParts` + into_parts 实现 + Debug。
- [ ] 核对 public 泄漏（RetainedExternalSession 等 data-only 保证）。
- [ ] 核对 into_parts vs snapshot/restore 语义一致（M3 协作）。
- [ ] 复核 docs/refine.md §6 状态。
- [ ] 运行验证命令。
- [ ] 若无代码改动仅文档：可复用上次全绿测试。
- [ ] 标记 M5-3 [DONE] + 完成记录；提交。

## 进度
（待填）

## 进度（M5-3 完成）
- [x] 核对 Agent 13 字段全部被 into_parts 交出（无静默 drop）。
- [x] 核对 public 无泄漏：CollabState(pub(crate)) 未泄漏；RetainedExternalSession pub data-only；clippy -D warnings 佐证无 private-in-public。
- [x] 核对 into_parts/snapshot/builder 用途边界文档一致（rustdoc + facade-api.md §8.2）。
- [x] 核对 M3 协作 snapshot(data-only) 与 into_parts(live 句柄) 互补一致，测试双侧全绿。
- [x] 复核 docs/refine.md §6：状态行更新为「M5-3 复核通过」，修复结果块与实现一致。
- [x] 验证全绿：fmt(无源码改)/clippy(clean)/facade::agent(49 passed)/doc(clean)。
- [x] 仅文档改动，复用 M5-1 全量测试绿结果。
- [x] TODO.md 标记 M5-3 [DONE] + 完成记录。
