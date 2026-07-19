# 当前任务：M4-4 引入非破坏性 step 错误出口（软拒绝）（M-ERR-1 及连带项）

## 任务理解

来源 TODO.md M4-4。现状问题（上下文）：

1. `src/agent/machine/default/mod.rs:1048-1058`：任何 `StepError` → `fail_from` → `fail`
   → `cancel_pending(DiscardTurn)` + Error cursor。stale resume id（mod.rs:706-713）、
   不合法边界 pivot（mod.rs:576-588）、turn 中途第二条 UserMessage 都会销毁整个
   pending turn。
2. `fail()` 自身吞错（mod.rs:1021-1030）：转移表（`src/agent/state/cursor.rs:308-344`）
   无 `(Done|Error) → Error` 边，机器停在 Done/Error 时 fail 的转移静默失败，错误
   消息丢失。
3. `NestedMachine::route_by_id` fallback（`src/agent/machine/nested.rs:266-272`）：
   未知 id 的 Resume/Abandon 转发给根机 → 走破坏性 fail。
4. 步数上限误用终态：`machine/default/tools.rs:593-604` 达 `max_steps` 走
   `LoopCursor::Error`，而 `LoopDoneReason::StepLimitReached`（cursor.rs:650）是死变体。
5. during-turn reconfig abandon 用 DiscardTurn（mod.rs:978-986），而 tool abandon 用
   ResumeTurn 保全工作（tools.rs:701-716）。

## 实现要求（TODO 规格）

- `StepOutcome` 增加软拒绝表达（如 `rejected: Option<StepRejectReason>` 或专用
  outcome 变体）：协议违规类输入（stale id、非法边界 pivot、turn 中重复
  UserMessage、未知路由 id）被拒绝但机器状态不变。
- `fail()` 的 cancel_pending 与 cursor 转移失败必须显式处理（至少日志 +
  outcome 标注），不再 `let _ =`。
- 步数上限改用 `LoopDoneReason::StepLimitReached` 正常终态（保留已完成 tool
  结果的提交路径，与文本提交一致）。
- during-turn reconfig abandon 与 tool abandon 对齐（优先 ResumeTurn 保全文本
  响应）。
- 该任务触及 `AgentMachine` trait 的公共契约，属 breaking change，完成记录注明。

## 验证条件

- 单元测试：四类协议违规输入各自被软拒绝、cursor 不变、pending turn 完好、后续
  正常输入可继续。
- 单元测试：max_steps 到达后 cursor 为 Done(StepLimitReached) 且已冻结的 tool
  结果不丢。
- `cargo test -p agent-lib --lib agent::machine` 与 nested 测试全过。
- `docs/agent-effect-model.md` 的 step 契约节同步。

## 执行计划

1. [ ] 探索代码：`StepError`/`StepOutcome`/`StepInput` 形状、`fail`/`fail_from`/
   `cancel_pending`、四类违规路径点位、nested route_by_id、tools.rs max_steps、
   reconfig abandon；`docs/agent-effect-model.md` step 契约节。
2. [ ] 设计软拒绝 API 形状（选型：`StepOutcome.rejected: Option<StepRejectReason>`
   vs 专用变体；写入完成记录）。
3. [ ] 实现软拒绝：stale resume id、非法边界 pivot、turn 中重复 UserMessage、
   nested 未知路由 id。
4. [ ] `fail()` 吞错修复：cancel_pending/转移失败显式处理。
5. [ ] max_steps → `LoopDoneReason::StepLimitReached` 正常终态 + tool 结果提交路径。
6. [ ] reconfig abandon 对齐 ResumeTurn。
7. [ ] 测试（每类违规一条 + max_steps 一条 + reconfig abandon 一条）。
8. [ ] 文档：`docs/agent-effect-model.md` step 契约节；`docs/review-2026-07.md`
   M-ERR-1 标注 `✅ 已修复（M4-4）`。
9. [ ] 门禁：fmt → clippy（默认 + external features）→ test（默认 + external）→ doc。
10. [ ] TODO.md 标 `[DONE]` + 完成记录；commit。

## 最终设计（探索后定稿）

**选型：`StepOutcome` 增加 `rejected: Option<StepRejectReason>` 字段**（serde default +
skip_serializing_if），不新增 outcome 变体。`StepOutcome::new` 保持 3 参（rejected=None），
新增 `StepOutcome::rejected(reason)` 构造（quiescent=true——被拒步不改变状态，机器仍停在
原卡点）。软/硬边界：软拒绝 = 输入在当前位置不适用（stale/未知 id、非法 pivot 边界、
turn 进行中第二条 UserMessage）；硬失败 = payload kind 不匹配与内部不一致（保持不变）。

**`StepRejectReason`**（pub，machine/mod.rs，serde tag=kind/content=detail）：
`UnknownRequirement(String)` / `IllegalPivotBoundary(String)` / `TurnInProgress(String)`。

**`StepError::Rejected(StepRejectReason)`**（crate 私有新变体）；`step()` 把它映射为
rejected outcome（不动状态），其余错误仍走 `fail_from`。

软拒绝点位（default machine）：
1. `resume` 无 outstanding 分支（mod.rs:695-700）→ UnknownRequirement
2. `resume_llm` stale id（711-718）→ UnknownRequirement
3. `resume_tool` 未知 id（tools.rs:407-412）→ UnknownRequirement；"no active tool phase" 保持硬
4. `resume_approval` 两个 stale id 检查（500-513）→ UnknownRequirement + **重排到 `.take()` 之前**
   （现状先 take 再校验，软拒绝要求零状态变化）
5. `resume_reconfig` stale id（867-874）→ UnknownRequirement；"no deferred reconfig" 保持硬
6. `abandon` 无 outstanding 分支 + id 不匹配（945-956）→ UnknownRequirement
7. `inject_pivot`：非 StreamingStep / 无 outstanding LLM req / `pivot.validate()` /
   `inject_user_message` 错误 → IllegalPivotBoundary
8. `begin_user_turn`：cursor 非 Idle/Done/Error 时在**任何变更之前** → TurnInProgress
9. nested：`route_by_id`/`start_pending_children`/`step` 聚合传播 `rejected`；未知 id fallback
   经 own machine 自然变成软拒绝（修复"迟到的 cancel 销毁根机 turn"）

**fail() 加固**：转移表加 `(Done|Error) → Error` 边（error 停靠对所有 kind 全可达）；
`fail_with_notifications` 的 cancel_pending 失败折叠进错误消息；空消息回退固定文本；
transition 失败（加边后结构性不可达）用 debug_assert + 注释显式声明，不再 `let _ =`。

**max_steps → 正常终态**：`finish_tool_phase` 改调新 `finish_step_limit`——
`cancel_pending(ResumeTurn { cancelled_results: vec![] })`（tool phase 已 drain、无 open call，
空闭包合法，冻结的 tool 结果保留在可恢复 pending 中，与 tool abandon 同一保全语义）+
cursor → `Done(StepLimitReached)` + 保留 boundary notifications。facade 两侧
（agent.rs:374、stream.rs:278）在 `Done` 分支前加
`Done(reason == StepLimitReached) → FacadeError::LoopLimitExceeded`（保住既有 facade 契约与
`exceeding_the_tool_round_budget_fails` 测试；结构化判定取代字符串匹配的改动归 M5-3）。

**reconfig abandon 对齐**：during-turn park 时 pending 在 ReadyToCommit，`ResumeTurn` 被
conversation 层结构性拒绝（prepare.rs:189）——保全文本的唯一闭包是 `commit_pending`。
`abandon_reconfig` 改为：有 pending → `commit_pending(TurnMeta::default())`（文本落账、
reconfig 留队列下轮重试）；无 pending（start-of-turn park）→ 直接 finish_cancel。选型理由
写入完成记录。

**测试**：更新 8 条既有失败断言为软拒绝断言；新增——mid-turn UserMessage 软拒绝后正常
resume 完成 turn、approval stale id 软拒绝且 scratch 未丢、nested 未知 id 软拒绝、
reconfig abandon 文本落账、StepOutcome rejected serde round-trip。

**文档**：`docs/agent-effect-model.md` step 契约节（§2.2/§8）、
`docs/agent-effect-migration.md` §2.1 类型草图加 `rejected` 字段、
`docs/review-2026-07.md` M-ERR-1 标 ✅。

## 进度日志

- 2026-07-19：开始 M4-4。上一个任务 M4-3 已完成（HEAD b287768）。探索完成，设计定稿
  （见上节）。开始实现。
- 2026-07-19：实现完成——StepOutcome.rejected + StepRejectReason、StepError::Rejected、
  8 处软拒绝点位、fail() 加固（转移表 (Done|Error)→Error 边）、max_steps →
  Done(StepLimitReached) + ResumeTurn 保全、facade 两侧结构化 LoopLimitExceeded 映射、
  abandon_reconfig → commit_pending 保全文本、nested rejected 传播、testkit
  StepObservation.rejected。测试全部更新/新增，lib 944 + facade 210 全过。下一步：文档。
- 2026-07-19：文档完成（agent-effect-model §2.4、migration §2.1、agent-layer §4.2、
  review-2026-07 M-ERR-1 ✅）。发现并修复集成测试
  `step_limit_parks_on_error_before_second_model_step`（断言旧 Error 终态）——教训：
  后台全量测试不能经管道掩盖 exit code。全量门禁通过：fmt / clippy（默认 + external
  features）/ test（默认 50 目标 + external features，均 exit 0）/ doc。TODO.md M4-4
  已标 [DONE] 并写完成记录。M4-4 完成，提交后停止。
