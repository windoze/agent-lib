# M2-6 执行计划：worktree cleanup 使用记录的 base repo（M-EXT-7）

## 任务摘要

`src/agent/external/worktree.rs` 中 `PreparedWorktree` 不保存 base repo 路径，
cleanup 时把 worktree 自己当 `git -C` 目录（靠 git 向上发现 gitdir），
当 worktree 目录部分损坏/被移动时 cleanup 失败。

## 关键实验结论（真实 git，本地临时目录）

- worktree 的 `.git` 文件被删后，`git -C <base> worktree remove --force <path>`
  **仍然失败**（"验证失败，无法删除工作区"）；double `--force` 同样失败。
- `git -C <base> worktree prune` 能清除 stale admin entry（.git 缺失或目录被移动均可），
  之后直接 `remove_dir_all` 剩余目录即可完成清理。
- 因此修复 = 记录 base repo 作 `-C` **+** remove 失败时针对"树已损坏/移动"
  （以 `worktree/.git` 是否存在判定，非错误字符串匹配）的 prune+rmdir 兜底；
  其他失败原因（如 locked）原样报错，不过度兜底。

## 实现方案（已定）

- `PreparedWorktree` 不参与 serde 持久化（无 serde derive），无需 serde default。
- 新增私有字段 `base_repo: Option<WorktreeRef>`；`new()` 签名不变（= None，
  缺省回退旧行为：以 worktree 自身作 `-C`）；新增 `with_base_repo()` builder 与
  `base_repo()` accessor（避免 public API breaking change）。
- `GitWorktreeManager::prepare` 三个分支均 `.with_base_repo(base.clone())`。
- cleanup 用 `base_repo` 作 `-C`。
- `SystemGit::remove_worktree` 加固：`worktree remove --force` 失败且
  `worktree/.git` 不存在时 → `worktree prune` + best-effort `remove_dir_all`
  （NotFound 忽略）；否则原错误上抛。trait 文档同步强化契约。

## 测试

1. ScriptedGit 记录 remove 的 repo 参数：cleanup 断言 `-C` == base repo 而非 worktree。
2. 兼容回退：`new()` 手工构造（无 base repo）→ `-C` == worktree（旧行为）。
3. 真实 git 集成（临时目录、离线、git 缺失时优雅跳过）：
   - A：删除 worktree `.git` → cleanup 成功、目录消失、admin entry 清除。
   - B：移动 worktree 目录 → cleanup 成功、admin entry 清除。

## 验证与收尾

- fmt / clippy（默认 + external features）/ 全量 test / doc。
- `docs/managed-external-agent.md` 1359/1386 行措辞同步；`docs/review-2026-07.md`
  M-EXT-7 标注 ✅；TODO.md 标 [DONE] + 完成记录；提交。

## 进度记录

- [x] 代码阅读与真实 git 实验
- [x] 实现 worktree.rs 修改（base_repo 字段 + prepare 记录 + cleanup 使用 + SystemGit 兜底）
- [x] 新增测试（4 条：ScriptedGit 断言 -C、兼容回退、真实 git 损坏/移动 2 条）
- [x] 门禁验证：fmt / clippy（默认 + external features）/ 全量 test / doc 全过
- [x] 文档：managed-external-agent.md §16 同步；review-2026-07.md M-EXT-7 标 ✅；TODO.md 标 [DONE]

## 完成总结

M2-6 完成。核心决策：仅改 `-C` 为 base repo 不足以满足验证条件（实验证明 git
对损坏树连 base repo 下的 remove 也拒绝），故 `SystemGit::remove_worktree` 增加
以 `worktree/.git` 存在性为判定的 prune + rmdir 兜底，其他失败原因原样上抛。
无 breaking change。
