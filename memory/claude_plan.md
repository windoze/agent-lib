# M9-1 实现 worktree isolation 管理与 cleanup 标记

**当前任务 = TODO.md 第一个未完成任务 = M9-1**（`### [TODO] M9-1`, line 2676）。
M1..M8 全部 `[DONE]`。M9-1 是 M9（Managed External Agent hardening）第一子任务。

## 任务要求（TODO.md 2676-2698）
1. 新增 `WorktreeManager` trait / adapter hook：
   - `prepare(worktree ref, isolation) -> prepared worktree`
   - `cleanup(prepared, shutdown disposition)`
   - 标记 residual side effects
2. session registry cleanup 时记录 `ExternalSessionShutdown`（已返回 disposition，补文档+测试证明协调）。
3. ephemeral worktree graceful 后删除；forced/failed 保留或标记；策略写文档。
4. 验证：unit tests 覆盖 shared/per-agent/ephemeral 三策略；forced/failed 不误标 clean；
   `cargo test -p agent-lib external_worktree`；完整验证序列 1-6。

## 现状核对（已读源码）
- `WorktreeIsolation::{Shared,PerAgentWorktree,EphemeralGitWorktree}` @ external/mod.rs:198。
- `ExternalSessionShutdown{Graceful,ForcedKill,Failed}` + `leaves_residual_side_effects()` @ shutdown.rs。
- `ExternalAgentState::{cleanup_required,mark/clear_cleanup_required}` @ state.rs。
- registry `cleanup/cleanup_agent` 返回 `ExternalSessionShutdown` @ registry.rs:255/279。
- `TraceHandle::record_external_shutdown` 已存在 @ context/trace.rs:316。
- `WorktreeRef` @ agent/spec.rs:95。`AgentId: Display`。
- 设计意图 docs/managed-external-agent.md §16（拟新增 `WorktreeManager`），status table line 124「拟新增」。

## 设计
新文件 `src/agent/external/worktree.rs`：
- `PreparedWorktree`（Clone/Debug/PartialEq/Eq）：agent_id, isolation, worktree(WorktreeRef), ephemeral。accessors。
- `WorktreeCleanupOutcome`：isolation, worktree, removed:bool, residual_side_effects:bool；`safe_to_reuse()=!residual`。
- `WorktreeError`（thiserror Clone/PartialEq/Eq）：Prepare{isolation,path,detail} / Cleanup{...}。
- `WorktreeGitExec`（async trait）：`add_worktree(repo,worktree)` / `remove_worktree(repo,worktree)` -> `Result<(),String>`。
- `SystemGit` impl：`git -C <repo> worktree add --detach <path> HEAD` / `worktree remove --force <path>`（tokio::process）。
- `WorktreeManager`（async trait, object-safe）：`prepare(agent_id,base,isolation)` / `cleanup(prepared,disposition)`。
- `GitWorktreeManager<G=SystemGit>`：
  - root = temp_dir()/"agent-lib-worktrees"，`with_root` 可覆盖（测试用）。
  - prepare：Shared→原 base 无 IO；PerAgent→root/agent-<id> 幂等复用否则 git add；Ephemeral→root/ephemeral/<uuid> 总是 add。
  - cleanup：Shared/PerAgent→removed=false，residual=disposition.leaves_residual_side_effects()；
    Ephemeral graceful→git remove removed=true residual=false；forced/failed→保留 removed=false residual=true。
- tests（inline，fn 名 `external_worktree_*` 匹配 filter）：三策略、幂等、graceful remove、forced/failed 不误标 clean、
  shared/per-agent residual、git 失败冒泡、disposition 协调。

导出：external/mod.rs `pub use worktree::{...}`；agent/mod.rs re-export。
文档：docs/managed-external-agent.md §16 更新为已实现 + 策略表；status table line 124「已实现」。

## 验证序列
1. cargo fmt --all
2. cargo clippy --all-targets -- -D warnings
3. cargo test -p agent-lib external_worktree
4. cargo test -p agent-lib external
5. git diff --check
6. 全量 cargo test --all --all-targets（<=30min）

## 进度
- [ ] 写 worktree.rs
- [ ] 导出 + 文档
- [ ] fmt/clippy/test
- [ ] TODO.md DONE + commit

## 完成状态（M9-1 DONE）
- [x] worktree.rs（types + WorktreeManager trait + GitWorktreeManager + SystemGit + WorktreeGitExec + 12 单测）
- [x] 导出（external/mod.rs + agent/mod.rs）+ 文档（managed-external-agent.md §16 + status table）
- [x] 计数器替代 uuid v4（crate 未启用 v4 feature；遵循 no-random 约束）
- [x] fmt/clippy(default+3 features)/doc(workspace)/git diff 全干净
- [x] cargo test -p agent-lib external_worktree → 12 passed
- [x] cargo test --all --all-targets → 46 ok, 0 failed, exit 0
- [x] TODO.md M9-1 → [DONE] + 完成记录
- [ ] commit（进行中）
