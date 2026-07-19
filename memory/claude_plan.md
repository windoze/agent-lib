# 当前执行计划

## 约束

- 以 `TODO.md` 为唯一任务顺序与完成状态来源。
- 本轮只完成第一个未标记 `[DONE]` 的任务，然后停止。
- 如遇到阻塞当前任务的具体前置问题，先在 `TODO.md` 中插入最小必要前置任务并提交，然后停止。
- 不做开放式历史问题泛扫；仅处理会阻塞当前任务或使当前任务行为失效的问题。
- 不记录隐藏推理链；本文件只记录可审阅的执行计划、决策与进度。

## 步骤

1. 读取 `TODO.md`，定位第一个标题未带 `[DONE]` 的任务。
2. 查看最近提交信息，判断是否明确提到与当前任务直接相关的未完成事项。
3. 阅读当前任务相关代码、文档和测试，确认要求、依赖与验证方式。
4. 以最小正确改动实现当前任务；若发现必须先完成的具体前置问题，则更新 `TODO.md` 并停止。
5. 按仓库要求先运行 `cargo fmt --all`，再运行 clippy，最后运行相关或全量测试；任何未排期失败测试都必须修复或排入当前任务前置。
6. 完成后更新 `TODO.md`：任务标题加 `[DONE]`，补充完成记录；仅在阶段计划确实变化时更新 `PLAN.md`。
7. 检查 `git status`、`git diff` 和最近提交，提交本轮相关更改。
8. 停止，不进入下一个任务。

## 进度

- 已刷新本轮执行计划。
- 已读取 `TODO.md` 并定位首个未完成任务：`M9-1 [TODO] panic/poison 策略统一`。

## 当前任务计划：M9-1 panic/poison 策略统一

1. 检查最近提交信息，确认是否有与 M9-1 直接相关的未完成事项。
   - 已完成：最近提交为 `[M8-3] Review consolidation cleanup`，未提到 M9-1 相关未完成事项；工作区仅有本轮 `memory/claude_plan.md` 计划更新。
2. 搜索生产代码中 mutex/rwlock poison 相关 `expect("...poisoned...")`，统一改为中毒恢复（`unwrap_or_else(|e| e.into_inner())`）或少量文档化例外。
   - 已完成：`src/` 中 `expect("...poison...")` 清零；生产路径 `lock().expect(...)` 清零；测试替身中的同类锁也改为恢复中毒锁。
3. 搜索本任务点名的其他生产 `expect`，对真正不变量保留但确保 panic 消息具备上下文，必要时改为 `debug_assert` + 防御分支。
   - 已完成：任务单中 `drive.rs:603` 旧点位已不存在；当前 `fulfill_batch` 的 handler-presence 不变量从 `expect` 改为 `debug_assert` + `AgentError::Other` 防御分支。
4. 更新 `AGENTS.md` 的 Conventions，写明库代码的 poison 恢复策略与不变量 panic 例外。
   - 已完成：`AGENTS.md` Conventions 新增标准库锁中毒恢复约定，以及少量不变量 panic 的上下文/防御分支要求。
5. 按验证条件运行 grep、格式化、clippy、测试与 rustdoc；若发现未排期失败测试，先修复或在 `TODO.md` 插入前置任务。
   - 已完成：`cargo fmt --all`、任务指定 grep、默认 clippy、external features clippy、`cargo test --all --all-targets`、external features 全目标测试、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` 全部通过。
6. 更新 `TODO.md` 将 M9-1 标为 `[DONE]` 并写入完成记录；阶段计划未变化时不更新 `PLAN.md`。
   - 已完成：`TODO.md` 的 M9-1 标题已改为 `[DONE]` 并写入完成记录；`docs/review-2026-07.md` 对应 Mutex poison 条目已标注 `✅ 已修复（M9-1）`；`PLAN.md` 无阶段级变化，未更新。
7. 检查 git 状态、差异和最近提交，提交本轮相关更改后停止。
   - 正在进行：已检查 `git status --short`、`git diff --stat`、`git log --oneline -10` 和完整 `git diff`；差异范围符合 M9-1，下一步提交。
