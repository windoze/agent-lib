# 执行计划（2026-07-19）

## 当前任务：M2-5 [TODO] 决策点后 reap 子进程 + prelude 总时限与取消（M-EXT-5、M-EXT-6）

### 任务理解（来自 TODO.md）

问题（两个，审查条目 M-EXT-5、M-EXT-6）：
1. **close disposition 被吞**：`codex/adapter.rs:462-464`（opencode 同）`let _ = old.close().await;`
   丢弃 disposition——ForcedKill 被吞，不进 trace 也不影响 worktree 残余副作用判定。
   背景：decoder 在 `turn.completed` 行即返回 decision，不读到 EOF（`codex/adapter.rs:559-561`），
   子进程可能还活着，close 的 disposition（M1-6 分类）必须被观测。
2. **prelude 循环无总时限/无取消**：`claude_code/adapter.rs:298-311`（codex/opencode 同构）
   `begin()` 的 prelude 循环 `while self.decoder.session_id().is_none()`，per-line timeout 每行
   重置、无 `ctx.is_cancelled()` 检查、无总 deadline；`advance()` 循环反而有取消检查。

实现要求：
- close disposition 不再 `let _ =` 丢弃：记录进 trace/日志，并在判断 worktree 残余副作用时纳入
  （与 M1-6 的分类联动）。
- prelude 循环加总 deadline（用 M1-5 的 launch timeout）与 `ctx.is_cancelled()` 检查；
  超时/取消走正常错误路径。
- 三个 adapter 行为一致。

验证条件：
- 单元测试：fake CLI 持续吐非 init 帧时 `begin()` 在 deadline 内返回错误而非挂起。
- 单元测试：close 超时被强杀的场景 disposition 被观测到（断言 trace 或返回值包含 ForcedKill）。
- external feature 测试与 clippy 全过。

### 执行步骤

1. 探索代码：三个 adapter 的 `begin()` prelude 循环、`advance()` 循环、close 调用点（`let _ =`），
   理解 session/decoder/registry 之间 disposition 的流向；确认 trace/log 设施（用什么记录
   disposition——`ExternalSessionResult`？trace event？log？）；确认 worktree 残余副作用判定的
   现有消费点（registry/worktree）。
2. 设计并记录选型：
   - prelude deadline：用 `config.timeout()`（M1-5 的 launch 语义）包整个 prelude 循环；
     取消检查每行迭代做。超时映射到合适的 `ExternalAgentError` 变体（看现有 `TimedOut` 等）。
   - close disposition 观测：找到所有 `let _ = ...close().await` 点（grep），改为把 disposition
     记录下来——候选通道：(a) 返回值传给调用方（registry cleanup 已有 disposition 消费），
     (b) 日志（`tracing`? 检查项目是否用 tracing/log），(c) trace。以现有设施为准。
3. 三个 adapter（claude_code/codex/opencode）一致实现。
4. 测试：
   - fake CLI 持续吐非 init 帧 → `begin()` 在 deadline 内返回错误（用小 timeout 配置注入）。
   - close 强杀场景 disposition 被观测到。
5. 门禁：fmt → clippy（默认 + external features）→ 全量测试 → external features 测试 → doc。
6. 文档：`docs/managed-external-agent.md` 相关节同步（若行为描述变化）；
   `docs/review-2026-07.md` M-EXT-5/M-EXT-6 打标。
7. TODO.md 标题改 `[DONE]` + 完成记录；commit `[M2-5] ...`；停止。

### 设计选型（探索后记录）

探索结论：
- `begin()` 当前不接收 `ctx`；adapter `start`/`resume` 持有 `ctx` 与 `config.timeout()`。
  测试直接调 `session.begin(...)`（claude 8 处、codex/opencode 各 ~12 处调用点）。
- `let _ = old.close().await;` 只在 codex/adapter.rs:496 与 opencode/adapter.rs:516
  （claude 单进程跨 turn，无 mid-turn close；acp 也无）。
- trace 设施：`ctx.trace().record_external_shutdown(TraceNodeId, disposition)`（best-effort，
  budget.rs 已有先例：run_id + 计数器 mint id）；`ctx.trace().records()` 可在测试断言。
- worktree 判定通道：registry `cleanup`/`cleanup_agent` → `session.shutdown()` 的
  disposition → host 传给 `WorktreeManager::cleanup`。
- `RunContext::cancellation().cancel()` 可在测试中取消；`ctx.is_cancelled()` 检查。

选型：
1. **M-EXT-6（prelude deadline + cancel）**：`begin` 增加 `ctx: &RunContext` 与
   `prelude_timeout: Duration` 两参（adapter 传 `config.timeout()`，M1-5 launch 语义）。
   循环每轮查 `ctx.is_cancelled()`（取消 → `SessionLost`，与 advance 口径一致）；
   整个 prelude 用 `tokio::time::timeout_at(deadline, read_line)` 总限时，超时 →
   fresh 报 `Launch`、resume 报 `ResumeUnavailable`（与 spawn 失败同一分类轴）。
2. **M-EXT-5（close disposition 不丢）**：codex/opencode session 新增
   `worst_close: Option<ExternalSessionShutdown>` + `close_trace_seq: u64`；
   `spawn_follow_up_turn` 收 `ctx`，close 后 `note_close()`：best-effort 写
   `record_external_shutdown` trace 节点 + merge 进 `worst_close`。
   `shutdown()` 返回 `worst_close` 与当前 close 的 merge（severity:
   Graceful < Failed < ForcedKill），使 mid-turn ForcedKill/Failed 经 registry cleanup
   流入 worktree 残余判定。`shutdown.rs` 新增 `merge` helper（共享、带测试）。
   claude 无 mid-turn close，无需改 shutdown；三 adapter 一致的是 prelude 修复。
3. Reap 时机不变（decision 后进程留到下轮 spawn / shutdown 才 close）——任务要求只修
   disposition 通道，提前 reap 不在范围内。

测试计划：
- 每 adapter：prelude 总时限测试（fake 无限吐非 init 帧 + 小 prelude_timeout →
  限时内返回 Launch/ResumeUnavailable 而非挂起）；取消测试（ctx 预取消 → SessionLost）。
- codex/opencode：mid-turn close ForcedKill → trace 含 `ExternalShutdown{ForcedKill}` 节点
  且 `shutdown()` 返回 merge 后的 ForcedKill（fake per-turn close 序列 [ForcedKill, Graceful]）。
- shutdown.rs：`merge` 严重性序单测。

### 进展日志

- [x] 步骤 1：代码探索（三 adapter begin/advance/close、registry、sweep、trace、worktree 通道）
- [x] 步骤 2-3：实现（shutdown.rs `merge` + 三 adapter prelude deadline/cancel + codex/opencode
  `note_close` trace+worst_close merge）
  - **踩坑**：fake transport 瞬时 ready 会镀死 tokio timer（`timeout_at` 只在 future yield 时
    才有机会触发）→ prelude 循环加显式 `Instant::now() >= deadline` 检查；首个后台测试运行
    因此挂起 15 min（500% CPU 热转），已杀进程修复，修复后 349 条 external lib 测试 0.28s 全过。
- [x] 步骤 4：测试（349 条 external lib 测试通过；新测试：三 adapter 各 prelude 超时+取消，
  codex/opencode 各 resume 超时+mid-turn close trace/merge，shutdown.rs merge 单测）
- [x] 步骤 5：门禁（fmt / clippy 默认+external / 全量测试 / external 全目标测试 / doc 全过）
- [x] 步骤 6-7：文档（managed-external-agent.md §12/§13/§14/§16、external-agent.md §6.4、
  review-2026-07.md M-EXT-5/6 打标）+ TODO.md [DONE] + 完成记录
- [x] 步骤 8：commit

任务 M2-5 已完成。
