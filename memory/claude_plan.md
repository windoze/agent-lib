# 执行计划

## 当前状态

- 已读取 `TODO.md` 并确认第一个未完成任务为 `M8-2 [TODO] 三个 CLI adapter 收敛共享 child-process 模块（external 报告 L-12）`。
- 最新提交 `5d2edcb [M8-1] Consolidate LLM adapter transport helpers` 是上一项收敛任务的完成提交，未发现直接声明 `M8-2` 的未完成阻塞。
- 当前任务 `M8-2` 已实现并在 `TODO.md` 标记为 `[DONE]`。
- 已新增 `src/agent/external/process/`，并将原进程组 kill 原语移动到 `process/group.rs`；三个 CLI adapter 已迁移到共享 `ManagedChild`，ACP connection/adapter 已复用共享读行、close、capability 和 observation helper。
- 已在 `docs/review-2026-07.md` 的复制代码条目中标注 `✅ 已修复（M8-2）`。

## 步骤计划

1. 读取三个 CLI adapter 与 ACP connection 的相关进程、prelude、drain、capability helper 代码，记录可安全抽取的公共边界。
2. 新增 `src/agent/external/process/`，优先抽取行为完全同构且不改变 public API 的部分：line-oriented child process wrapper、close/kill 分类、prelude deadline/cancel helper、capability/tool-message 等纯函数。
3. 将 Claude Code / Codex / OpenCode adapter 改为调用共享模块，只保留各自 argv 构造、decoder 接线和 runtime-specific 分支；ACP 可复用的进程管理部分同步接入。
4. 补齐或调整既有测试，保持外部 feature 测试断言行为不变；记录三 adapter 行数 before/after。
5. 运行 `cargo fmt --all`，再运行 external feature clippy，随后运行 external feature 测试；若发现未排期失败测试，按测试失败政策处理。
6. 更新 `TODO.md`：将 `M8-2` 标题改为 `[DONE]`，填写完成记录、验证结果和行数变化；`PLAN.md` 仅在阶段级计划变化时更新。
7. 检查 `git status`、`git diff`、最近提交，提交本次任务改动并停止。

## 进度日志

- 初始化计划文件已创建。
- 已定位当前任务为 `M8-2`。
- 已读取 M8 相邻任务与审查报告中 L-12 描述，确认本次范围是复制代码收敛而非新行为变更。
- 已完成第一轮代码迁移：`ManagedChild` 统一 spawn/read/close，`PreludeDeadline` 统一 prelude deadline/cancel 检查，`intersect_capabilities`/`reject_unsupported_tools`/`autonomous_turn_message`/`emit_observations` 等重复 helper 统一到 `external::process`。
- 已修复 clippy 暴露的问题：or-pattern 绑定不一致、测试专用 `child_id` 的 cfg 范围、共享 helper 的 `result_large_err` lint、旧 trait import 未使用。
- 验证已通过：`cargo fmt --all`、external feature clippy、默认 clippy、external feature 全目标测试、默认全量测试、rustdoc warning 门禁。
- 验证完成后仅更新了 markdown/任务记录和本计划文件；无需重跑编译测试。
- 下一步检查 git 差异并提交本次 `M8-2` 改动。
