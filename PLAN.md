# 实施计划：Agent Testability 与 `agent-testkit`

> 本计划以 [`docs/TESTABILITY.md`](docs/TESTABILITY.md) 为规范性设计输入。它接续已完成的
> Agent Effect Model 迁移,把现有散落在各测试文件中的 fake、fixture、script machine、scope
> wrapper 与断言逻辑收敛成一套 dev-only 测试基础设施。
>
> 已完成的 Agent Effect Model 迁移计划和任务记录已归档到
> [`docs/archive/2026-07-14-agent-effect-migration/`](docs/archive/2026-07-14-agent-effect-migration/)。
> 已完成的 Client、Conversation、旧 Agent Layer 记录分别在 `docs/archive/2026-07-13-*` 下。
> 逐任务要求见 [`TODO.md`](TODO.md)。

## 范围与非目标

**范围**:为 agent effect 层建立可复用测试基础设施,包括 deterministic id source、provider-neutral
fixtures、scripted handlers、cassette 录制/重放、scope builder、script machine、step/drain harness、
断言 helper、并发/取消测试工具,并据此组织一组基础 Rust suites、脚本化 scenario suites 与
recorded replay suites。

**非目标**:不 mock HTTP provider;不替代 adapter/client/stream 协议层测试;不在首版做 Node/NAPI;
不把所有底层单测改写到 testkit;不拆出 trait crate;不改变 `agent-lib` 运行时 API 语义;不引入
真实时间 sleep、真实网络或 credentials 作为默认测试条件。

## 规范优先级与关键决策

1. **测试分层固定**:协议层测 HTTP/SSE/provider JSON;agent 层测 `Requirement` emit、handler
   wiring、resume/abandon、conversation/trace/budget 终态;应用层/未来 TS 测复杂编排场景。
2. **effect 边界 mock**:`agent-testkit` 直接实现 `LlmHandler`、`ToolHandler`、`InteractionHandler`、
   `SubagentHandler`、`ReconfigHandler` 等公开 trait,不模拟 provider wire format。
3. **短期不拆 trait crate**:`agent-testkit` 直接依赖 `agent-lib`。若未来要拆,应拆为承载 DTO、错误和
   trait 的 `agent-core`/`agent-api`,而不是只抽一个薄 traits crate。
4. **首版优先 Rust**:基础行为与简单组合用 Rust + testkit 覆盖;复杂/真实场景先用 scripted/cassette;
   JS/TS/NAPI 等 Rust scenario model 稳定后再接。
5. **cassette 是 provider-neutral fixture**:录制 `ChatRequest`/`Response`、`ToolCall`/`ToolResponse`、
   `Interaction`/`InteractionResponse` 等 effect req/resp,不录制 headers、auth、base URL 或 provider raw body。
6. **cassette 默认安全**:record/update 必须显式 opt-in;writer 必须经过 redactor;replay 在 CI 离线可跑。
7. **testkit 不掩盖不变量**:`TestScope` 不默认兜底所有 handler;顶层缺 handler 仍应暴露
   `UnhandledRequirement`;handler 常规错误必须留在同 family 的 `Err` 中。
8. **完成门一致**:每个任务按 format → clippy → 聚焦测试 → 全量测试 → rustdoc → diff check 验证。

## 里程碑总览

| 里程碑 | 目标 | 主要产出 |
|---|---|---|
| **M1 Testkit 骨架与基础数据** | 建立 dev-only crate / 支持模块、id source 与 fixtures | `agent-testkit` skeleton、`SeqIds`、fixtures、prelude |
| **M2 Scripted handlers 与 scope** | 把现有 fake 收敛成可脚本化 handlers 和 scope builder | script model、call log、scripted handlers、`TestScope`、`ScriptMachine` |
| **M3 Cassette 录制/重放** | 记录真实 effect req/resp,并在 CI 离线重放 | cassette schema、redactor、fingerprint、replay handlers、record/update wrapper |
| **M4 Harness 与断言库** | 降低 step/drain 测试样板,统一状态断言 | `StepHarness`、`DrainHarness`、conversation/notification/trace/budget assertions |
| **M5 并发、取消与 subagent 工具** | 稳定表达乱序、并发峰值、取消时机和子 agent scope | delay/barrier/peak、cancel-on-call、panic-on-call、scripted subagent spawner |
| **M6 测试套件迁移与扩展** | 用 testkit 组织基础 Rust suites、scenario suites、recorded replay suites | 迁移 e2e/reference fake、补基础 coverage、离线 cassette replay 测试 |
| **M7 Scenario DSL 与文档并轨** | 为未来 TS/NAPI 保留数据化 scenario 入口,完成文档和示例 | scenario model 草案、runner spike、README/docs 更新、总 Review |

依赖顺序固定:M1 → M2 → M3 → M4 → M5 → M6 → M7。每个里程碑末尾必须有独立 Review 任务。

## 建议目录与依赖形状

首选 crate 形态:

```text
crates/agent-testkit/
  Cargo.toml
  src/
    lib.rs
    ids.rs
    fixtures.rs
    script.rs
    handlers.rs
    cassette.rs
    scope.rs
    machine.rs
    harness.rs
    assertions.rs
    concurrency.rs
    subagent.rs
    prelude.rs
  tests/
    smoke.rs
```

推荐依赖:

```text
agent-lib                 # 当前核心库:公开 trait + 默认机器 + reference driver
agent-testkit --dep--> agent-lib

agent-lib tests --dev-dep--> agent-testkit   # 若 Cargo 拓扑可接受
或:
agent-testkit/tests/ 同时依赖 agent-testkit 和 agent-lib
```

如果 root package + workspace 共存带来不必要复杂度,允许先以 `tests/support/agent_testkit` 过渡,但
M1 Review 必须记录最终选择与理由。

## 测试套件策略

**Core Rust suites** 快速、细粒度、默认离线:

| 套件 | 目标 |
|---|---|
| `agent_step_basic` | user -> NeedLlm、resume text、wrong id/kind、abandon |
| `agent_tool_basic` | single/parallel tool、tool error、step limit、provider call mismatch |
| `agent_interaction_basic` | approve/deny/timeout/cancel、wrong call/step rejection |
| `agent_pivot_basic` | post-tool pivot 成功与非法边界拒绝 |
| `agent_reconfig_basic` | idle/during-turn reconfig、registry effect、atomic reject |
| `agent_driver_basic` | local handler、pop、top unhandled、misaligned result |
| `agent_cancel_basic` | never-resume, cancel 后新 turn 可继续 |
| `agent_trace_budget_basic` | resolved_at_scope、disposition、budget 共享 |

**Scripted scenario suites** 覆盖复杂组合:

- 多轮 tool loop。
- auto tool + guarded tool 混合。
- child headless interaction pop 到 parent。
- tool batch out-of-order 与 cancel timing。
- 多 queued reconfig 与 registry swap。
- subagent depth/budget/cancel 组合。

**Recorded replay suites** 复用真实 req/resp:

- recorded text turn。
- recorded tool-use round trip。
- recorded approval flow。
- recorded reconfig / registry swap。
- 历史 bug 的真实 flow 固化。

**Future DSL/TS suites** 等 scenario model 稳定后再接,先支持 JSON runner,再考虑 NAPI。

## Cassette 边界

录制内容:

- schema version、crate version、test name、说明。
- effect family、调用序号、normalized request、request fingerprint。
- normalized result、review summary。
- 可选 final cursor、notifications summary、conversation summary、trace requirement disposition。

不录制:

- HTTP headers、auth token、endpoint、provider raw response body。
- live client/registry/callback/runtime handle。
- 未脱敏敏感输入。
- wall-clock timing,除非显式作为测试输入。

默认匹配策略:

- family + 顺序 + request fingerprint。
- 忽略 volatile ids,包括 `RequirementId`、`TraceNodeId` 和测试运行分配的 host ids。
- `ChatRequest` fingerprint 包含 model/system/messages/tools/max_tokens/temperature/provider extras 的脱敏 canonical 形状。

## 验证与完成门

每项任务至少执行:

- `cargo fmt --all`
- `cargo clippy --all-targets -- -D warnings`
- 相关聚焦测试
- `cargo test --all --all-targets`
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`
- `git diff --check`

若任务只改文档,至少执行 `git diff --check`,并说明未运行 Rust 构建的理由。新增测试不得依赖网络、
credentials 或真实 sleep。record/update cassette 必须默认 skipped 或显式 opt-in。

## 每阶段 Review

每个 `Mx-R` 必须核对:

- 是否仍只 mock agent effect 边界,没有把 provider wire format 混入 testkit。
- testkit 是否只依赖公开 API,没有绕过 `agent-lib` 不变量。
- 是否保留 `UnhandledRequirement`、misaligned result、cancel never-resume 等负例覆盖。
- cassette 是否可审阅、可脱敏、CI 离线可跑。
- 新 helper 是否减少样板而不是掩盖行为。
- 文档、README、计划与实际代码是否一致。
