# 实施计划：复杂 Mock 测试与 Plan 依赖语义

> 本计划以 [`docs/complex-tests.md`](docs/complex-tests.md) 为主要设计输入,并引用
> [`docs/agent-layer.md`](docs/agent-layer.md) §6.2 的 plan API 语义。上一轮 Agent Testability 与
> `agent-testkit` 计划已完成并归档到
> [`docs/archive/2026-07-15-agent-testability/`](docs/archive/2026-07-15-agent-testability/)。
> 逐任务要求见 [`TODO.md`](TODO.md)。

## 目标

在现有 `agent-testkit` 基础设施之上,新增一组高价值复杂 mock 测试,覆盖这些机制的组合边界:

- 多轮会话与多次 LLM/tool/interaction 往返。
- tool call approval approve / deny / cancel。
- subagent 创建、执行、summary、scope pop、budget/cancel 传播。
- plan / blackboard mock vertical feature,包含 plan item dependency、claim 前置检查和 claim-first 入口。
- cancel never-resume 与 cancel 后继续新 turn。
- pivot message 在合法 step boundary 注入并影响后续 request/subagent brief。

本计划不是重写 testkit,而是用 testkit 写复杂场景,把实际 agent 层容易漏的 corner case 固化为离线测试。

## 范围与非目标

**范围**:

- 新增复杂测试支持模块,提供内存 `MockPlanBlackboardStore`、plan/blackboard/tool adapter、危险 tool、审批 policy 与断言 helper。
- 新增 root integration tests,优先覆盖 P0 场景,再补 P1 回归场景。
- 所有 mock 站在 agent effect 边界,使用 `DefaultAgentMachine`、`StepHarness`、`DrainHarness`、`Scripted*Handler`、`ScriptedSubagentSpawner` 等现有工具。
- plan/blackboard 当前仍是测试用 mock vertical feature;未来正式 API 落地后,这些场景应迁移到真实 store/API。

**非目标**:

- 不实现生产级 plan/blackboard API。
- 不 mock Anthropic/OpenAI HTTP、SSE、provider raw JSON。
- 不引入新的 scenario DSL 或 Node/NAPI runner。
- 不用真实 wall-clock sleep、网络、credentials 或外部工具。
- 不改变 `agent-lib` 运行时 API 语义。

## 关键语义

### Plan

- 每个 plan item 有稳定 id、status、owner、`depends_on: Vec<TaskId>`。
- `depends_on` 必须引用已知 item,不得自依赖,不得形成环。
- `plan_claim` 必须原子检查版本/owner/status/dependencies。任一前置 item 未 `completed` 时返回 dependency-blocked tool error,且不得修改 owner/status/version。
- `plan_claim_first_available` 按 plan 的稳定创建/显示顺序扫描 item,跳过已完成、已有 owner、依赖未完成的 item,原子认领第一个可用 item。没有可用项时返回 `NoAvailableItem` tool error。
- `plan_update` 每次成功更新递增 version,并记录操作日志。

### Blackboard

- append-only,只追加消息,不删除、不更新。
- 每条消息记录 offset、sender、text。
- offset 单调递增,可按 cursor 读取。
- blackboard 只做广播/讨论,不承担 claim、lock 或 exactly-once 语义。

### Approval 与 Dangerous Tool

- dangerous tool 必须走 `ToolApprovalPolicy` 触发 `NeedInteraction`。
- approve 后 tool 执行一次。
- deny/cancel 后 tool 不执行,机器应合成对应 tool result 并继续下一轮 LLM。
- approval cancel 只取消单个 tool call;`RunContext` cancel 才是整个 continuation 的 never-resume。

### Pivot

- 复杂测试只覆盖合法 pivot:在 tool result 后、下一次 LLM resume 前通过 `StepHarness::pivot` 注入。
- pivot 应重渲染 outstanding LLM request,后续 request/messages/subagent brief 必须能看到 pivot 文本。
- pivot 不应破坏 tool pairing,最终 committed turn 无 pending。

### Subagent

- headless child 无 interaction handler 时,approval/interaction 必须 pop 到 parent scope。
- child context 由 parent 派生,budget 计入 parent,cancel 从 parent 传播到 child。
- subagent summary 只在 child drain 完成或受控 cancel 收尾后产生。

## 里程碑

| 里程碑 | 目标 | 主要产出 |
|---|---|---|
| **M1 Support 与 mock vertical features** | 建立复杂测试共用支持层 | `tests/complex_support/`、mock plan/blackboard store、tool adapter、approval policy、断言 helper |
| **M2 主复杂 flow** | 覆盖多轮 + plan/blackboard + approve/deny + pivot | `tests/agent_complex_flow.rs` P0 主场景 |
| **M3 Subagent 与 cancel 组合** | 覆盖 subagent pop/shared store 与 never-resume cancel | `tests/agent_complex_subagent.rs`、`tests/agent_complex_cancel.rs` |
| **M4 P1 回归补强与文档并轨** | 固化 claim conflict/dependency block、approval cancel、pivot 后 subagent brief | P1 tests、文档更新、总 review |

依赖顺序固定:M1 → M2 → M3 → M4。每个阶段末尾必须有独立 Review 任务。

## 建议文件结构

```text
tests/
  complex_support/
    mod.rs
    plan_blackboard.rs
    tools.rs
    assertions.rs
  agent_complex_support.rs
  agent_complex_flow.rs
  agent_complex_subagent.rs
  agent_complex_cancel.rs
```

每个 integration test 使用:

```rust
#[path = "complex_support/mod.rs"]
mod complex_support;
```

如果首版 helper 很少,允许先放在 `tests/agent_complex_flow.rs` 内部;但一旦 M2/M3 需要复用,必须提取到
`tests/complex_support/`,不要复制多份 store/tool adapter。

## 验证门

每个实现任务至少执行:

- `cargo fmt --all -- --check`
- `cargo clippy --all-targets -- -D warnings`
- 相关聚焦测试,例如 `cargo test --test agent_complex_flow`
- `cargo test --all --all-targets`
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`
- `git diff --check`

若任务只改文档,至少执行 `git diff --check`,并说明未运行 Rust 构建的理由。

## Review 标准

每个 `Mx-R` 必须核对:

- 测试是否仍只 mock agent effect 边界,没有引入 provider wire mock。
- plan dependency、claim 前置检查和 claim-first 是否按设计执行且失败原子。
- approval deny/cancel 是否不执行 dangerous tool。
- pivot 是否只在合法边界注入,并影响后续 request。
- subagent interaction pop、budget/cancel 传播是否可观察。
- cancel 是否是 never-resume,且 cancel 后 agent/conversation 可继续使用。
- 新 helper 是否提升可读性,没有过早抽象成 DSL。
