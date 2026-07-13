# 执行计划 — M5-3：Observability：trace 记 resolved-by-scope 与 disposition

## 选中的任务
`TODO.md` 第一个未完成任务 = **M5-3**（M1..M5-2 全 `[DONE]`）。前置 M5-2 已完成。

## 任务目标（TODO.md M5-3 / 迁移文档 §8/§11 / effect-model §8）
动态作用域要求 trace 记录每个 requirement：
1. 被哪层 scope 的 handler 兑现（resolved-at-scope）。
2. 兑现结果是 resume 还是 never-resume（disposition）——never-resume（cancel）是真实发生、
   影响下层 Conversation 的事件，必须留痕，不是 non-event。
新增 `TraceNodeKind::Requirement { kind_tag, resolved_at_scope, disposition }`，
`disposition ∈ { Resumed, NeverResumed }`。与旧 trace tree（run→step→llm/tool/sub-agent）对齐。

## 关键设计决策
### resolved_at_scope 的表示
- 用 `u32` 表示「从 requirement 被 perform 的那层 scope 起，向外 pop 了几跳才被兑现」。
  0 = 本层（emitting scope）自己兑现；1 = 向外 pop 一层兑现；依此类推。
- 这是动态作用域下最直接可测的「哪层兑现」的相对表示，且天然经 pop 链累加。

### hop 计数如何穿过 pop 链（drive.rs）
- `Pop::pop` 返回类型改为 `Result<(RequirementResult, u32), AgentError>`，
  u32 = 从「本 pop 目标的 scope」起到真正兑现处的跳数。
- `resolve_requirement` 返回 `Result<(RequirementResult, u32), AgentError>`：
  - 本层兑现（subagent handler 或 fulfill_with_scope）→ `(result, 0)`。
  - pop → `let (r, h) = parent.pop(req, ctx)?; (r, h + 1)`（+1 = 到 parent 的那一跳）。
  - 顶层无 handler → `Err(UnhandledRequirement)`（不记录）。
- `ScopePop::pop` → `resolve_requirement(req, self.scope, self.parent, ctx)`，原样返回（+1 由调用方加）。
- `fulfill_batch` 返回 `Vec<Resolved{ resolution, resolved_at_scope }>`：
  本层并发集 hop=0；串行集经 resolve_requirement 得 hop。

### 记录点集中在 drain（单处，且只记「真会被 Resume/Abandon」的）
- `drain` 收到 `fulfill_batch` 的 `Vec<Resolved>`（都是 Ok、都会被 Resume）：
  每个 `record_requirement(ctx, tag, resolved_at_scope, Resumed)` 后再 `Resume`。
- cancel 分支：`record_requirement(ctx, tag, 0, NeverResumed)` 后再 `Abandon`。
  （cancel = 本层的 never-resume handler，故 scope=0。）
- trace 记录用 `ctx.trace()`（emitting 层的 trace parent：root 或 sub-agent 节点）。
- trace 节点 id 复用 requirement 的 host-minted id（库不造 id 哲学）。
- Trace 记录失败（重复 id / 未知 parent）→ `AgentError`（经 `RunContextError::Trace`，kind=Trace）。

### trace.rs
- 新增 `RequirementDisposition { Resumed, NeverResumed }`（Copy, serde snake_case）。
- `TraceNodeKind` 增 `Requirement { kind_tag: RequirementKindTag, resolved_at_scope: u32,
  disposition: RequirementDisposition }`。字段全 Copy → 枚举仍 `Copy`，`kind()` 不变。
- `TraceHandle::record_requirement(id, kind_tag, resolved_at_scope, disposition)`。
- context.rs / agent/mod.rs re-export `RequirementDisposition`。

## 聚焦测试
1. drive.rs：含 pop 的兑现在 trace 记录正确 `resolved_at_scope`（本层=0，pop 一层=1），disposition=Resumed。
2. drive.rs：cancel 一次在 trace 记录 `NeverResumed`。
3. context/tests.rs（或 trace.rs）：新 Requirement trace record serde round-trip。

## 验证
- `cargo fmt --all` → `cargo clippy --all-targets -- -D warnings` → `cargo test --all --all-targets`
  → `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` → `git diff --check`。

## 进度（完成）
- [x] trace.rs：RequirementDisposition + TraceNodeKind::Requirement + record_requirement
- [x] context.rs / mod.rs re-export
- [x] drive.rs：Pop/resolve_requirement/fulfill_batch hop 穿透 + drain 记录
- [x] 测试 1/2/3（drive.rs ×2 + context/tests.rs ×1）
- [x] fmt(clean) / clippy(0 warning) / test(lib 435 passed) / doc(clean) / diff --check(clean)
- [x] TODO.md 标记 [DONE] + 完成记录
