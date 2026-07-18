# Claude Plan — 当前任务跟踪

## 当前任务：M2-7 `ExternalSessionPolicy` 与 `WorktreeManager` 接入（M-PROM-5）

选自 TODO.md（第一个未完成标题无 [DONE] 的任务）。上一提交 b218809 完成 M2-6，
与本任务无直接未完成事项关联。

### 任务核心（摘自 TODO.md）

决策已定：`isolation` 采用**库内接线**方案——把 `GitWorktreeManager` 接进 `src/`
生产路径，接入点选定 registry 层（`ExternalSessionRegistry`）。

实现要求：

1. `ExternalSessionRegistry` 持有 `Arc<dyn WorktreeManager>`（构造时注入，默认
   `GitWorktreeManager::new()`）。
2. `get_or_start`（registry.rs:184）在 `adapter.start` 之前按
   `request.policy().isolation` 调 `WorktreeManager::prepare(agent_id,
   &request.worktree, isolation)`，把产出的 `PreparedWorktree` 路径作为会话工作
   目录传给 adapter（贯通到 `config.working_dir()` / opencode 的 `--dir`，一并
   修掉 `opencode/config.rs:254-257` 的漏传）。
3. `cleanup`/`cleanup_agent`（registry.rs:255、279）在会话关闭后按
   `ExternalSessionShutdown` 调 `WorktreeManager::cleanup(prepared, disposition)`；
   registry 需记住每个 live session 对应的 `PreparedWorktree`。
4. `permission_mode`：请求级覆盖 adapter 构造期 config；`max_turns`：传 CLI
   flag 或 machine 强制。未接入字段必须显式拒绝或文档标注未实现，不允许静默忽略。
5. 更新 AGENTS.md、`docs/managed-external-agent.md`、`docs/capability-matrix.md`。

验证条件：

- 决策与理由写入完成记录；文档措辞与实现一致。
- 单元测试：请求级 permission_mode 覆盖生效；registry 在 start 前调 prepare、
  cleanup 时按 disposition 调 cleanup（可用 `ScriptedGit`/`MockAdapter` 替身）。
- external feature 测试与 clippy 全过。

### 执行步骤

1. 探索：`registry.rs`、`worktree.rs`、`machine.rs`（policy 消费点）、三 adapter
   config 的 working_dir/permission_mode/max_turns 接线、opencode `--dir` 漏传。
2. 设计：`PreparedWorktree` 如何传给 adapter（start/resume 参数 vs config 覆盖）、
   registry 的 live session → PreparedWorktree 映射存储。
3. 实现 registry 接线 + adapter 工作目录贯通 + opencode `--dir` 修复。
4. 实现 permission_mode/max_turns 请求级接线或显式拒绝。
5. 单元测试（ScriptedGit/MockAdapter）。
6. 文档同步（AGENTS.md、managed-external-agent.md、capability-matrix.md、
   review-2026-07.md 标注 M-PROM-5）。
7. 门禁：fmt → clippy（默认 + external features）→ 全量测试 → doc。
8. TODO.md 标 [DONE] + 完成记录；git commit；停止。

### 设计定稿（探索后）

关键事实：
- `ExternalSessionRequest` 全 pub 字段 + serde；machine `build_request` 构造；
  registry 是唯一 choke point；facade `drive_external` 已声明
  `isolation: EphemeralGitWorktree`（目前无人执行）；示例用 `Shared`。
- 四个 adapter 的 config 均 `Clone`，且 launcher/spawn 在 `start`/`resume`
  内按 config 构造 → 「克隆 config 并应用请求级覆盖」是最小贯通方式。
- machine 已有 `max_decision_loops` 计数（`record_decision_loop`），
  `max_turns` 在 machine 层统一强制可覆盖全部 runtime 且离线可测。

决策：
1. **`ExternalSessionRequest` 新增 `session_dir: Option<WorktreeRef>`**
   （`#[serde(default, skip_serializing_if)]`，wire 兼容）。machine 置 `None`；
   registry prepare 后填入 prepared 路径。adapter 解析 cwd 优先级：
   `session_dir` > `config.working_dir()` > （ACP 既有回退 `request.worktree`）。
   不改写 `request.worktree`（保留 base 语义，cassette 不变）。
2. **registry 持有 `Arc<dyn WorktreeManager>`**：`new` 默认
   `GitWorktreeManager::new()`，新增 `with_worktree_manager` 注入。
   map value 改 `LiveEntry { handle, prepared }`；`get()` 签名不变。
   fresh start 与 resume 都 prepare；reattach 不 prepare。
   prepare 失败：start → `Launch`，resume → `ResumeUnavailable`。
   adapter.start 失败 → 以 `Graceful` 清理 prepared（ephemeral 树未被写过，
   避免泄漏）；cleanup/cleanup_agent 在 session 关闭后调
   `WorktreeManager::cleanup(prepared, disposition)`，cleanup 错误升级
   disposition 为 `Failed`。
3. **permission_mode**：请求级覆盖——adapter `start`/`resume` 内构造
   `session_config(request)`（clone config + `with_permission_mode(request.policy.
   permission_mode)` + session_dir → `with_working_dir`）， spawn/argv/decode
   context 全部走 effective config。ACP `session_over` 同步改用请求 mode
   （其 plan-mode 写门禁随之生效）与 session_dir 优先的 `session_cwd`。
4. **max_turns**：machine `block_on_session` 在 `record_decision_loop` 后检查
   `policy.max_turns`，超限 `LimitExceeded` 失败（统一、不传 CLI flag；
   文档写明 decision-loop 语义）。

### 进度日志

- [完成] 已读 TODO.md，确认当前任务 M2-7，写下本计划。
- [完成] 探索 registry/worktree/machine/四 adapter/facade，设计定稿（见上）。
- [完成] 实现：`ExternalSessionRequest.session_dir` 新字段（serde 兼容）+ 全部
  构造点接线；registry 持有 `Arc<dyn WorktreeManager>`（默认 GitWorktreeManager，
  `with_worktree_manager` 注入），start/resume 前 prepare、`LiveEntry` 记录
  prepared、cleanup/cleanup_agent 按 disposition 清扫（失败升级 Failed、start
  失败 Graceful 丢弃）；machine `block_on_session` 强制 `policy.max_turns`
  （LimitExceeded）；四 adapter `session_config` 请求级覆盖（permission_mode +
  session_dir，opencode 贯通 `--dir`）；`WorktreeCleanupOutcome::new` 公有化。
- [完成] 测试：registry 8 条新（stub manager + MockAdapter 捕获 session_dir）、
  四 adapter session_config 各 1 条、machine max_turns 1 条、ACP plan-mode 测试
  改请求级 Plan；testkit `PassThroughWorktreeManager`；四个真机 e2e 夹具改
  Shared（与示例同模式）。
- [完成] 文档：rustdoc（policy/request/registry）、managed-external-agent §16/§14、
  external-agent §5.1、capability-matrix、AGENTS.md、review-2026-07 M-PROM-5 ✅。
- [完成] 门禁：fmt、clippy（默认 + external features）、`cargo test --all
  --all-targets`、external features 全量、`cargo doc`（修 4 处 intra-doc link）
  全部通过。
- [完成] TODO.md M2-7 标 [DONE] + 完成记录（含 breaking change 记录）。
- [进行中] git commit，然后停止。下一任务 M2-8（M2 review）。
