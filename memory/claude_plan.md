# 当前任务：M4-3 pivot 重发的 requirement id 与 trace 去重解耦（H-STATE-4）

## 任务来源
`TODO.md` 中第一个未完成任务是 **M4-3**（M4-1、M4-2 已 [DONE]）。

## 问题描述
- `src/agent/machine/default/mod.rs:614-623`：pivot 路径用同一 requirement id 重发 LLM 请求。
- `src/agent/drive.rs:445-494`：drain 把 requirement id 直接当 trace node id；`TraceHandle::record_node` 对重复 id 返回 `TraceError::DuplicateNodeId`（`src/agent/context/trace.rs:379-382`），经 `?` 中止整个 drain（`drive.rs:425-430`）。
- 广义问题：观测侧（trace）失败杀死实际驱动。

## 实现要求
- trace 记录改 best-effort：`record_node` 失败（含 DuplicateNodeId）记录 warning 类痕迹但不中止 drain；或 drain 对 pivot 重发生成派生 node id（如 `<id>#attempt-2`）。选型写入完成记录；effect-model 文档对 trace 完整性的承诺需同步修订。
- 检查 `NeverResumed` 等其他 trace 失败点同样不致命。

## 验证条件
- 单元测试：手写 driver 触发 pivot 重发同 id requirement + 开启 trace，drain 不再因 DuplicateNodeId 失败，turn 正常完成。
- `cargo test -p agent-lib --lib agent::` 全过。

## 执行计划
1. 阅读 `src/agent/drive.rs`（425-499）、`src/agent/context/trace.rs`（record_node / NeverResumed 路径）、`src/agent/machine/default/mod.rs`（pivot 重发处）。
2. 选型：倾向 "best-effort trace 记录"（trace 失败只记 warning，不中止 drain），因为这才是 "观测侧失败杀死实际驱动" 的根本修复；派生 id 只解决 DuplicateNodeId 一种情况。需要看代码后决定。
3. 实现改动。
4. 添加单元测试（手写 driver + trace 触发 pivot 重发）。
5. 同步修订 `docs/agent-effect-model.md` 中 trace 完整性承诺。
6. `cargo fmt` → `cargo clippy --all-targets -- -D warnings` → `cargo test --all --all-targets`（超时 ≤ 30min）→ 可能加 doc 检查。
7. 更新 `TODO.md`（标 [DONE] + 完成记录），提交 git，停止。

## 选型决定
采用**混合方案**（以派生 node id 为主，best-effort 兜底）：
- `record_requirement_node`（drive.rs）不再返回 `Result`；drain / drive_streamed 四处调用点全部非致命。
- `DuplicateNodeId`（pivot 同 id 重发）→ 派生 `<id>#attempt-N`（N 从 2 递增）重试，直至记录成功——trace 完整性保留，每次 settle 都有节点。
- 其余 `TraceError`（UnknownParent，结构性 bug）→ 丢弃该节点但不中止 drain（观测侧失败不杀驱动；crate 无 log facade，沿用 budget.rs:308 `let _ =` 的既有 best-effort 先例）。
- 文档同步：`docs/agent-effect-model.md` §8 + `docs/agent-effect-migration.md` §11。

## 进度日志
- [x] 读取 TODO.md，确认 M4-3 为第一个未完成任务。
- [x] 代码探索（drive.rs / stream.rs / trace.rs / 评审报告 H-STATE-4 / effect-model §8）
- [x] 实现（record_requirement_node 非致命化 + 派生 `<id>#attempt-N`；stream.rs 同步）
- [x] 测试（新增 drain_records_pivot_reemission_under_a_derived_trace_id；agent:: 445 全过；
  fmt/clippy 干净；cargo test --all --all-targets 全绿；cargo doc -D warnings 通过）
- [x] 文档（effect-model §8 + effect-migration §11）+ TODO.md 标 [DONE]
- [ ] 提交
