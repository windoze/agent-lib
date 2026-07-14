# Testability 计划 —— Agent Effect 测试增强

> 状态:规划稿。本文记录 `agent-lib` 在 agent effect 模型落地后的测试增强计划。
> 目标不是重写已有测试,而是把现有散落的 fake、fixture、handler 组合与断言能力收敛成一套
> 可复用的测试基础设施,让高层 agent 场景能用脚本化 effect 回放表达,避免每个用例都手写一套
> Rust mock。

## 0. 一句话

**保留 Rust 底层不变量测试,新增 dev-only `agent-testkit` 做 agent 层脚本化 effect 测试;协议层仍测协议,agent 层直接 mock `Requirement` 的兑现边界。**

## 1. 背景

agent 层已经迁移为 sans-io + effect-handler 形状:

- `AgentMachine::step` 同步推进状态,只吐 `Notification` 与 `Requirement`。
- `drain` / `HandlerScope` / `Pop` 负责兑现 `NeedLlm`、`NeedTool`、`NeedInteraction`、`NeedSubagent`、`NeedReconfigRegistry`。
- cancel 是 `StepInput::Abandon` 的 never-resume;approval / pivot / reconfig 也都归到同一套 step + requirement 机制。

这使 agent 层天然适合 mock:测试不用起 HTTP server、不用模拟 provider wire format,只要脚本化 handler 对每个 effect 的返回即可。

现状的问题不是不能测,而是测试代码正在膨胀:

- `FakeClient`、`FakeToolRegistry`、`ScriptedRequirementIds`、`FakeToolIds`、scope wrapper 在多个测试文件里重复出现。
- 复杂场景需要大量样板:构造 spec/state/input/response/tool ids,再手动断言 committed conversation、notifications、trace、budget。
- `drain` 一口气跑到 terminal 很适合 e2e,但缺一个可手动逐步推进的 harness 来检查每个 requirement 与 resume/abandon 时机。
- 并发、取消、pop 路由、subagent scope 这类行为已能测,但缺统一的 barrier/delay/call-log 工具。

## 2. 分层原则

### 2.1 协议层测试

协议层测试仍留在 adapter/client/stream 相关模块,覆盖:

- HTTP request/response shape。
- SSE event sequence。
- provider 原始 JSON 解析。
- headers、auth、retry-after、错误码分类。
- streaming fold 到 provider-neutral `Response`。

这些测试可以使用 recorded fixture 或 transport mock,但不属于 `agent-testkit` 的职责。

### 2.2 Agent 层测试

agent 层测试直接站在 effect 边界:

- `NeedLlm` 由 `LlmHandler` 返回 provider-neutral `Response` 或 `ClientError`。
- `NeedTool` 由 `ToolHandler` / `ToolRegistry` 返回 `ToolResponse` 或 `ToolRuntimeError`。
- `NeedInteraction` 由 `InteractionHandler` 返回 `InteractionResponse`。
- `NeedSubagent` 由 `SubagentSpawner` / `SubagentHandler` 构造子机器和子 scope。
- `NeedReconfigRegistry` 由 `ReconfigHandler` 返回 registry swap 成功或失败。

agent 层不关心 Anthropic/OpenAI wire format,只关心 effect 是否被正确 emit、handler wiring 是否正确、resume/abandon 后状态是否正确。

### 2.3 Rust 与未来 JS/TS 的边界

短期不做 Node/NAPI。先把 Rust testkit 的 API 形状稳定下来。

未来若需要 JS/TS 场景测试,优先复用同一个场景模型:

- Rust testkit 提供 scenario runner。
- TS 生成 JSON scenario 或通过 NAPI 调用 runner。
- 核心 agent 测试仍由 Rust 实现和校验不变量。

## 3. 目标

### 3.1 直接目标

- 降低 agent e2e / driver / subagent 测试的样板代码。
- 让测试按场景脚本表达:LLM 第几次返回什么、tool 第几次返回什么、interaction 如何决策。
- 统一 deterministic id 分配,避免每个测试都手写 UUID 后缀。
- 提供可观察 call log,方便断言 handler 调用次数、请求内容、完成顺序、并发峰值。
- 提供高层断言 helper,减少手写 conversation/notification/trace 检查。

### 3.2 非目标

- 不 mock HTTP provider。
- 不替代 adapter/client/stream 协议测试。
- 不让核心 `agent-lib` 依赖 testkit。
- 不把所有已有低层单测迁到 testkit。
- 不在首版实现完整 DSL、property testing、NAPI 或 TS runner。

## 4. 建议结构

首版可以先作为 repo 内 dev-only crate 或测试支持模块落地。为了避免过早稳定公共 API,建议分两步:

1. 先在仓库内建立 `crates/agent-testkit` 或 `tests/support/agent_testkit`。
2. API 顺手后再决定是否作为正式 dev-dependency crate 暴露。

若采用 crate 形态,推荐:

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
```

`agent-lib` 不依赖 `agent-testkit`;测试目标通过 dev-dependency 使用它。

> **实际落地(M1,2026-07-14)**:采用上面的 crate 形态,`crates/agent-testkit` 作为工作区成员
> (root `Cargo.toml` `[workspace] members=[".","crates/agent-testkit"], resolver="3"`)。testkit 单向
> 依赖 `agent-lib = { path="../.." }`,`agent-lib` 未反向 dev-dep testkit,因此无依赖周期;当前 testkit
> 的集成测试放在自身 `tests/`(如 `smoke.rs`),`agent-lib` 现有测试改用 testkit 属 M6 迁移范围。
> 上面“先建立 `crates/agent-testkit` 或 `tests/support/agent_testkit`”的过渡门已定案为 crate 形态,
> 无需过渡支持模块。

### 4.1 是否需要拆出 trait crate

短期不拆。

原因:

- 当前 `agent-lib` 已公开导出 testkit 需要实现的 trait:`AgentMachine`、`LlmHandler`、`ToolHandler`、`InteractionHandler`、`SubagentHandler`、`ReconfigHandler`、`RequirementIds`、`ToolExecutionIds`、`ToolRegistry` 等。
- `agent-testkit` 可以直接依赖 `agent-lib`,实现这些 trait,测试代码只从 `agent_testkit::prelude` 使用 helper,不需要在每个测试程序里做桥接。
- 单独的“trait crate”不会很薄:这些 trait 的签名引用 `ChatRequest`、`Response`、`ClientError`、`Requirement`、`RequirementResult`、`RunContext`、`LoopCursor`、`Conversation` id、`ToolCall`、`ToolResponse` 等大量 DTO 和错误类型。只抽 trait 不抽这些类型没有意义;抽齐后本质上就是把 `agent-lib` 拆成 `agent-core` + runtime/adapter,这是更大的架构重排。

推荐依赖形状:

```text
agent-lib                 # 当前核心库:公开 trait + 默认机器 + reference driver
agent-testkit --dep--> agent-lib

agent-lib 的测试 --dev-dep--> agent-testkit   # 若 Cargo/dev-dep 拓扑可接受
或:
agent-testkit/tests/ 集成测试同时依赖 agent-testkit 和 agent-lib
```

只有出现下面任一情况时,再考虑拆出真正的 core/API crate:

- 需要多个不同 runtime/implementation crate 共享同一套 agent API。
- `agent-testkit` 必须独立版本化,且不能依赖默认机器/reference driver 所在 crate。
- `agent-lib` 被拆成 `agent-core`、`agent-runtime`、`agent-adapters` 等多 crate 架构。
- Cargo dev-dependency 拓扑在实际落地中造成不可接受的循环或构建问题。
- 编译时间/依赖体积显示 testkit 依赖整个 `agent-lib` 成为现实负担。

若未来要拆,名称应更接近 `agent-core` 或 `agent-api`,而不是只叫 `*-traits`,因为它必须承载 trait 所需的稳定数据类型与错误分类。

## 5. 模块规划

### 5.1 `ids`

提供 deterministic id source,消除测试里的 UUID 噪音。

职责:

- 实现 `RequirementIds`。
- 实现 `ToolExecutionIds`。
- 生成 `RunId`、`TraceNodeId`、`AgentId`、`ToolSetId`、`ConversationId`、`TurnId`、`MessageId`、`ToolCallId`、`StepId`。
- 支持 clone 后共享序列,保证 parent/child/subagent 全局唯一。
- 支持按 tag 记录 id 分配日志,方便断言 requirement family 顺序。

建议 API:

```rust
let ids = SeqIds::new();

let requirement_id = ids.requirement_id();
let run_id = ids.run_id();
let trace_node = ids.trace_node("child");
let input = fixtures::user_input(&ids, "hello");
```

验收:

- 同一 `SeqIds` clone 后分配不冲突。
- `RequirementIds` / `ToolExecutionIds` 的错误路径可脚本化触发。
- 生成的 id 具有稳定字符串形状,便于 golden assertion。

### 5.2 `fixtures`

提供 provider-neutral 数据构造器。

职责:

- `user_message(text)`。
- `user_input(ids, text)`。
- `assistant_text(text, usage)`。
- `assistant_tool_use(calls, usage)`。
- `tool_call(provider_id, name, input)`。
- `tool_response(provider_call_id, text, status)`。
- `usage(input, output)`。
- `weather_tool()`、`calendar_tool()` 等最小 tool declaration。
- `agent_spec(...)`、`agent_state(...)`、`default_machine(...)`。
- `root_context(ids, limits)`。

建议 API:

```rust
let response = assistant_tool_use(
    [tool_call("call-weather", "get_weather", json!({ "city": "Shanghai" }))],
    usage(4, 2),
);
```

验收:

- fixtures 不绕过公开/稳定构造器。
- fixtures 默认值与现有测试语义一致。
- fixtures 只产生 provider-neutral `model` / `client` 数据。

### 5.3 `script`

提供脚本化队列,把“第几次调用返回什么”表达为数据。

职责:

- LLM script:返回 text、tool_use、client error、stream-folded response。
- Tool script:按 call name、provider call id 或调用顺序返回 ok/error/denied/cancelled。
- Interaction script:按 interaction kind、tool call id 或调用顺序返回 approve/deny/timeout/cancel/answer/choice。
- Reconfig script:按 tool set id 返回 ok 或 registry error。
- 支持 call log:请求参数、调用次数、完成顺序。
- 支持严格模式:脚本耗尽时 panic 或返回分类错误。
- 支持从 recorded cassette 装载脚本,把真实运行捕获的 effect req/resp 作为离线 replay 输入。

建议 API:

```rust
let llm = ScriptedLlm::new([
    LlmStep::tool_use([tool_call("call-weather", "get_weather", json!({}))]),
    LlmStep::text("sunny"),
]);

let tools = ScriptedTools::new([
    ToolStep::ok("call-weather", "Sunny"),
]);

let interaction = ScriptedInteractions::approve_all();
```

验收:

- 脚本耗尽的失败信息包含 handler family 与调用序号。
- call log 可断言 request/messages/tools/system。
- 可以表达错误路径,不是只支持 happy path。
- replay 模式下请求不匹配时给出 cassette 名称、step index、expected fingerprint 与 actual fingerprint。

### 5.4 `cassette`

提供真实 req/resp 录制与离线重放能力。

目标:

- 在有网络、有真实工具后端或人工交互后端的环境里跑一次真实场景,记录 agent effect 边界看到的 provider-neutral 请求与结果。
- 在 CI 或本地离线环境中重放这些记录,覆盖比手写 fixture 更接近真实使用的流程。
- 仍然不模拟 HTTP/provider wire format;录制点在 `LlmHandler`、`ToolHandler`、`InteractionHandler`、`ReconfigHandler` 的 effect 边界。

录制内容:

- metadata: cassette schema version、crate version、test name、created_at、可选说明。
- run inputs: root input 摘要、可选 scenario label。
- per-effect entries:
  - requirement family 与调用序号。
  - normalized request payload: `ChatRequest`、`ToolCall`、`Interaction`、`ToolSetRef`。
  - request fingerprint:对稳定字段做 canonical JSON hash。
  - normalized result payload: `Response`、`ToolResponse`、`InteractionResponse`、reconfig ok/error。
  - result summary:便于 review 的 text/tool/status/usage 摘要。
- optional observations:drained notifications 摘要、final cursor、committed conversation 摘要、trace requirement disposition。

不录制内容:

- HTTP headers、auth token、base URL、provider raw response body。
- live tool registry、client handle、runtime callback。
- wall-clock timing,除非显式作为测试输入。
- 未脱敏的 provider-specific extras,除非测试明确 opt-in。

模式:

- `Record`:调用真实 handler,同时写 cassette。
- `Replay`:不调用真实 handler,按 cassette 返回记录结果。
- `Verify`:调用真实 handler,同时与 cassette 做 request/result 对比,用于更新前确认行为漂移。
- `Update`:调用真实 handler 并覆盖 cassette,默认只允许显式环境变量启用。

匹配策略:

- 默认按 family + 顺序 + request fingerprint 匹配。
- 可选按自定义 key 匹配,例如 tool name + provider call id。
- 默认忽略 volatile id: `RequirementId`、`TraceNodeId`、测试运行产生的 host ids 不进入 request fingerprint。
- 对 `ChatRequest` 默认包含 model、system、messages、tools、max_tokens、temperature、provider_extras 的脱敏后 canonical 形状。

脱敏策略:

- cassette writer 必须经过 redactor。
- 默认 redactor 移除 auth/endpoint 类字段,虽然 agent effect 层正常不应出现这些字段。
- 对 `Message` 文本内容默认保留,因为它是 agent 行为测试的核心;涉及敏感数据的真实录制必须在测试侧提供自定义 redactor 或 synthetic prompt。
- provider extras 默认保守处理:未知字段 redact,测试可白名单允许字段。

建议 API:

```rust
let cassette = Cassette::load("tests/cassettes/weather_tool_roundtrip.json")?;

let llm = CassetteLlmHandler::replay(cassette.clone());
let tools = CassetteToolHandler::replay(cassette.clone());

let recorder = CassetteRecorder::record("tests/cassettes/weather_tool_roundtrip.json")
    .with_redactor(DefaultRedactor::new())
    .wrap_llm(real_llm_handler);
```

验收:

- replay 不需要网络、credentials、真实 tool 后端或人工交互。
- cassette 文件是稳定 JSON,适合 code review。
- request mismatch 失败信息指向具体 entry,不是泛泛的“脚本耗尽”。
- `Record` / `Update` 默认不会在 CI 意外覆盖 fixture。
- cassette schema 有版本号;未知未来版本明确失败。

### 5.5 `handlers`

把 scripts 包装成 effect handler。

职责:

- `ScriptedLlmHandler: LlmHandler`。
- `ScriptedToolHandler: ToolHandler`。
- `ScriptedInteractionHandler: InteractionHandler`。
- `ScriptedReconfigHandler: ReconfigHandler`。
- `CassetteLlmHandler` / `CassetteToolHandler` / `CassetteInteractionHandler` / `CassetteReconfigHandler`。
- 可选 `ScriptedToolRegistry: ToolRegistry`,用于需要走 `ToolRegistryHandler` 的 reference tests。
- 可选 `ChargingLlmHandler`,按 response usage 或固定 token 记账。

设计要求:

- handler 返回 `RequirementResult` 的正确 family。
- 错误脚本仍通过对应 family 的 `Err` 返回,不制造 misaligned result。
- misaligned result 另做专门 negative handler,用于测 `drain` 校验。
- handler 不碰 HTTP/provider wire format。

建议 API:

```rust
let llm = ScriptedLlmHandler::from_steps([...]);
let tool = ScriptedToolHandler::from_steps([...]);
let interaction = ScriptedInteractionHandler::approve_all();
```

验收:

- 能替代 `tests/agent_effect_e2e.rs` 里的 `FakeClient` / `FakeToolRegistry` / interaction fake。
- 能替代 `src/agent/drive/reference/tests.rs` 里的重复 fake。
- handler call log 可 clone 后在 run 结束断言。
- cassette-backed handler 与 scripted handler 共享同一套 call log / strict mismatch 报错风格。

### 5.6 `scope`

提供 scope builder,让每个测试不用手写 `impl HandlerScope`。

职责:

- `TestScopeBuilder` 组合 llm/tool/interaction/subagent/reconfig handler。
- 支持 headless scope:省略 interaction。
- 支持 attended scope:挂 interaction。
- 支持 parent/child scope 组合。
- 支持 wrapping `ReferenceScope` 或只使用 testkit handlers。

建议 API:

```rust
let scope = TestScope::builder()
    .llm(llm.clone())
    .tool(tool.clone())
    .interaction(interaction.clone())
    .build();

let headless = TestScope::builder()
    .llm(llm.clone())
    .tool(tool.clone())
    .build();
```

验收:

- 同一个 child machine 可通过不同 scope 体现 attended/headless 差异。
- top scope 缺 handler 时仍走 `UnhandledRequirement`,不被 testkit 默认兜底掩盖。

### 5.7 `machine`

提供小型 scripted machine double,用于测 driver/pop/subagent 机制,不依赖 `DefaultAgentMachine` 内部细节。

职责:

- `ScriptMachine` external input 后吐固定 requirement batch。
- 记录 resume order、resume result tags、abandon count。
- 支持 resume 全部后进入 `Done`。
- 支持 abandon 后进入 `Idle` 或保持测试指定 cursor。
- 支持嵌套测试中的 child machine。

建议 API:

```rust
let machine = ScriptMachine::builder()
    .requirements([need_interaction(ids.requirement_id())])
    .done_after_all_resumed()
    .idle_on_abandon()
    .build();
```

验收:

- 替代 `drive.rs`、`subagent/tests.rs`、`agent_effect_e2e.rs` 里的 ad hoc batch machine。
- 能明确表达 out-of-order resume 是否被接受。

### 5.8 `harness`

提供两类测试驱动。

`DrainHarness`:

- 包装 `drain`。
- 给定 machine、scope、context、input,跑到 turn done。
- 返回 `TurnDone` 和可观察 logs。
- 适合 e2e / reference driver / subagent happy path。

`StepHarness`:

- 手动 `step(External)`。
- 暴露当前 `requirements`。
- 允许按 id `resume`。
- 允许按 id `abandon`。
- 每步累积 notifications。
- 适合检查中间 requirement、pivot、取消时机、错误路径。

建议 API:

```rust
let mut harness = StepHarness::new(machine);
let first = harness.user("hello");
let llm = first.single_llm();
harness.resume(llm.id, RequirementResult::Llm(Ok(response)));
```

验收:

- `StepHarness` 不需要 async。
- `DrainHarness` 不隐藏 `UnhandledRequirement`、trace failure、budget failure。
- harness 的断言失败信息包含当前 cursor 和 outstanding ids。

### 5.9 `assertions`

提供高层断言 helper,降低测试噪音。

职责:

- Conversation 断言:committed turn count、pending 是否存在、message role/text、tool result status、pairing count、open call count。
- Requirement 断言:single llm/tool/interaction/subagent/reconfig、origin path、id tag。
- Notification 断言:tool started/finished、step boundary 顺序、boundary metadata。
- Trace 断言:requirement resolved_at_scope、disposition、subagent parent chain。
- Budget 断言:tokens/steps/cost/wall-clock。
- Handler log 断言:call count、request count、completion order、peak concurrency。

建议 API:

```rust
assert_conversation(machine.state().conversation())
    .committed_turns(1)
    .last_assistant_text("sunny");

assert_trace(&ctx)
    .requirement(requirement_id)
    .resolved_at_scope(1)
    .resumed();
```

验收:

- 断言 helper 不吞掉底层错误上下文。
- helper 只读观察,不修改 machine/context。

### 5.10 `concurrency`

提供并发测试工具。

职责:

- 延迟 handler:按 yield count 或 barrier 延迟完成。
- Peak counter:记录最大并发数。
- Completion log:记录完成顺序。
- Cancel-on-call:某个 handler 被调用时 cancel context。
- Panic-on-call:确保取消路径没有触发不该触发的 handler。

建议 API:

```rust
let tools = ScriptedToolHandler::new([...])
    .with_delay("call-a", Delay::yields(2))
    .with_delay("call-b", Delay::yields(0))
    .record_peak_concurrency();
```

验收:

- 能稳定复现 out-of-order completion。
- 不依赖真实 sleep。
- 每个并发测试 1 分钟内完成,通常应在毫秒级完成。

### 5.11 `prelude`

导出常用测试类型与 helper,降低 import 噪音。

建议包含:

- `SeqIds`。
- fixtures 常用函数。
- `ScriptedLlmHandler`、`ScriptedToolHandler`、`ScriptedInteractionHandler`。
- `Cassette`、`CassetteRecorder`、cassette-backed handlers。
- `TestScope`。
- `DrainHarness`、`StepHarness`。
- assertion entry points。

## 6. 首版 API 示例

目标写法:

```rust
use agent_testkit::prelude::*;

#[tokio::test]
async fn tool_round_trip_is_scripted() {
    let ids = SeqIds::new();
    let llm = ScriptedLlmHandler::new([
        LlmStep::tool_use([tool_call("call-weather", "get_weather", json!({ "city": "Shanghai" }))]),
        LlmStep::text("sunny in Shanghai"),
    ]);
    let tools = ScriptedToolHandler::new([
        ToolStep::ok("call-weather", "Sunny"),
    ]);
    let scope = TestScope::builder()
        .llm(llm.clone())
        .tool(tools.clone())
        .build();
    let ctx = root_context(&ids);
    let mut machine = default_machine(&ids, default_spec_with_tools([weather_tool()]));

    let done = DrainHarness::new(&mut machine, &scope, &ctx)
        .run_user(user_input(&ids, "weather?"))
        .await
        .expect("turn drains");

    assert_done(&done);
    assert_conversation(machine.state().conversation())
        .committed_turns(1)
        .last_assistant_text("sunny in Shanghai");
    assert_calls(&llm).count(2);
    assert_calls(&tools).count(1);
}
```

录制/重放写法:

```rust
use agent_testkit::prelude::*;

#[tokio::test]
async fn recorded_tool_round_trip_replays_offline() {
    let ids = SeqIds::new();
    let cassette = Cassette::load("tests/cassettes/weather_tool_roundtrip.json")
        .expect("cassette loads");
    let scope = TestScope::builder()
        .llm(CassetteLlmHandler::replay(cassette.clone()))
        .tool(CassetteToolHandler::replay(cassette.clone()))
        .interaction(CassetteInteractionHandler::replay(cassette.clone()))
        .build();
    let ctx = root_context(&ids);
    let mut machine = default_machine(&ids, default_spec_with_tools([weather_tool()]));

    let done = DrainHarness::new(&mut machine, &scope, &ctx)
        .run_user(user_input(&ids, "weather?"))
        .await
        .expect("recorded turn replays");

    assert_done(&done);
}
```

## 7. 覆盖矩阵

首版 testkit 应支持下面这些场景,但不要求一次性全部迁移。

| 区域 | 场景 | 主要工具 |
|---|---|---|
| text turn | user -> NeedLlm -> text -> commit | `ScriptedLlmHandler`, `DrainHarness` |
| tool turn | LLM tool_use -> NeedTool -> result -> next LLM -> text | `ScriptedToolHandler`, assertions |
| parallel tools | 一批 NeedTool 并发兑现、乱序 resume | `concurrency`, call logs |
| approval | NeedInteraction approve/deny/timeout/cancel | `ScriptedInteractionHandler` |
| headless | top scope 无 interaction 报 `UnhandledRequirement` | `TestScope` |
| pop routing | child interaction pop 到 parent | `TestScope`, `ScriptMachine` |
| cancel | context cancel 后 abandon, tool 不执行 | `CancelOnCall`, `PanicOnCall` |
| pivot | post-tool boundary 注入 user pivot | `StepHarness`, requirement assertions |
| reconfig | turn boundary registry effect 与 registry swap | `ScriptedReconfigHandler` |
| recorded replay | 真实 req/resp 离线重放,CI 不联网 | `Cassette`, cassette-backed handlers |
| subagent | depth guard、budget inheritance、cancel propagation | `ScriptMachine`, `SubagentSpawner` fake |
| trace | resolved_at_scope 与 disposition | `assert_trace` |
| budget | usage/token/cost 超限与共享 ledger | `ChargingLlmHandler`, `assert_budget` |

## 8. 测试套件规划

测试增强的目标不是只提供 helper,而是逐步形成一组稳定的测试套件。分层原则:

- Rust 端覆盖基础行为与简单组合,提高基础正确性密度。
- scripted/cassette/DSL 覆盖复杂组合与真实世界回放,避免复杂场景把 Rust 测试代码膨胀到不可维护。
- 协议层仍保留自己的 adapter/client/stream 测试,不混入 agent-testkit。

### 8.1 Core Rust Suites

这些套件用 Rust + testkit 直接写,目标是快、稳定、覆盖细。

| 套件 | 目标 | 典型用例 | 工具 |
|---|---|---|---|
| `agent_step_basic` | 单机 step 协议正确 | user -> NeedLlm、resume text、wrong id、wrong result kind、abandon | `StepHarness`, fixtures |
| `agent_tool_basic` | tool phase 基础正确 | single tool、parallel tool、tool error、step limit、provider call mismatch | `StepHarness`, `ScriptedToolHandler` |
| `agent_interaction_basic` | interaction/approval 基础正确 | approve、deny、timeout、cancel、wrong call/step rejection | `ScriptedInteractionHandler` |
| `agent_pivot_basic` | pivot 边界正确 | post-tool pivot 成功、too early/open tool/idle 拒绝 | `StepHarness` |
| `agent_reconfig_basic` | turn-boundary reconfig 正确 | idle reconfig、during-turn reconfig、registry effect、atomic reject | `ScriptedReconfigHandler` |
| `agent_driver_basic` | drain/pop 基础正确 | local handler、pop to parent、top unhandled、misaligned result | `ScriptMachine`, `TestScope` |
| `agent_cancel_basic` | never-resume 基础正确 | LLM abandon、tool batch abandon、approval abandon、new turn after cancel | `StepHarness`, assertions |
| `agent_trace_budget_basic` | trace/budget 可观察 | resolved_at_scope、disposition、child token charge | `assert_trace`, `assert_budget` |

验收标准:

- 每个用例应尽量只证明一个不变量。
- 不依赖网络、credentials、真实时间或真实 provider。
- 优先替换现有重复 fixture,但保留已有低层状态机单测中直接表达更清楚的部分。
- 每个 suite 应能单独过滤运行,方便定位回归。

### 8.2 Scripted Scenario Suites

这些套件仍在 Rust 中运行,但用脚本化步骤表达复杂流程。目标是覆盖多轮、多 handler、多 scope 的组合正确性。

| 套件 | 目标 | 典型用例 | 工具 |
|---|---|---|---|
| `agent_scenario_tool_loops` | 多轮工具循环 | tool -> llm -> tool -> llm final、多 tool 混合成功/失败 | `ScriptedLlmHandler`, `ScriptedToolHandler`, `DrainHarness` |
| `agent_scenario_mixed_interaction` | 工具与审批混合 | auto tool + guarded tool、连续 approve/deny/cancel、headless unhandled | scripted handlers |
| `agent_scenario_scope_pop` | 动态作用域组合 | child headless interaction pop 到 parent、local interaction 不 pop | `TestScope`, `ScriptedSubagentSpawner` |
| `agent_scenario_concurrency` | 并发与乱序 | tool batch out-of-order、cancel after first handler、peak concurrency | `concurrency` |
| `agent_scenario_reconfig` | 复杂 reconfig | 多个 queued reconfig、registry swap 后下一 tool 使用新 registry | cassette/scripted reconfig |
| `agent_scenario_subagent` | 子 agent 生命周期 | depth guard、budget inheritance、cancel propagation、summary folding | subagent testkit |

验收标准:

- 测试主体应读起来像 scenario,不应堆满 fixture 细节。
- 断言应同时覆盖 final conversation、handler call log、trace/budget 中至少一个可观察面。
- 复杂场景失败时,错误信息应能指出脚本 step 与 effect family。

### 8.3 Recorded Replay Suites

这些套件使用 cassette,目标是在 CI 中离线复用真实 req/resp,观察真实流程在 agent 层的表现。

| 套件 | 目标 | 典型用例 | 工具 |
|---|---|---|---|
| `agent_replay_text` | 真实 text turn 回放 | recorded user -> LLM text -> commit | `CassetteLlmHandler` |
| `agent_replay_tool` | 真实 tool-use 回放 | recorded LLM tool_use + real-ish tool result + final text | cassette-backed llm/tool |
| `agent_replay_approval` | 真实审批流程回放 | tool approval request + approve/deny + final response | cassette-backed interaction |
| `agent_replay_reconfig` | registry/reconfig 回放 | recorded tool set change and subsequent request shape | cassette-backed reconfig |
| `agent_replay_regression` | 历史 bug 固化 | 用真实场景 cassette 固化曾出错的 flow | cassette + assertions |

验收标准:

- replay 测试默认必须离线可跑。
- record/update 测试默认 skipped,只能通过显式环境变量或命令启用。
- cassette 文件必须通过 redactor,并在 review 中可读。
- cassette mismatch 应报告 request fingerprint 与 entry index。

### 8.4 Future DSL/TS Suites

这些套件等 Rust scenario model 稳定后再做。目标是用更轻量的脚本表达复杂真实世界场景。

| 套件 | 目标 | 典型用例 | 工具 |
|---|---|---|---|
| `scenario_json_smoke` | JSON scenario runner 冒烟 | text/tool/approval 三类最小场景 | scenario runner |
| `scenario_json_complex` | 复杂场景回放 | 多 agent、多 tool、多 interaction、多 cancel timing | JSON scenario |
| `scenario_ts_app_like` | 应用层近真实流程 | 类 Desktop/Codex app 的 attended/headless 混合运行 | TS/NAPI 或 runner binary |

进入条件:

- Rust testkit 的 `script`、`cassette`、`harness` API 已稳定。
- 至少有 3-5 个复杂 Rust scenario 能自然映射到 data-only scenario。
- 已明确哪些断言属于 runner 输出 summary,哪些仍留在 Rust 内部。

## 9. 迁移计划

### Phase 1: 提取基础 fake

目标:不改测试语义,只减少重复。

做什么:

- 新增 testkit 位置。
- 提取 `SeqIds` / deterministic id helpers。
- 提取 provider-neutral fixtures。
- 提取 scripted LLM/tool/interaction handlers。
- 提取 `TestScope` builder。

优先迁移:

- `tests/agent_effect_e2e.rs` 中的 `SeqIds`、`FakeClient`、`FakeToolRegistry`、interaction fake。
- `src/agent/drive/reference/tests.rs` 中的重复 fake。

验收:

- 迁移后的测试行数明显下降。
- `cargo test --all --all-targets` 通过。
- 无核心库行为变更。

### Phase 2: Recorded cassette

目标:把真实 agent effect req/resp 记录为 provider-neutral fixture,供 CI 离线重放。

做什么:

- 定义 cassette schema v1。
- 实现 `CassetteRecorder` 的 record/replay 基本能力。
- 实现 `CassetteLlmHandler` 与 `CassetteToolHandler` replay。
- 增加 request fingerprint 与 mismatch 报错。
- 增加默认 redactor 与 update 环境变量护栏。

优先迁移:

- 新增一个真实录制生成的 LLM + tool round-trip cassette。
- 用 replay handler 跑一条不联网的 agent turn 测试。

验收:

- replay 环境不需要网络、credentials、真实 tool 后端。
- cassette JSON 可读、可 review、schema version 明确。
- request fingerprint 不包含 volatile id。
- record/update 不会在普通 CI 中意外覆盖 fixture。

### Phase 3: Step/Drain harness

目标:让复杂场景测试不再手写推进循环。

做什么:

- 增加 `DrainHarness`。
- 增加 `StepHarness`。
- 增加 requirement / notification / conversation assertions。

优先迁移:

- `agent::drive::tests` 的 `BatchMachine` 相关用例。
- `agent::machine::default::tests::tools` 中重复的 `park_on_need_llm` / `resume_llm` / requirement extractor。

验收:

- 手动 step 测试仍能精确断言中间 cursor。
- drain 测试可直接断言最终 conversation/trace/log。

### Phase 4: 并发与取消工具

目标:稳定表达 out-of-order、并发峰值、取消时机。

做什么:

- 增加 delay/yield/barrier 工具。
- 增加 peak concurrency counter。
- 增加 cancel-on-call / panic-on-call handler wrapper。

优先迁移:

- `batch_requirements_are_fulfilled_concurrently`。
- `reference_cancel_during_tool_wait_abandons_turn`。
- subagent cancel propagation 测试。

验收:

- 不使用真实 sleep。
- 并发用例稳定。
- 取消测试能断言未触发 IO。

### Phase 5: Subagent 与 hierarchy 场景

目标:把 attended/headless/pop/depth/budget/cancel 场景写成可复用组合。

做什么:

- 提供 `ScriptedSubagentSpawner`。
- 提供 parent/child scope builder helper。
- 提供 subagent summary 与 child log 断言。

优先迁移:

- `src/agent/drive/subagent/tests.rs`。
- `tests/agent_effect_e2e.rs` 的 parent + headless child 场景。

验收:

- 同一个 child machine 可在 attended scope 与 headless scope 下复用。
- `resolved_at_scope` 与 child budget aggregation 可直接断言。

### Phase 6: 场景 DSL 草案

目标:在 Rust 内部先形成稳定 scenario model,为未来 TS/NAPI 保留入口。

做什么:

- 定义数据化 scenario:inputs、LLM steps、tool steps、interaction policy、expected observations。
- 提供 runner:scenario -> result summary。
- 支持 serde,但先不做 Node。

验收:

- 能用一个 JSON-like Rust value 表达常见 agent turn。
- 输出 summary 适合 golden assertion。
- 不影响现有底层测试。

## 10. 设计风险与约束

### 9.1 不要掩盖顶层 total 要求

`TestScope` 不应默认给所有 requirement family 安装 handler。测试必须显式选择 headless 或 total scope,否则容易把 `UnhandledRequirement` 路径测没。

### 9.2 不要把 misaligned result 当常规错误

常规 handler 错误应放在对应 family 的 `Err` 中。比如 tool 失败返回 `RequirementResult::Tool(Err(_))`。只有专门测试 `drain` 类型对齐时才构造 wrong family result。

### 9.3 不要依赖 private API

testkit 应尽量只用 `agent-lib` 公共 API。若发现某个测试必须依赖 private API,应先判断是测试过度耦合还是核心库缺少合理观察入口。

### 9.4 不要过早稳定 DSL

首版先做 builder + handlers + harness。场景 DSL 等多个测试迁移后再抽,避免把早期不顺手的形状固化。

### 9.5 不要引入真实时间

并发测试使用 yield、barrier 或手动 channel,不使用 sleep 等 wall-clock 等待。wall-clock budget 测试通过注入 elapsed duration。

### 9.6 Cassette 不是协议 fixture

cassette 记录的是 agent effect 边界的 provider-neutral req/resp,不是 HTTP fixture。它不应包含 header、token、base URL、provider raw body。若某个测试需要验证这些内容,应回到 adapter/client 协议层测试。

### 9.7 Cassette 必须可审阅且可脱敏

真实运行可能包含用户输入、工具参数和模型输出。默认 cassette writer 必须使用 redactor,并让测试显式 opt-in 保留 provider extras 或敏感字段。更新 cassette 需要显式环境变量或命令,避免 CI 或普通测试运行产生不可控 diff。

## 11. 与现有设计缺口的关系

测试增强不是直接修复设计缺口,但应把缺口变成可观察事实。

已识别需要后续单独决策或补测的点:

- 中途 pause/restore 语义:当前 `AgentState` 序列化依赖 `Conversation::snapshot`,pending turn 会拒绝。testkit 应先能测试 committed-boundary restore;mid-requirement restore 需等设计闭合。
- `NestedMachine` 与 `DrivingSubagentHandler` 的模型关系:一个是 parent-contained tree,一个是 handler 内 ephemeral child drain。testkit 可先覆盖当前行为,但不应把未定语义包装成稳定 DSL。
- budget enforcement:当前 `RunContext` 有 charge API,但 reference driver 不统一 charge usage/step。testkit 可提供 charging handler 并补覆盖,同时帮助识别是否需要 driver-level charge。
- trace granularity:当前重点记录 requirement disposition,完整 run -> step -> llm/tool/subagent trace 仍需补。testkit 的 `assert_trace` 应从现有能力开始,逐步扩展。
- streaming tee:当前 `LlmHandler` 多数场景返回 folded `Response`;token delta UI sink 尚未落地。testkit 暂不模拟 provider SSE,后续等 sink 接口定型再加 token-level 场景。
- `max_parallel_tools`:字段存在,但当前 tool phase 会吐出 auto-approved batch,driver 会并发兑现本地 batch。testkit 应增加覆盖来钉住预期,随后决定实现 limit 或调整语义。
- cancel all outstanding:当前 `drain` cancel 时 abandon pending 的第一个 requirement。单机 tool phase 能闭合整批;未来 hierarchy 聚合多节点时需测试是否要 abandon 全部 affected subtree。

## 12. 验收标准

每个 phase 至少满足:

- `cargo fmt --all` 通过。
- `cargo clippy --all-targets -- -D warnings` 通过。
- 相关聚焦测试通过。
- `cargo test --all --all-targets` 通过。
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` 通过。
- `git diff --check` 干净。

testkit 自身额外标准:

- 所有 helper 失败信息包含足够上下文。
- 每个测试用例正常应在毫秒级完成,不得依赖真实网络或 credentials。
- cassette replay 测试不得依赖真实网络或 credentials;record/update 测试必须默认 skipped 或显式 opt-in。
- 测试数据只使用 provider-neutral model/client 类型。
- 不改变 `agent-lib` 运行时 API 语义。

## 13. 未来 TS/NAPI 入口

短期结论:先不做 Node/NAPI。

保留方向:

- 如果 Rust scenario model 稳定,可以新增一个 JSON scenario runner。
- TS 测试可先调用 runner binary,传入 scenario JSON,读取 result JSON。
- 若需要更低延迟或更强 IDE 体验,再把同一个 runner 包成 NAPI。

这样可以避免直接把 Node async callback、NAPI 错误映射、tokio runtime 管理引入当前测试增强主线。
