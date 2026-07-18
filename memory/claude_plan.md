# Claude Plan — 当前任务跟踪

## 当前任务：M2-8 M2 review：external 生命周期收口

选自 TODO.md（第一个未完成标题无 [DONE] 的任务，M2-7 已完成于上一提交）。
这是 M2 里程碑的 review 任务，纯审查性质，按任务单头部规则必须跑全量门禁。

### 检查项（摘自 TODO.md）

1. 逐条核对 H-EXT-2、M-EXT-1~7、M-PROM-5 状态，`docs/review-2026-07.md` 已标注。
2. 重点复验：
   - force-close 后无存活孙进程（M2-1 的 process-group 测试）；
   - resume 后事件流无缺口（M2-2 的 resume high-water 测试）；
   - 崩溃 session 不再判 Graceful（M1-6 的 close_classification 测试）。
3. 全量门禁命令通过（含 external-acp feature 的 clippy）：
   - `cargo fmt --all`
   - `cargo clippy --all-targets -- -D warnings`
   - `cargo clippy --all-targets --features "external-claude-code external-codex external-opencode external-acp" -- -D warnings`
   - `cargo test --all --all-targets`
   - `cargo test --features "external-claude-code external-codex external-opencode external-acp" --all-targets`
   - `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`
4. `docs/managed-external-agent.md`、`docs/capability-matrix.md`、`AGENTS.md` 与实现一致。

### 执行步骤

1. 读 `docs/review-2026-07.md`，核对九条审查条目（H-EXT-2、M-EXT-1..7、M-PROM-5）
   均有 ✅ 标注且引用的 milestone 编号正确。
2. 代码点位抽查（grep）：process_group.rs、with_resume_high_water、
   runtime_output、`--` argv 分隔、prelude deadline、base_repo、
   with_worktree_manager / max_turns 强制等关键修复在场。
3. 重点复验三条行为的定向测试（external features）：
   process-group / close_classification / resume seq 相关测试。
4. 跑全量门禁（fmt → clippy 默认 → clippy external → test 默认 → test external → doc）。
5. 抽查 docs 一致性（managed-external-agent §16、capability-matrix、AGENTS.md
   的 safety properties 与库内 worktree 接线描述）。
6. TODO.md 标 [DONE] + 完成记录；review doc 若需补充则同步；git commit；停止。

### 进度日志

- [完成] 已读 TODO.md，确认当前任务 M2-8（M2 review），写下本计划。
- [完成] 条目核对：review doc 九条全部 ✅（M-EXT-2 不在 M2 清单内，待后续）。
- [完成] 代码点位抽查：process_group/resume seq/session_config/runtime_output/
  `--` argv/prelude deadline/base_repo/worktree manager/max_turns 均在。
- [完成] 重点复验：process-group kill 4 条 + close_classification 12 条 +
  resume seq 4 条 + machine dedup 1 条，全部通过。
- [完成] 文档核对：managed-external-agent §16、capability-matrix、AGENTS.md
  与实现一致。
- [完成] 全量门禁：fmt、clippy（默认 + external）、test 默认（50 目标 exit 0）、
  test external（48 目标 exit 0）、doc 全部通过。
- [完成] TODO.md M2-8 标 [DONE] + 完成记录。
- [进行中] git commit，然后停止。下一任务 M3-1（禁止 reverted head 上 compaction）。
