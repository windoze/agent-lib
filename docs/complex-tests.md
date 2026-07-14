# 复杂 Mock 测试设计

> 目标:在现有 `agent-testkit` 基础上补一组复杂组合测试,用 mock 覆盖多轮会话、tool approval/deny、subagent、plan/blackboard、cancel、pivot message 之间的交互。重点不是增加很多互相重复的 happy path,而是用少量场景把容易错的边界组合起来。

## 1. 范围

本设计只覆盖 agent 层 mock 测试:

- 使用 `DefaultAgentMachine`、`NestedMachine`、`drain`、`StepHarness`、`DrainHarness`、`Scripted*Handler`、`ScriptedSubagentSpawner` 等现有 testkit 能力。
- 站在 `Requirement` / `RequirementResult` effect 边界 mock LLM、tool、interaction、subagent、reconfig。
- 不 mock provider HTTP/SSE wire format,不依赖真实网络、credentials、真实时间或真实工具后端。
- `plan` / `blackboard` 当前在设计文档中是垂直 API 概念,代码里只有 `PlanId` / `BlackboardId` 等 identity。复杂测试先把它们作为 mock vertical feature/tool adapter 表达;等正式 API 落地后,同一批场景改用真实 API store。

## 2. 测试文件建议

建议新增一组集成测试,先放在 root `tests/` 下,因为它们验证 `agent-lib` 的公开 agent 行为,同时通过 dev-dependency 使用 `agent-testkit`:

| 文件 | 目的 |
|---|---|
| `tests/agent_complex_flow.rs` | 主组合场景:多轮 + approval approve/deny + plan/blackboard + pivot。 |
| `tests/agent_complex_subagent.rs` | subagent 创建/执行与 plan/blackboard、cancel 传播组合。 |
| `tests/agent_complex_cancel.rs` | cancel 在 tool、approval、subagent 等不同阻塞点的 never-resume 语义。 |

如果希望先保持更小变更,也可以先只建 `tests/agent_complex_flow.rs`,内部用 3 个测试覆盖下文 P0 场景。不要把所有行为塞进一个超长测试;每个测试应有一个主断言目标,但场景里同时经过多个机制。

## 3. Mock 组件

### 3.1 LLM 脚本

用 `ScriptedLlmHandler` 表达多轮:

| 阶段 | LLM 返回 |
|---|---|
| 第 1 次 | tool_use: `plan_create` 或 `plan_add_task`,task 可带 `depends_on` 依赖数组。 |
| 第 2 次 | tool_use: `blackboard_post` + 一个需要 approval 的危险 tool。 |
| 第 3 次 | 根据 tool deny/error 继续恢复,返回另一个安全 tool_use 或 final text。 |
| 第 4 次 | final assistant text。 |

关键断言:

- LLM call count 与脚本步数匹配。
- 第二次及之后的 `ChatRequest.messages` 包含前序 tool result、denied/cancelled result、pivot user message。
- 脚本耗尽必须是测试失败,不能被错误地当成正常 final text。

### 3.2 Tool mock

提供最小 `MockPlanBlackboardStore`,再由 `ScriptedToolRegistry` 或轻量 `ToolHandler` 包装成 tool:

| Tool | 行为 |
|---|---|
| `plan_create` | 创建 plan,返回 plan id 与版本。 |
| `plan_add_task` | 添加 plan item,可声明 `depends_on` 一个或多个前置 item。 |
| `plan_claim` | CAS 认领指定 item;必须检查依赖已完成,否则返回 tool error。 |
| `plan_claim_first_available` | 按稳定顺序认领第一个依赖已完成、未完成、未被认领的 item。 |
| `plan_update` | 更新 task 状态;检查版本。 |
| `blackboard_post` | append-only 写消息,返回 offset。 |
| `blackboard_read` | 按 cursor 读消息。 |
| `dangerous_write` | 需要 approval 的高危 tool。 |
| `safe_read` | auto-allow 的普通 tool。 |

`plan` 和 `blackboard` 的 mock store 要记录完整操作日志,用于断言顺序和原子性。不要把 store 做成复杂框架;首版只需要内存结构和几个固定返回。

### 3.3 Interaction mock

用 `ScriptedInteractionHandler::sequence` 覆盖 approve、deny、cancel:

| 场景 | 决策序列 |
|---|---|
| approval mixed | `Approve` 后 `Deny(Some("policy"))`。 |
| cancel while approval | `Cancel(Some("user aborted"))` 或在 requirement 暴露后直接 cancel context。 |
| subagent pop | child headless,interaction pop 到 parent,由 parent sequence 决策。 |

关键断言:

- 被 deny 的 tool 不执行,但 conversation 中有对应 tool result/status。
- 被 approve 的 tool 正常执行且 call log 只有一次。
- approval response 必须地址正确,不能错 step/call。

### 3.4 Subagent mock

使用 `ScriptedSubagentSpawner` + `DrivingSubagentHandler`:

| 子 agent 类型 | 用途 |
|---|---|
| headless worker | 自己无 interaction handler,需要时 pop 到 parent。 |
| attended worker | 自己有 interaction handler,验证不会误 pop 到 parent。 |
| cancel-sensitive worker | 第一步 emit `NeedLlm` 或 `NeedTool`,父 cancel 后必须 abandon,handler 不应被调用。 |

关键断言:

- `ids_calls`、`spawn_calls`、`summarize_calls` 符合预期。
- 子 agent budget charge 计入父 `RunContext`。
- 父 cancel 传播到子,子 outstanding requirement never-resume。
- trace 中 subagent requirement 的 disposition/resolved scope 可观察。

### 3.5 Pivot 注入

用 `StepHarness::pivot` 手动卡在 post-tool step boundary 后注入用户消息。不要只用 `DrainHarness`,因为 pivot 的合法落点需要观察中间 requirement。

关键断言:

- pivot 只在合法 streaming-step/post-tool 边界成功。
- pivot re-emits 同一个 LLM requirement family,并刷新 request messages。
- pending turn 中 role 顺序为 user -> assistant(tool_use) -> tool -> user(pivot) -> assistant(final)。
- 非法时机的 pivot 另由已有基础测试覆盖,复杂场景只测合法边界与后续 tool/approval/subagent 的组合。

## 4. P0 组合场景

### 4.1 多轮 + plan/blackboard + approve/deny + pivot

建议测试名:`complex_turn_combines_plan_blackboard_approval_deny_and_pivot`。

流程:

1. 用户输入“实现功能 A”。
2. LLM 第 1 次返回 `plan_create` + `plan_add_task` tool_use,创建 `design` 与依赖 `design` 的 `implement` 两个 item。
3. Tool mock 创建 plan,追加带 `depends_on` 的 item,返回 ok。
4. LLM 第 2 次返回 `blackboard_post` + `dangerous_write` tool_use,其中 `dangerous_write` 需要 approval。
5. Interaction 第 1 次 `Approve`,`dangerous_write` 执行成功。
6. 在 tool result 后、下一次 LLM resume 前,用 `StepHarness::pivot("先不要改文件,只给方案")` 注入 pivot。
7. LLM 第 3 次看到 pivot 后改向,返回另一个 `dangerous_write` tool_use。
8. Interaction 第 2 次 `Deny(Some("pivot changed scope"))`,该 tool 不执行,机器把 deny 折叠成 tool result。
9. LLM 第 4 次返回 final text,说明已更新 plan/blackboard,未执行第二个危险写入。

主断言:

- committed turn 数为 1,无 pending。
- message role 序列包含 pivot user message,且出现在第一次 tool result 后。
- plan store 中 `implement.depends_on == [design]`;`design` 可被认领,但 `implement` 在 `design` 完成前不可认领。
- plan store 中 task 状态从 `todo` 到 `in_progress` 或 `blocked`,没有被第二个危险写入误改成 `done`。
- blackboard 是 append-only,至少包含“开始处理”和“pivot 后改变策略”两条消息,offset 单调。
- `dangerous_write` tool log 只执行一次;第二次 approval deny 后没有 tool execution。
- interaction log 为 approve、deny 两次,顺序稳定。
- LLM log 为 4 次,最后一次 request 能看到 deny/cancelled tool result 与 pivot message。

覆盖 corner case:

- 同一 turn 内 pivot 与 approval deny 同时存在。
- deny 后仍能继续下一轮 LLM,不会把 turn 直接失败。
- tool result 后插入 user pivot 不破坏 tool pairing。
- plan/blackboard side effect 发生在 tool 层,不会被 LLM 重试或 deny 路径重复执行。

### 4.2 subagent 创建/执行 + parent approval + shared plan/blackboard

建议测试名:`complex_subagent_updates_shared_plan_and_pops_approval_to_parent`。

流程:

1. 父 agent 收到用户任务,LLM 返回 `spawn_reviewer` 或直接由 `ScriptMachine` emit `NeedSubagent`。
2. `ScriptedSubagentSpawner` 创建 headless child,child opening brief 中包含 plan id。
3. Child 第 1 轮 LLM 读取 plan,调用 `plan_claim_first_available`,claim 到第一个依赖已完成的 task,post blackboard “review started”。
4. Child 请求一个需要 approval 的 tool,但 child scope 没有 interaction handler。
5. `NeedInteraction` pop 到 parent scope,parent interaction handler `Approve`。
6. Child tool 执行成功,更新 plan task 为 `done`,post blackboard “review done”。
7. Subagent summarize 返回 “review complete”,父 agent resume `NeedSubagent` 后 final。

主断言:

- `spawn_calls == 1`,`summarize_calls == 1`。
- parent interaction log 收到 child 的 approval,child interaction log 为空。
- plan store 中 task claim 只发生一次,owner 是 child agent id 或测试指定 worker id。
- `plan_claim_first_available` 不会返回依赖未完成的 task;若前置未完成,该 task 保持 `todo` 且无 owner。
- blackboard 消息按 started -> done 顺序 append,且 sender 可区分 parent/child。
- 子 agent token/budget charge 反映在父 `RunContext`。
- trace 记录 child requirement resolved at parent scope,subagent requirement resumed。

覆盖 corner case:

- headless child 的 interaction 正确向上 pop,不会被 subagent handler 自己吞回造成环。
- 子 agent 与父共享 plan/blackboard side effect,但 budget/cancel/trace 从父派生。
- subagent summary 只在 child drain 完成后产生。

### 4.3 cancel during subagent/tool wait + never-resume + no extra side effects

建议测试名:`complex_cancel_abandons_child_and_preserves_committed_state`。

流程:

1. 父 agent 创建 child,child 已 claim plan task 并 post “started”。
2. Child emit `NeedTool` 或 `NeedLlm`,handler 用 `Barrier`/`Delay` 卡住。
3. 父 `RunContext.cancellation().cancel()`。
4. Driver 走 cancel path,abandon child outstanding requirement。
5. Child drain 结束后 summarize 为 “cancelled” 或返回可识别 subagent output。
6. 父 agent 后续可接受新 user turn,把 plan task 标记为 `blocked/cancelled`,post blackboard “cancelled”。

主断言:

- 被卡住的 child handler 没有 complete 或根本没有被调用,取决于 cancel 注入时机。
- child log `abandon_count == 1`,`resume_count == 0`。
- plan store 没有出现 `done` 状态,只允许 `in_progress -> cancelled/blocked`。
- blackboard started 已提交,cancelled 追加在后,没有重复 started。
- conversation cancel 后无 pending,且下一轮 user turn 可正常 commit。
- trace disposition 为 `NeverResumed` 或等价的 cancel disposition。

覆盖 corner case:

- cancel 是 never-resume,不是 wrong-family error,也不是静默丢状态。
- 子 agent 已发生的 side effect 不回滚,未发生的 side effect 不应补跑。
- cancel 后同一 agent 可继续多轮会话。

## 5. P1 组合场景

### 5.1 plan claim 冲突/依赖阻塞 + blackboard 通知 + LLM 恢复

建议测试名:`complex_plan_claim_conflict_or_dependency_block_recovers_through_blackboard`。

流程:

1. 父创建两个 child 或同一 turn 中两个 worker 都尝试 claim 同一 task,或尝试 claim 一个前置未完成的 task。
2. Mock plan store 让第一个 claim 成功,第二个返回 version conflict;对前置未完成的 claim 返回 dependency-blocked error。
3. 第二个 child post blackboard “claim conflict” 或 “dependency blocked”,LLM 恢复后调用 `plan_claim_first_available` 选择另一个可用 task。

断言:

- 同一 task 只有一个 owner。
- conflict / dependency-blocked 被作为 tool error/result 反馈给模型,不是 panic。
- `plan_claim_first_available` 跳过依赖未完成的 task,返回第一个可认领 item 或 `NoAvailableItem`。
- blackboard conflict 消息 append-only 保留。

### 5.2 approval cancel 与 context cancel 的区别

建议测试名:`complex_approval_cancel_does_not_cancel_context_unless_driver_cancels`。

流程:

1. Tool approval 返回 `InteractionDecision::Cancel(Some("not now"))`。
2. 机器把该 tool call 折叠成 cancelled/denied tool result,继续 LLM。
3. `RunContext` 未 cancel,后续安全 tool 仍可执行。
4. 另一个测试中真正调用 `ctx.cancellation().cancel()`,后续 requirement abandon。

断言:

- approval cancel 只取消单个 tool call。
- context cancel 取消整个 in-flight continuation。
- 两种 cancel 在 trace/handler log 中可区分。

### 5.3 pivot 后触发 subagent,subagent 不读取旧 request

建议测试名:`complex_pivot_then_subagent_uses_rerendered_brief`。

流程:

1. 首轮 LLM tool result 后 pivot 注入“改由 reviewer 子 agent 处理”。
2. 重新渲染后的 LLM request 返回 `NeedSubagent` 或 tool 化 `spawn_reviewer`。
3. 子 agent opening brief 必须包含 pivot 后的新目标,不能只包含 pivot 前旧目标。

断言:

- LLM 第 2 次 request 包含 pivot。
- subagent brief/opening message 包含 pivot 文本。
- 旧目标没有被执行危险 tool。

## 6. 必要断言清单

每个复杂测试至少覆盖以下三类观察面中的两个,主 P0 场景尽量覆盖全部:

| 观察面 | 断言 |
|---|---|
| Conversation | committed turn count、无 pending、role 序列、tool result status、pivot user message 位置。 |
| Handler logs | LLM/tool/interaction/subagent call count、dispatch order、completion order、dangerous tool 是否未调用。 |
| Mock stores | plan 版本/状态/owner/depends_on、claim 前置检查、claim-first 选择、blackboard offset/sender/content、无重复 side effect。 |
| Trace/Budget | resolved_at_scope、resume/never-resume disposition、child budget 计入 parent。 |
| Cursor | final `LoopCursorKind::Done`,cancel 后新 turn 可继续。 |

失败信息要求:

- 对脚本耗尽,错误应包含 family 与 call index。
- 对 plan/blackboard store mismatch,错误应打印操作日志。
- 对 conversation mismatch,错误应打印 role sequence 与 tool result status。
- 对 cancel mismatch,错误应打印 outstanding requirement ids 与 child log。

## 7. 测试推进方式

### 7.1 使用 `StepHarness` 的场景

需要精确插入 pivot 或在 requirement 暴露后 cancel 时,使用 `StepHarness`:

1. `harness.user(...)` 打开 turn,拿到 `NeedLlm`。
2. 手动 resume LLM tool_use。
3. 手动 resume tool 或 approval。
4. 在 post-tool boundary 调 `harness.pivot(...)`。
5. 继续 resume re-emitted LLM requirement。

适用场景:

- 4.1 的 pivot 注入。
- 5.3 的 pivot 后 subagent brief。
- 对 “approval 已暴露但尚未决策” 的取消时机断言。

### 7.2 使用 `DrainHarness` 的场景

只需要验证完整 drain 结果时,使用 `DrainHarness`:

1. 组装 `TestScope`。
2. 注册 watched logs。
3. `run_user` 到终态。
4. 断言 final conversation、store log、trace/budget。

适用场景:

- 4.2 subagent pop 到 parent。
- 4.3 cancel 已通过 `CancelOnCall`/`Barrier` 稳定触发。
- 5.1 plan claim conflict / dependency block。

## 8. Plan/Blackboard Mock Store 草案

首版测试内联一个最小 store,后续若多处复用再提到 `agent-testkit`。

数据结构:

```rust
struct MockPlanBlackboardStore {
    plan: Mutex<PlanState>,
    board: Mutex<Vec<BoardMessage>>,
    ops: Mutex<Vec<StoreOp>>,
}

struct PlanState {
    id: PlanId,
    version: u64,
    task_order: Vec<String>,
    tasks: BTreeMap<String, TaskState>,
}

struct TaskState {
    status: TaskStatus,
    owner: Option<String>,
    depends_on: Vec<String>,
}

struct BoardMessage {
    offset: u64,
    sender: String,
    text: String,
}
```

约束:

- plan update 每次递增 version。
- 添加 task 时校验 `depends_on`:引用已知 task,不得自依赖,不得形成环。
- claim 必须检查 expected version 或 expected owner,冲突返回 tool error。
- claim 必须检查所有 `depends_on` item 都已 `completed`;前置未完成时返回 dependency-blocked tool error,且不修改状态。
- `claim_first_available` 按 `task_order` 跳过已完成、已有 owner、依赖未完成的 item,原子认领第一个可用 item;没有可用项时返回 `NoAvailableItem` tool error。
- blackboard 只 append,不提供 delete/update。
- 所有操作写入 `ops`,测试失败时打印。

## 9. 落地顺序

建议按风险从高到低落地:

1. P0-1:多轮 + plan/blackboard + approve/deny + pivot。它一次覆盖最多当前缺口,但不引入 subagent。
2. P0-2:subagent + parent approval pop + shared store。验证动态作用域和共享 side effect。
3. P0-3:cancel during subagent/tool wait。验证 never-resume 与 cancel 后可继续。
4. P1-2:approval cancel vs context cancel。把两个容易混淆的 cancel 语义钉住。
5. P1-1/P1-3:claim conflict / dependency-blocked 与 pivot 后 subagent brief。作为回归补强。

每一步完成标准:

- 聚焦测试可单独运行。
- 不依赖真实 sleep,并发/等待使用 `Barrier`、`Delay` 或 testkit cancellation wrapper。
- `cargo test --test <file> <test_name>` 通过。
- 复杂 mock 的 helper 若超过单文件可读范围,再提取到 `tests/support` 或 `agent-testkit`,不要提前抽 DSL。

## 10. 非目标与风险

非目标:

- 不在这组测试中验证 provider request JSON、SSE、HTTP 错误分类。
- 不把 plan/blackboard 做成生产实现。
- 不引入新的稳定 scenario DSL;现有 `scenario` module 仍是 spike。
- 不用真实 wall-clock sleep 证明 cancel 或并发。

风险:

- 如果一个测试组合过多机制,失败定位会变差。控制方式是 P0 拆成 3 个主场景,每个有明确主断言。
- plan/blackboard API 未落地,测试 mock 的形状可能需要随真实 API 调整。控制方式是把 mock store 做得薄,只断言设计层不变量:plan CAS/状态/依赖、claim-first、blackboard append-only。
- `StepHarness` 与 `DrainHarness` 混用可能造成样板增加。控制方式是只有 pivot/cancel 时机需要手动 step,其余使用 drain。
