# 当前执行计划

## 任务:M1-6 [TODO] `close()` 按退出码分类 Graceful/Failed(H-EXT-3)

来源:TODO.md 第一个未完成任务(M1-1..M1-5 均已 [DONE])。

### 任务要求

- 四处 close 站点统一改为按退出码分类:
  - `src/agent/external/claude_code/adapter.rs:198-199`
  - `src/agent/external/codex/adapter.rs:257-258`
  - `src/agent/external/opencode/adapter.rs:271-272`
  - `src/agent/external/acp/connection.rs:188-189`
  - `status.success()` → `Graceful`;否则 → 失败变体(查看 `ExternalSessionShutdown` 现有变体,必要时新增带 exit code 的变体,注意 serde 兼容与 `leaves_residual_side_effects()` 语义)。
- 检查 `ExternalSessionShutdown` 全部 match 点,确认穷尽匹配不破坏。
- 同步 `docs/managed-external-agent.md` §6.4 关闭分类描述。

### 验证条件

- 单元测试:exit 0 → Graceful;exit 1 → Failed 类;grace 超时 → ForcedKill 不变。
- `cargo test --features "external-claude-code external-codex external-opencode" --all-targets`
- `cargo clippy --all-targets --features "external-claude-code external-codex external-opencode" -- -D warnings`
- 全量门禁:cargo fmt / clippy(无 feature)/ cargo test --all --all-targets / doc。

### 执行步骤

1. [ ] 读 `ExternalSessionShutdown` 定义与全部 match 点(worktree.rs、machine、handler 等)。
2. [ ] 确定失败变体方案:优先复用现有失败变体;若无合适的,新增带 exit code 的变体,检查 serde 兼容与 `leaves_residual_side_effects()`。
3. [ ] 四处 close 站点改为 `status.success()` 分类。
4. [ ] 补单元测试:exit 0 → Graceful;exit 非 0 → Failed 类;超时路径 → ForcedKill 不回归。
5. [ ] 更新 `docs/managed-external-agent.md` §6.4。
6. [ ] 跑 fmt → clippy(带 feature)→ clippy(默认)→ 全量 test → doc。
7. [ ] TODO.md 标记 [DONE] + 完成记录;`docs/review-2026-07.md` H-EXT-3 标注(若 M1-7 才做则看条目惯例,参照 M1-5 已完成记录中 review 文档勾销惯例)。
8. [ ] git commit,停止。

### 进展日志

- 2026-07-19:开始,读 TODO.md 确认 M1-6 为第一个未完成任务,正在探索代码。
- 2026-07-19:**M1-6 完成**。
  - 变体选型:复用现有 `ExternalSessionShutdown::Failed`(Copy 分类载体,详细文本留 `ShutdownFailed`),未新增变体 → serde wire 不变、无穷尽匹配破坏。
  - 四处 close 站点改为 `status.success()` guard 分类:0 → Graceful,非 0 → Failed,超时 → ForcedKill 不变。
  - 四模块各加 `close_classification` 测试子模块(真实 `sh -c` 子进程):exit 0/1/超时 3 条 × 4 = 12 条全过。
  - 文档:managed-external-agent.md §12/§16、external-agent.md §6.4、review-2026-07.md H-EXT-3 标注 ✅。
  - 门禁:fmt / clippy(默认+external features)/ feature 测试 / 全量测试 / rustdoc 全绿。
  - TODO.md 已标 [DONE] + 完成记录。随后 commit 并停止。
