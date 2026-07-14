# TODO：Agent Testability 与 `agent-testkit` 实现任务列表

> 依据 [`PLAN.md`](PLAN.md) 与 [`docs/TESTABILITY.md`](docs/TESTABILITY.md)。任务按真实依赖顺序编号;
> coding agent 每次只执行首个标题带 `[TODO]` 的任务,完成后把该标题的 `[TODO]` 改为 `[DONE]`,并在
> 任务末尾补充完成记录。
>
> 当前任务列表接续已完成的 Agent Effect Model 迁移。旧迁移计划和任务已归档在
> [`docs/archive/2026-07-14-agent-effect-migration/`](docs/archive/2026-07-14-agent-effect-migration/)。

通用约束:测试增强不得改变 `agent-lib` 运行时语义;不得 mock HTTP provider;不得把 auth、endpoint、
provider raw body 录入 cassette;不得依赖真实 sleep、网络或 credentials 作为默认测试条件;testkit 优先使用
`agent-lib` 公开 API;每个测试用例必须在 1 分钟内完成。完整验证按 “format → 严格 clippy → 聚焦测试 →
全量测试 → rustdoc → diff check” 执行。

---

## Milestone 1 — Testkit 骨架与基础数据

### [DONE] M1-1 建立 `agent-testkit` 拓扑与最小 crate 骨架

**前置依赖**:无。

**上下文**:当前仓库根 `Cargo.toml` 是单 package。新测试基础设施应是 dev-only,由 testkit 依赖
`agent-lib` 并实现其公开 trait。短期不拆 trait crate。需要先验证 Cargo 拓扑:首选
`crates/agent-testkit`,如 root dev-dep 周期不可接受,先把迁移类集成测试放在 `crates/agent-testkit/tests/`。

**做什么**:

- 新建 `crates/agent-testkit/` skeleton,包含 `Cargo.toml`、`src/lib.rs`、`src/prelude.rs`。
- 在 root `Cargo.toml` 增加 workspace 配置,把 `.` 与 `crates/agent-testkit` 纳入 members;若这种拓扑导致
  Cargo 无法正常测试 root package,记录原因并改用 `tests/support/agent_testkit` 过渡方案。
- `agent-testkit` 依赖 `agent-lib = { path = "../.." }`,并复用 `async-trait`、`futures`、`serde`、
  `serde_json`、`tokio`、`uuid` 等已有基础依赖,避免引入 mockall、proptest、insta 等新工具。
- 在 `lib.rs` 预声明模块:`ids`、`fixtures`、`script`、`handlers`、`cassette`、`scope`、`machine`、
  `harness`、`assertions`、`concurrency`、`subagent`、`prelude`。除 `prelude` 外可先为空模块或最小 stub。
- 增加一个 smoke test,证明 testkit 能引用 `agent_lib::agent::AgentMachine` 等公开类型。

**验证**:

- `cargo fmt --all`。
- `cargo clippy --all-targets -- -D warnings`。
- `cargo test -p agent-testkit` 或过渡方案下的等价聚焦测试。
- `cargo test --all --all-targets`。
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`。
- `git diff --check`。

**完成记录**(2026-07-14):

- **拓扑决策**:采用首选方案 `crates/agent-testkit`。root `Cargo.toml` 增加
  `[workspace] { members = [".", "crates/agent-testkit"], resolver = "3" }`(edition 2024 → resolver 3)。
  testkit 只单向依赖 `agent-lib = { path = "../.." }`,`agent-lib` 未反向 dev-dep testkit,
  因此无 Cargo 依赖周期,root package 测试与构建正常,无需 `tests/support/agent_testkit` 过渡方案。
- **crate 骨架**:新建 `crates/agent-testkit/Cargo.toml`(`publish = false`,复用
  `async-trait`/`futures`/`serde`/`serde_json`/`tokio`/`uuid`,未引入 mockall/proptest/insta)、
  `src/lib.rs`(crate 级文档 + `#![warn(missing_docs)]`)、`src/prelude.rs`。
- **模块预声明**:`ids`、`fixtures`、`script`、`handlers`、`cassette`、`scope`、`machine`、
  `harness`、`assertions`、`concurrency`、`subagent`、`prelude` 全部落地。除 `prelude` 外均为带模块
  文档的 skeleton stub,标注各自将由哪个里程碑填充。`prelude` 先 re-export
  `AgentMachine`/`DefaultAgentMachine`/`LlmStepMode`/`Requirement`/`RequirementKind`/`StepInput`/`StepOutcome`。
- **smoke test**:`tests/smoke.rs` 通过泛型约束 `assert_agent_machine::<DefaultAgentMachine>()`
  证明 `DefaultAgentMachine` 满足公开 `AgentMachine` trait,并经 `prelude` 与全限定路径引用
  `agent_lib::agent::LlmStepMode`,验证 testkit 能引用公开类型。
- **验证结果**(全绿):`cargo fmt --all`;`cargo clippy --all-targets -- -D warnings`(两 crate 均干净);
  `cargo test -p agent-testkit`(2 passed);`cargo test --all --all-targets`(全部通过,0 failed);
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`(agent-lib 与 `-p agent-testkit` 均干净);
  `git diff --check` 干净。
- **备注**:`cargo doc --no-deps` 默认只文档化 root package(workspace default member),故额外用
  `-p agent-testkit` 单独验证 testkit rustdoc 无 warning。后续里程碑如需默认一并出 testkit 文档,可再评估
  `default-members`,当前未做此改动以避免超出 M1-1 范围。

### [DONE] M1-2 实现 deterministic id source: `SeqIds`

**前置依赖**:M1-1。

**上下文**:现有测试在 `tests/agent_effect_e2e.rs`、`src/agent/drive/reference/tests.rs`、
`src/agent/machine/default/tests/tools.rs` 等处重复手写 UUID helper、`RequirementIds`、`ToolExecutionIds`。
testkit 需要一个 clone 后共享计数器的 id source,确保 parent/child/subagent/trace 节点全局唯一。

**做什么**:

- 在 `ids.rs` 实现 `SeqIds { counter: Arc<AtomicU64>, prefix/base }`。
- 实现 `RequirementIds` 和 `ToolExecutionIds`。
- 提供 helper: `requirement_id()`、`run_id()`、`trace_node(label)`、`agent_id()`、`tool_set_id()`、
  `conversation_id()`、`turn_id()`、`message_id()`、`tool_call_id()`、`step_id()`。
- 支持 `fork(label)` 或 `named(label)` 生成带可读 label 的 trace id,但底层仍保证唯一。
- 记录分配日志:至少能查看 requirement id 按 `RequirementKindTag` 的分配顺序。
- 提供耗尽/失败模式,用于测试 id unavailable 路径。
- 从 `prelude` re-export `SeqIds`。

**验证**:

- 单测:clone 后共享 counter 且不会重复。
- 单测:实现 `RequirementIds` / `ToolExecutionIds` 后,分配出的 id 可被 `agent-lib` 解析和使用。
- 单测:失败模式返回 `RequirementError::IdUnavailable` / `ToolRuntimeError::IdUnavailable`。
- 单测:分配日志能按 tag 查询。
- 跑全套验证命令。

**完成记录**(2026-07-14):

- **`SeqIds` 结构**:`crates/agent-testkit/src/ids.rs` 实现 `SeqIds { shared: Arc<Shared>, base: u64,
  label: Arc<str> }`。`Shared { counter: AtomicU64, base_counter: AtomicU64,
  requirement_log: Mutex<Vec<RequirementAllocation>>, remaining: AtomicI64 }` 被整棵 clone/fork 树共享。
- **唯一性模型**:每个 UUID = `((base as u128) << 64) | seq`,`seq` 取自单一共享单调 `AtomicU64`
  (从 1 起,永不为 nil UUID)。低 64 位全局单调不重复 → 无论 `base` 如何都不会碰撞;`base` 只做高位
  可读区分。`clone` 保持同 `base` 并共享 counter;`fork(label)` 分配新 `base`(新子树)、共享 counter、
  携带嵌套可读 label;`named(label)` 同 `base` 重贴 label。
- **contract 实现**:`impl RequirementIds`(`next_requirement_id`)与 `impl ToolExecutionIds`
  (`tool_call_id`/`tool_result_message_id`/`next_assistant_message_id`/`next_step_id`)。
  inherent helpers:`requirement_id`/`run_id`/`agent_id`/`tool_set_id`/`conversation_id`/`turn_id`/
  `message_id`/`tool_call_id`/`step_id`/`trace_node(node)`。注:inherent 无参 `tool_call_id()` 与
  trait `ToolExecutionIds::tool_call_id(&call)` 同名共存(inherent 方法优先解析,driver 经 trait bound
  仍调用 contract 方法),已在文档说明,测试用 UFCS 调用 contract 版本。
- **分配日志**:`next_requirement_id` 按顺序记录 `RequirementAllocation { tag, id }`。
  `requirement_log()` 返回全序,`requirement_ids(tag)` 按 tag 过滤(保序)。
- **失败模式**:`SeqIds::exhausted()` / `with_budget(n)` 用共享 `remaining`(CAS 递减,`-1`=unlimited)
  控制 contract 方法可成功次数;耗尽后 `next_requirement_id` 返回 `RequirementError::IdUnavailable`、
  tool 方法返回 `ToolRuntimeError::IdUnavailable`。inherent helpers 不消耗 budget(供 fixtures 构造)。
- **prelude**:re-export `SeqIds` 与 `RequirementAllocation`。
- **测试**(`ids.rs` 内 8 个单测):clone 共享 counter 不重复;fork 唯一 + 嵌套 label;named 重贴 label
  不碰撞;minted id 经 `agent-lib` `parse`/`to_string` 往返一致(证明可被 agent-lib 解析使用);
  日志按 tag 查询保序;exhausted 两种 `IdUnavailable`;budget 跨 clone 共享且仅 contract 方法消耗。
- **验证结果**(全绿):`cargo fmt --all`;`cargo clippy --all-targets -- -D warnings`(两 crate 干净);
  `cargo test -p agent-testkit`(lib 8 + smoke 2 passed);`cargo test --all --all-targets`(全部通过,
  0 failed;3+1 个 network-gated 用例照旧 ignored);`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`
  与 `-p agent-testkit` 均干净(修掉两处 redundant-explicit-links);`git diff --check` 干净。

### [DONE] M1-3 实现 provider-neutral fixtures

**前置依赖**:M1-2。

**上下文**:agent 层测试只应构造 provider-neutral `Message`、`Response`、`ToolCall`、`ToolResponse`、
`AgentSpec`、`AgentState`、`RunContext`。不要构造 Anthropic/OpenAI wire JSON。

**做什么**:

- 在 `fixtures.rs` 实现 `text_block`、`user_message`、`user_input(&SeqIds, text)`。
- 实现 LLM response helpers:`assistant_text(text, usage)`、`assistant_tool_use(calls, usage)`、
  `usage(input, output)`。
- 实现 tool helpers:`tool_call(provider_id, name, input)`、`tool_response(provider_call_id, text, status)`、
  `tool_ok`、`tool_error_response`。
- 实现 declaration helpers:`weather_tool()`、`calendar_tool()`。
- 实现 agent helpers:`agent_spec`、`agent_spec_with_tools`、`agent_state`、`default_machine`、
  `root_context(&SeqIds)`。
- 所有 helper 通过公开构造器创建数据,不得使用 private API 或 unchecked mutation。
- 从 `prelude` re-export 常用 fixtures。

**验证**:

- 单测:fixtures 生成的 `AgentInput::UserMessage` role 合法。
- 单测:assistant text/tool_use response 可被 `DefaultAgentMachine` fold 的最小 smoke。
- 单测:tool declaration 与 `ToolSetRef` round-trip 保持稳定。
- 跑全套验证命令。

**完成记录**(2026-07-14):

- **fixtures 落地**:`crates/agent-testkit/src/fixtures.rs` 全部经 `agent-lib` 公开构造器实现,无 private
  API / unchecked mutation。message/content:`text_block`、`user_message`、`user_input(&SeqIds, text)`
  (turn/user-message/assistant-message/step id 全取自 `SeqIds`,经 `AgentInput::user_message` 校验 role)。
  LLM response:`usage(input, output)`、`assistant_text(text, usage)`(stop=`end_turn`)、
  `assistant_tool_use(Vec<ToolCall>, usage)`(stop=`tool_use`,每个 `ToolCall` 映射为一个
  `ContentBlock::ToolUse`)。tool:`tool_call(provider_id, name, input)`、
  `tool_response(provider_call_id, text, status)`、`tool_ok`(`ToolStatus::Ok`)、
  `tool_error_response`(`ToolStatus::Error`)。declaration:`weather_tool()`(`get_weather`/`city`)、
  `calendar_tool()`(`get_calendar`/`date`)。agent:`agent_spec(&SeqIds)`(空 toolset)、
  `agent_spec_with_tools(&SeqIds, Vec<Tool>)`、`agent_state(&SeqIds, AgentSpec)`(conversation id 取自
  `SeqIds`)、`default_machine(&SeqIds, AgentState)`(`RequirementIds` 与 `ToolExecutionIds` 均由
  `ids.clone()` 提供、NonStreaming)、`root_context(&SeqIds)`(`RunContext::new_root` + `BudgetLimits::unbounded()`
  + `trace_node("root")`)。
- **prelude**:re-export 上述常用 fixtures(与 `SeqIds`/`RequirementAllocation` 并列)。
- **测试**(`fixtures.rs` 内 6 个单测):`user_input` 产出 role=User 的 `AgentInput::UserMessage`;
  `assistant_text` 经 `default_machine` fold(External→NeedLlm,Resume(Llm(Ok))→提交 turn、cursor=Done);
  `assistant_tool_use` fold 出 `RequirementKind::NeedTool`(证明 tool 执行 id 源接线正确);tool_ok/error 状态正确;
  `weather_tool()`+`calendar_tool()` 经 `ToolSetRef` serde round-trip 与 `AgentSpec::initial_tools` 稳定;
  `root_context` depth=0、budget unbounded。
- **验证结果**(全绿):`cargo fmt --all`(--check 干净);`cargo clippy --all-targets -- -D warnings`
  (两 crate 干净);`cargo test -p agent-testkit`(lib 14 + smoke 2 passed);`cargo test --all --all-targets`
  (14 个测试二进制全部 `test result: ok`,0 failed);`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`
  与 `-p agent-testkit` 均干净(module doc 用 in-scope intra-doc link 规避 redundant-explicit-links);
  `git diff --check` 干净。
- **备注**:`PLAN.md` 无需改动(阶段拓扑/依赖未变);仅 testkit 新增数据构造器,无 `agent-lib` 运行时语义变化,
  未引入 provider wire mock。

### [DONE] M1-R Milestone 1 Review

**前置依赖**:M1-1..M1-3。

**上下文**:确认 testkit 拓扑、id source、fixtures 已形成稳定基础,且未引入 provider wire mock 或运行时语义变化。

**做什么**:

- Review `Cargo.toml` 拓扑,确认是否采用 `crates/agent-testkit` 还是过渡支持模块,并记录理由。
- 确认 testkit 只依赖 `agent-lib` 公开 API。
- 确认 `SeqIds` 覆盖 `RequirementIds`、`ToolExecutionIds` 与常用 Agent/Conversation id。
- 确认 fixtures 只产生 provider-neutral 类型。
- 更新 `PLAN.md` 或 `docs/TESTABILITY.md` 中与实际拓扑不一致的描述。

**验证**:

- 全套验证命令全部通过。
- Review 结论和任何偏离计划的原因写入完成记录。

**完成记录**(2026-07-14):

- **拓扑结论**:采用首选 crate 形态,非过渡支持模块。root `Cargo.toml` =
  `[workspace] { members = [".", "crates/agent-testkit"], resolver = "3" }`;`crates/agent-testkit/Cargo.toml`
  单向依赖 `agent-lib = { path = "../.." }`(`publish = false`),`agent-lib` 未反向 dev-dep testkit,
  故无 Cargo 依赖周期,root package 构建/测试正常,无需 `tests/support/agent_testkit`。当前 testkit 的
  集成测试落在自身 `tests/smoke.rs`;`agent-lib` 现有测试改用 testkit 属 M6 迁移范围,符合计划。
- **只依赖公开 API**:testkit 全部 `use agent_lib::{ agent::*, client::*, conversation::*, model::* }`
  均为公开路径(`RequirementIds`/`ToolExecutionIds`/`AgentMachine`/`DefaultAgentMachine`/`Message`/`Response`/
  `ToolCall`/`ToolResponse`/`AgentSpec`/`AgentState`/`RunContext` 等);`Cargo.toml` 只复用
  `async-trait`/`futures`/`serde`/`serde_json`/`tokio`/`uuid`,未引入 mockall/proptest/insta。跨 crate
  只能访问 `pub` 项,天然不触及 `agent-lib` 内部不变量,未见 unchecked mutation / private 绕过。
- **`SeqIds` 覆盖度**:`impl RequirementIds`(`next_requirement_id`)与 `impl ToolExecutionIds`
  (`tool_call_id`/`tool_result_message_id`/`next_assistant_message_id`/`next_step_id`)齐全;inherent
  helper 覆盖 `requirement_id`/`run_id`/`agent_id`/`tool_set_id`/`conversation_id`/`turn_id`/`message_id`/
  `tool_call_id`/`step_id`/`trace_node`。clone 共享 counter、`fork` 新子树 + 嵌套 label、`named` 重贴 label、
  `exhausted`/`with_budget` 失败模式、`requirement_log`/`requirement_ids(tag)` 保序可查——单一共享单调
  `AtomicU64` 保证全局唯一。
- **fixtures provider-neutral**:所有 helper 经 `agent-lib` 公开构造器产出 provider-neutral 类型
  (`ContentBlock`/`Message`/`Response`/`Usage`/`ToolCall`/`ToolResponse`/`Tool`/`AgentSpec`/`AgentState`/
  `RunContext`/`DefaultAgentMachine`),无 Anthropic/OpenAI wire JSON,无 private API。`assistant_tool_use`
  仅把 `ToolCall` 映射为 `ContentBlock::ToolUse`,不触碰传输层。
- **文档并轨**:`PLAN.md` 过渡门(`tests/support/agent_testkit`)与 `docs/TESTABILITY.md` §4 候选拓扑各补一处
  “已定案:crate 形态(工作区成员,无反向 dev-dep,无依赖周期)”注记;`docs/TESTABILITY.md` §4 推荐模块树
  补齐遗漏的 `subagent.rs`,与实际 `lib.rs` 预声明一致。`PLAN.md` 阶段拓扑/依赖/完成门未变,故不改结构。
- **验证结果**(全绿):`cargo fmt --all -- --check`;`cargo clippy --all-targets -- -D warnings` 与
  `-p agent-testkit --all-targets`(两 crate 均干净);`cargo test -p agent-testkit`(lib 14 + smoke 2 passed);
  `cargo test --all --all-targets`(agent-lib 434 unit + e2e/capability/conversation 等集成套件全绿,testkit
  14 + 2,0 failed;7 个 network-gated 集成用例照旧 ignored);`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`
  与 `-p agent-testkit` 均干净;`git diff --check` 干净。
- **偏离计划**:无。M1 三项产出(拓扑、`SeqIds`、fixtures)已形成稳定基础,未引入 provider wire mock 或
  `agent-lib` 运行时语义变化。本次仅改文档(TODO/PLAN/TESTABILITY),编译产物自 M1-3 起未变,但作为里程碑
  Review 仍完整重跑全套验证以确认现态全绿。

---

## Milestone 2 — Scripted handlers、scope 与 machine doubles

### [DONE] M2-1 实现 script model、strict 模式与 call log

**前置依赖**:M1-R。

**上下文**:现有 fake 大多是 `VecDeque<Result<...>>` 加 `Mutex`。需要统一成可复用脚本模型,支持严格模式、调用日志和清晰错误。

**做什么**:

- 在 `script.rs` 定义 `StrictMode`:脚本耗尽时 panic 或返回分类错误。默认选择返回分类错误,专门测试可 opt-in panic。
- 定义 `LlmStep`、`ToolStep`、`InteractionStep`、`ReconfigStep`。
- 定义 `CallLog<TRequest, TResultSummary>` 或分 family 的 log 类型,记录调用序号、请求摘要、结果摘要、完成顺序。
- 脚本支持按顺序匹配;为 tool 和 interaction 预留按 key 匹配接口,但首版可只实现顺序匹配。
- 错误信息必须包含 family、调用序号、脚本长度、可选 cassette/scenario label。

**验证**:

- 单测:脚本按顺序消费并记录 call log。
- 单测:脚本耗尽返回可断言的错误信息。
- 单测:strict panic 模式只在显式 opt-in 时 panic。
- 跑全套验证命令。

**完成记录**(2026-07-14):

- **StrictMode**:`enum StrictMode { Error(#[default]), Panic }`。`Script::next_step` 耗尽时,`Error` 返回
  `Err(ScriptError::Exhausted)`,`Panic` 走 `panic!("{error}")`。默认 `Error`,`with_strict_mode(Panic)` 显式
  opt-in——`error_mode_does_not_panic_on_exhaustion` 与 `panic_mode_panics_only_when_opted_in` 用
  `catch_unwind` 分别断言两种模式行为。
- **Step 类型**:`trait ScriptStep`(`const FAMILY: RequirementKindTag` + `into_result(self) -> RequirementResult`
  + 预留 `match_key`)统一四个 family。`LlmStep`(`text`/`tool_use`/`response`/`error`/`with_usage`,载荷
  `Result<Response, ClientError>`)、`ToolStep`(`ok`/`error`/`response`/`runtime_error`/`with_key`,载荷
  `Result<ToolResponse, ToolRuntimeError>`,provider call id 兼作 key)、`InteractionStep`(`answer`/`choice`/
  `approval`/`response`/`with_key`,载荷 `InteractionResponse`)、`ReconfigStep`(`ok`/`error`,载荷
  `Result<(), ToolRuntimeError>`)。每个 `into_result` 只产出对应 family 的 `RequirementResult`,由
  `steps_convert_to_their_result_family`、`tool_step_error_is_a_model_visible_error_response`、
  `tool_step_runtime_error_stays_in_the_tool_family_err_path`、`llm_step_error_stays_in_the_llm_family_err_path`
  验证错误路径不串 family。
- **CallLog**:泛型 `CallLog<Req, Res>`(内部 `Mutex`),`begin(req) -> CallTicket` 记 dispatch 序号,
  `complete(ticket, res)` 记 completion 序号(与 dispatch 分离,为 M5 并发乱序完成预留),`record` 为同步一体化;
  `CallRecord { call_index, request, result: Option, completion_index: Option }`;查询 `len`/`is_empty`/
  `completed_len`/`with_records`/`records`/`requests`。`call_log_records_request_result_and_orders` 用乱序
  complete 断言 dispatch 与 completion 两套序号,`call_log_record_begins_and_completes_atomically` 断言一体化路径。
- **顺序匹配 + 预留 key**:`Script<S: ScriptStep>`(内部 `Mutex<VecDeque<S>>` + dispatched 计数)首版仅按 dispatch
  顺序 `pop_front`;`ScriptStep::match_key` 已在 `ToolStep`/`InteractionStep` 上返回 key,但 `Script` 暂不消费,
  为后续按 key 匹配预留接口。`script_consumes_steps_in_dispatch_order` 验证顺序消费与 key 保留。
- **错误信息**:`ScriptError::Exhausted { family, call_index, script_len, label }` 手写 `Display`,含 family、
  0-based 调用序号、脚本长度、可选 label。`exhausted_script_returns_a_classified_error_by_default` 断言字段与
  "llm"/"call #1"/"1 step" 子串;`exhaustion_error_includes_the_optional_label` 断言含 "weather-scenario" 与
  "reconfig"。
- **prelude**:`prelude.rs` 追加 `CallLog`/`CallRecord`/`CallTicket`/`InteractionStep`/`LlmStep`/`ReconfigStep`/
  `Script`/`ScriptError`/`ScriptStep`/`StrictMode`/`ToolStep` 导出。
- **验证结果**(全绿):`cargo fmt --all -- --check`;`cargo clippy --all-targets -- -D warnings` 与
  `-p agent-testkit --all-targets`(两 crate 均干净);`cargo test -p agent-testkit`(lib 25 含 11 个新 script 用例
  + smoke 2 passed);`cargo test --all --all-targets`(agent-lib 434 unit + 各集成套件全绿,testkit 25 + 2,
  0 failed,7 个 network-gated ignored);`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`(root + testkit,修掉两处
  redundant explicit intra-doc link 后干净);`git diff --check` 干净。
- **偏离计划**:无。script 层为纯新增模块,未改 `agent-lib` 运行时语义,未引入 provider wire mock。

### [DONE] M2-2 实现 scripted effect handlers

**前置依赖**:M2-1。

**上下文**:testkit 需要直接实现 agent effect handler traits,而不是 mock `LlmClient` 或 HTTP provider。

**做什么**:

- 在 `handlers.rs` 实现 `ScriptedLlmHandler: LlmHandler`。
- 实现 `ScriptedToolHandler: ToolHandler`。
- 实现 `ScriptedInteractionHandler: InteractionHandler`,提供 `approve_all`、`deny_all`、按顺序决策等 helper。
- 实现 `ScriptedReconfigHandler: ReconfigHandler`。
- 可选实现 `ScriptedToolRegistry: ToolRegistry`,用于需要走 `ToolRegistryHandler` 的 reference-scope 测试。
- 常规错误必须返回对应 family 的 `RequirementResult::* (Err(...))`,不得用 wrong family 表达失败。
- 另提供 `MisalignedHandler` 或测试专用 wrapper,专门用于验证 `drain` 的 result family 校验。
- 从 `prelude` re-export 常用 handler。

**验证**:

- 单测:每个 handler 返回的 `RequirementResult` 被对应 `RequirementKind::accepts` 接受。
- 单测:LLM/tool/interaction/reconfig 错误路径都保留在正确 family。
- 单测:misaligned wrapper 能触发 `drain` 的 misaligned result 错误。
- 跑全套验证命令。

**完成记录**(2026-07-14):

- **四个 scripted handler**:`crates/agent-testkit/src/handlers.rs` 实现 `ScriptedLlmHandler`(`LlmHandler`)、
  `ScriptedToolHandler`(`ToolHandler`)、`ScriptedInteractionHandler`(`InteractionHandler`)、
  `ScriptedReconfigHandler`(`ReconfigHandler`)。前三/四个 script-backed handler 各持
  `Arc<Script<Step>>` + `Arc<CallLog<Req, RequirementResult>>`(别名 `LlmCallLog`/`ToolCallLog`/
  `InteractionCallLog`/`ReconfigCallLog`),`new(Arc<Script>)` 与 `from_steps(iter)` 两种构造,
  `script()`/`log()` accessor 供 drain 后读取。每次 `fulfill` 都 `log.begin(req)` → 产结果 →
  `log.complete(ticket, result)`,记录 dispatch 与 completion 双序号。
- **family-aligned 错误**:`into_result()` 恒对齐 family;script 耗尽(`StrictMode::Error`)按 family 折叠为
  族内 `Err`——LLM→`Llm(Err(ClientError::Other))`,Tool→`Tool(Err(ToolRuntimeError::ExecutionFailed{tool_name}))`,
  Reconfig→`Reconfig(Err(ToolRuntimeError::InvalidRegistry))`;`StrictMode::Panic` 时 `Script::next_step`
  自身 panic(opt-in),handler 不折叠。绝不用 wrong family 表达失败。
- **interaction 反应式**:`InteractionResponse` 无 `Err` family 且 approval 响应必须寻址活 request 的
  `step_id`/`call_id`,故 `ScriptedInteractionHandler` 采用反应式 `InteractionDecision`
  (`Approve`/`ApproveWith`/`Deny`/`Answer`/`Choice`/`Response`)而非重放预建响应队列。helper:
  `approve_all()`、`deny_all(message)`(固定 disposition),`sequence(decisions)`(按 dispatch 顺序消费,
  耗尽时 `StrictMode::Error` 回落到可配 `with_exhausted_decision`(默认 `Deny(None)`),`StrictMode::Panic`
  + `with_label` 显式 panic)。`approval_response` 从 request 构造寻址正确的 `ApprovalResponse`,
  Question/Choice 给出 trivial in-family 响应。
- **ScriptedToolRegistry**:`ToolRegistry`(+`Debug`)变体,`declarations()` 返回声明的 `Tool` 集,
  `execute()` 走 `Script<ToolStep>`(与 `ScriptedToolHandler` 共用 `tool_step_result` 折叠逻辑),
  供经 reference `ToolRegistryHandler`/turn-boundary reconfig 的测试使用。
- **MisalignedHandler**:持一个 wrong-family `RequirementResult`,同时实现四个 handler trait,`fulfill`
  恒返回该 result,用于触发 `drain` 的 `RequirementKind::accepts` 校验失败。
- **prelude**:追加 `ScriptedLlmHandler`/`ScriptedToolHandler`/`ScriptedInteractionHandler`/
  `ScriptedReconfigHandler`/`ScriptedToolRegistry`/`MisalignedHandler`/`InteractionDecision` 及四个
  CallLog 别名导出。
- **测试**(`handlers.rs` 内 12 个单测):四 family 的结果均被对应 `RequirementKind::accepts` 接受
  (interaction 用真实 approval request,验证 step/call 寻址);LLM/tool/reconfig 的 scripted 错误与
  耗尽折叠都停留在正确 family;`interaction_deny_stays_in_the_interaction_family`;
  interaction sequence 顺序消费 + 耗尽回落;interaction panic 模式仅 opt-in 才 panic(`catch_unwind` +
  `futures::executor::block_on`);ScriptedToolRegistry declare/execute/耗尽;
  用内联 `LlmScope` + `drain` 做 scripted-LLM 文本回合冒烟(抵达 `Done`,log 记 1 条);
  `misaligned_handler_trips_the_drain_family_check` 断言 `drain` 返回含 "misaligned" 的 `AgentError::Other`。
- **验证结果**(全绿):`cargo fmt --all -- --check`;`cargo clippy --all-targets -- -D warnings`
  与 `-p agent-testkit`(两 crate 均干净);`cargo test -p agent-testkit`(lib 38 含 12 个新 handler 用例
  + smoke 2);`cargo test --all --all-targets`(agent-lib 434 unit + 集成套件全绿,testkit 38 + 2,
  0 failed,7 个 network-gated ignored);`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` 与
  `-p agent-testkit`(修掉一处 redundant explicit link 与一处把 `Deny(None)` 误判为 intra-doc link 后干净);
  `git diff --check` 干净。
- **偏离计划**:无。handlers 层为纯新增模块,未改 `agent-lib` 运行时语义,未引入 provider wire mock;
  interaction 采用反应式决策(而非直接包 `Script<InteractionStep>`)是因 interaction family 无 `Err`
  且响应须寻址活 request,已在模块与任务文档说明。

### [DONE] M2-3 实现 `TestScope` builder

**前置依赖**:M2-2。

**上下文**:现有测试大量手写 `impl HandlerScope for TestScope`。builder 应让测试显式选择哪些 family 被本层处理,避免默认 total 掩盖 `UnhandledRequirement`。

**做什么**:

- 在 `scope.rs` 实现 `TestScope` 和 `TestScopeBuilder`。
- builder 支持 `.llm(...)`、`.tool(...)`、`.interaction(...)`、`.subagent(...)`、`.reconfig(...)`。
- 支持 headless scope:未挂 interaction 时 `interaction()` 返回 `None`。
- 支持 attended scope helper,但必须显式调用。
- 支持 wrapping `ReferenceScope` 或把已有 handler trait object 放入 scope。
- scope 内 handler 使用 `Arc` 存储,方便测试结束后读取 call log。

**验证**:

- 单测:空 scope 所有 accessor 返回 `None`。
- 单测:只挂 tool 时只有 `tool()` 返回 `Some`。
- 单测:headless top scope 遇到 `NeedInteraction` 仍返回 `UnhandledRequirement`。
- 跑全套验证命令。

**完成记录**(2026-07-14):

- **`TestScope` + `TestScopeBuilder`**:`crates/agent-testkit/src/scope.rs` 实现二者。`TestScope` 持五个
  family 槽 `Option<Arc<dyn LlmHandler/ToolHandler/InteractionHandler/SubagentHandler/ReconfigHandler>>`
  加一个 `inner: Option<Arc<dyn HandlerScope>>`。`impl HandlerScope for TestScope` 每个 accessor 先看本层
  override(`Some(arc.as_ref())`),否则委派 `inner`(`inner.and_then(|s| s.llm())` 等),否则 `None`。
  未显式挂且无 inner 时返回 `None`——scope **默认不 total**,顶层缺 handler 仍暴露
  `UnhandledRequirement`。
- **builder 五 family setter**:`.llm(..)`/`.tool(..)`/`.interaction(..)`/`.subagent(..)`/`.reconfig(..)`
  各取 `Arc<dyn XHandler>`(调用点由具体 `Arc<H>` 自动 unsize 强转,调用方可持另一个具体 `Arc` clone
  在 drain 后读 `log()`)。`.build()` 出 `TestScope`;`TestScope::builder()`/`TestScope::empty()` 便捷入口。
- **headless / attended**:未调用 `.interaction(..)` 时 `interaction()` 恒 `None`(headless,不兜底、不
  auto-approve);`.attended(handler)` 是 `.interaction(handler)` 的语义别名,**必须显式调用**才让该层
  attended。
- **wrapping**:`.wrapping(Arc<dyn HandlerScope>)` 设 inner,把本层未 override 的 family 委派给内层
  (可包 `ReferenceScope` 或另一个 `TestScope`);per-family override 始终优先于被包 scope。
  「把已有 handler trait object 放入 scope」即各 setter 取 `Arc<dyn XHandler>` 的直接用法。
- **prelude**:追加 `TestScope`/`TestScopeBuilder` 导出。
- **测试**(`scope.rs` 内 4 个单测):`empty_scope_serves_no_family`(五 accessor 全 `None`);
  `tool_only_scope_serves_only_the_tool_family`(仅 `tool()` 为 `Some`,其余 `None`);
  `wrapping_delegates_unoverridden_families_to_the_inner_scope`(外层只 override llm,`tool()` 经内层解析);
  `headless_top_scope_surfaces_unhandled_interaction`(inline `RequireApprovalPolicy` +
  `agent_spec_with_tools([weather_tool])` + `ScriptedLlmHandler` 出 `tool_use`;TestScope 挂 llm+tool 不挂
  interaction;`drain(.., None, ..)` 返回 `AgentError::UnhandledRequirement { kind: Interaction }`,
  scripted tool 的 call log 长度 0,证明未 auto-approve 也未跳过)。
- **验证结果**(全绿):`cargo fmt --all -- --check`;`cargo clippy --all-targets -- -D warnings`
  与 `-p agent-testkit`(两 crate 均干净);`cargo test -p agent-testkit`(lib 42 含 4 个新 scope 用例
  + smoke 2);`cargo test --all --all-targets`(agent-lib 434 unit + 集成套件全绿,testkit 42 + 2,
  0 failed,7 个 network-gated ignored);`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` 与
  `-p agent-testkit`(修掉一处对已导入 `HandlerScope` 的 redundant explicit link 后干净);
  `git diff --check` 干净。
- **偏离计划**:无。scope 层为纯新增模块,未改 `agent-lib` 运行时语义,未引入 provider wire mock。
  wrapping 测试用内层 `TestScope`(而非 `ReferenceScope`)演示委派,因 `ReferenceScope` 需 `LlmClient`
  fake 而 testkit 刻意不 mock `LlmClient`;两者走同一 `Arc<dyn HandlerScope>` 委派路径。

### [DONE] M2-4 实现 `ScriptMachine` machine double

**前置依赖**:M2-3。

**上下文**:`drive.rs`、`subagent/tests.rs`、`agent_effect_e2e.rs` 都有 ad hoc batch machine。需要统一一个小型 machine double,用于测 driver/pop/subagent 机制。

**做什么**:

- 在 `machine.rs` 实现 `ScriptMachine: AgentMachine`。
- external input 后吐固定 requirement batch,并把 cursor 设为可被 `drain` 识别的非 terminal waiting state。
- 按 requirement id 记录 resume order、resume result tags、abandon count。
- 支持所有 outstanding resume 后进入 `LoopCursor::Done`。
- 支持 abandon 后进入 `Idle` 或按 builder 设定进入其他 cursor。
- 提供 builder:requirements、done_after_all_resumed、idle_on_abandon、initial cursor、label。

**验证**:

- 单测:batch requirements 被 emit,且 out-of-order resume 能完成。
- 单测:unknown resume id 产生可诊断状态或错误 cursor。
- 单测:abandon 行为可配置。
- 用 `drain` + `TestScope` 做一个 local tool fulfillment smoke。
- 跑全套验证命令。

**完成记录**(2026-07-14):

- **`ScriptMachine` + `ScriptMachineBuilder`**:`crates/agent-testkit/src/machine.rs` 实现二者,替代
  `drive.rs`/`subagent/tests.rs`/`agent_effect_e2e.rs` 里的三个 ad hoc batch machine。`impl AgentMachine`:
  - `StepInput::External` → 把 `outstanding` 重置为 batch 的 id 集合,cursor 设为非 terminal 的 `waiting_cursor`
    (默认 `LoopCursor::streaming_step`,固定 step id `DEFAULT_WAITING_STEP_ID`),emit 整个 batch(quiescent)。
  - `StepInput::Resume` → 先向共享 log 记录 `(id, result.tag())`;若 id 命中 `outstanding` 则移除,`outstanding`
    空且 `done_after_all_resumed` 时 cursor→`Done(Completed)`;若 id 不在 batch,cursor→`Error`(诊断信息含
    label 与未知 id),不动 `outstanding`——stray result 不被吞掉。
  - `StepInput::Abandon` → `abandon_count` +1;若配了 `abandon_cursor` 则 cursor 设为该值,否则保持不变。
- **共享观察 log**:`ScriptMachineLog`(`Arc` 共享,`Mutex<Vec<..>>` + `AtomicUsize` interior mutability)记录
  `resume_order()`/`resume_tags()`/`resume_count()`/`abandon_count()`。测试在把 machine 交给 driver(或作为
  nested child machine)前 clone 一份 log,drive 结束后仍可读——满足 docs §5.7「支持嵌套测试中的 child machine」。
  `ScriptMachine` 亦提供 `log()`/`outstanding()`/`resume_order()`/`resume_tags()`/`abandon_count()`/`label()`。
- **builder 五 knob**:`requirements(iter)`(extend)/`requirement(one)`;`done_after_all_resumed()`(opt-in,
  未调用则永不 terminal,供手动 `step` 驱动);`idle_on_abandon()`(= `abandon_cursor(Idle)` 的语义别名)与更
  通用的 `abandon_cursor(LoopCursor)`(覆盖 ParentBatchMachine 的 abandon→Done 行为);`initial_cursor(LoopCursor)`
  (设 waiting cursor,默认 streaming_step);`label(into String)`。`ScriptMachine::builder()` 入口。
- **prelude**:追加导出 `ScriptMachine`/`ScriptMachineBuilder`/`ScriptMachineLog`。
- **测试**(`machine.rs` 内 5 个单测):`emits_the_batch_and_completes_on_out_of_order_resume`(NeedTool+
  NeedInteraction 混合 batch,先 resume interaction 再 tool,验 batch 全 emit、resume_order/tags 按 resume 序、
  最终 `Done`);`unknown_resume_id_moves_to_an_error_cursor`(stray id → `Error` cursor,原 requirement 仍
  outstanding);`abandon_behaviour_is_configurable`(idle_on_abandon→`Idle`;无配置→cursor 不变;
  `abandon_cursor(Done)`→`Done`;各 abandon_count=1);`drain_fulfils_a_local_tool_batch_through_a_test_scope`
  (`drain` + `TestScope` 挂 `ScriptedToolHandler` 本地 fulfill NeedTool,跑到 `Done`,tool_log len 1,machine log
  resume 记录正确);`log_starts_empty`。
- **验证结果**(全绿):`cargo fmt --all -- --check`;`cargo clippy --all-targets -- -D warnings` 与
  `-p agent-testkit --all-targets`(两 crate 均干净);`cargo test -p agent-testkit`(lib 47 含 5 个新 machine 用例
  + smoke 2);`cargo test --all --all-targets`(agent-lib 434 unit + 集成套件全绿,testkit 47 + 2,0 failed,
  7 个 network-gated ignored);`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` 干净;
  `git diff --check` 干净。
- **偏离计划**:无。machine 层为纯新增模块,未改 `agent-lib` 运行时语义,未引入 provider wire mock。既有三个
  ad hoc batch machine 的实际删除/迁移由后续 M2-R 标记、M6-* 执行,本任务只交付统一 double。

### [DONE] M2-R Milestone 2 Review

**前置依赖**:M2-1..M2-4。

**上下文**:确认 scripted 层可以替代现有重复 fake,且没有掩盖 effect handler 不变量。

**做什么**:

- 核对 handler result family 是否正确。
- 核对 `TestScope` 默认不 total。
- 核对 `ScriptMachine` 能覆盖 driver/pop/subagent 测试所需语义。
- 标记优先迁移目标:至少列出 `tests/agent_effect_e2e.rs` 与 `src/agent/drive/reference/tests.rs` 中可删除的 fake 类型。

**验证**:

- 全套验证命令全部通过。
- Review 结论写入完成记录。

**完成记录**(2026-07-14):

- **核对 1 — handler result family 正确**(通过):`crates/agent-testkit/src/handlers.rs` 四个 scripted
  handler 的 `fulfill` 都经各自 `ScriptStep::into_result()` 只产出对应 family 的 `RequirementResult`
  (`Llm`/`Tool`/`Interaction`/`Reconfig`);script 耗尽(`StrictMode::Error`)按 family **折叠为族内
  `Err`**——LLM→`Llm(Err(ClientError::Other))`、Tool→`Tool(Err(ToolRuntimeError::ExecutionFailed))`、
  Reconfig→`Reconfig(Err(ToolRuntimeError::InvalidRegistry))`,绝不用 wrong family 表达失败。interaction
  family 无 `Err`,`ScriptedInteractionHandler` 反应式产 in-family 响应并寻址活 request 的 `step_id`/`call_id`。
  `MisalignedHandler` 专门返回 wrong-family 结果以触发 `drain` 的 `RequirementKind::accepts` 校验失败。相应单测
  (`*_stay_in_the_*_family`、`misaligned_handler_trips_the_drain_family_check` 等)全绿。**未发现掩盖 effect
  handler 不变量的情况。**
- **核对 2 — `TestScope` 默认不 total**(通过):`scope.rs` 每个 accessor 顺序为「本层 override → wrapping
  inner → `None`」;未挂 handler 且无 inner 即返回 `None`,故顶层缺 handler 的 family 会一路 pop 成
  `UnhandledRequirement`,不会被静默兜底。特别地未调 `.interaction(..)` 的 scope 为 headless。
  `headless_top_scope_surfaces_unhandled_interaction` 断言顶层 `NeedInteraction` 返回
  `AgentError::UnhandledRequirement { kind: Interaction }` 且 scripted tool 未被 auto-approve/跳过。**确认默认
  不 total。**
- **核对 3 — `ScriptMachine` 覆盖 driver/pop/subagent 语义**(通过):`machine.rs` 的 `step`:External 吐固定
  requirement batch 并停在**非 terminal** waiting cursor(`LoopCursor::streaming_step`,drain 可识别);Resume
  记录 order/tag、移除 outstanding、全清且 `done_after_all_resumed` 时→`Done(Completed)`;unknown resume id→
  诊断 `Error` cursor(不吞 stray);Abandon 计 `abandon_count` 且可配 `abandon_cursor`(含 `idle_on_abandon`)。
  requirements 为任意 `Vec<Requirement>`,可含 `NeedTool`/`NeedInteraction`/`NeedSubagent`,故能覆盖
  `agent_effect_e2e.rs` 的 `ParentBatchMachine`(batch=NeedTool+NeedSubagent、resume→Done、abandon→Done,
  用 `abandon_cursor(Done)`+`done_after_all_resumed` 复现)与 driver/pop/subagent 机制,符合 docs/TESTABILITY.md
  §5.7 验收(替代三处 ad hoc batch machine、明确表达 out-of-order resume)。共享 `Arc<ScriptMachineLog>` 支持
  machine 移入 driver / 作 nested child 后仍读回观察。**确认语义足够。**
- **优先迁移目标(可删除 fake 清单)**——实际删除/迁移在 M6-1(`agent_effect_e2e.rs`)、M6-2(reference driver
  测试)执行,此处仅标记:
  - `tests/agent_effect_e2e.rs`(M2 层可替换):`SeqIds`→testkit `SeqIds`;`EmptyScope`→`TestScope::empty()`;
    `ParentScope`→`TestScope::builder().tool/.interaction/.subagent`;`ParentBatchMachine`→`ScriptMachine`
    (M2-4 headline 目标);`CountingApproveInteraction`→`ScriptedInteractionHandler::approve_all()`+CallLog 计数;
    `CountingToolHandler`→`ScriptedToolHandler`(固定 `ToolStep::ok`)+`ToolCallLog` 计数;
    `FakeToolRegistry`→`ScriptedToolRegistry`;`ChildScope`/`ObservingScope`→`TestScope::builder()`(可 `wrapping`);
    payload 助手(`weather_tool`/`text_block`/`user_message`/`usage`/`assistant_response`→`assistant_text`/
    `tool_use_response`→`assistant_tool_use`/`tool_response`/`agent_id`/`tool_set_id`/`conversation_id`)→ testkit
    fixtures + `SeqIds` accessor。
  - `src/agent/drive/reference/tests.rs`(M2 层可替换):`ScriptedRequirementIds`+`FakeToolIds`→testkit `SeqIds`
    (`RequirementIds`+`ToolExecutionIds`);`FakeToolRegistry`→`ScriptedToolRegistry`(经 reference
    `ToolRegistryHandler`);`ScriptedApprovalInteraction`→`ScriptedInteractionHandler`(反应式 approve/deny 决策);
    `ComposedScope`→`TestScope::builder().interaction(..).wrapping(Arc<ReferenceScope>)`;fixture 助手
    (`weather_tool`/`calendar_tool`/`usage`/`assistant_response`/`tool_use_response`/`tool_response`/`user_message`/
    `spec`)→ testkit fixtures。
  - **明确延后(非 M2 层,后续里程碑提供)**:`FakeClient`(`LlmClient`——testkit 刻意不 mock 协议层 client;
    reference/tests.rs 的 `FakeClient` 为 `ReferenceScope` 适配器测试所必需,保留);`ConcurrentToolHandler`
    (peak 并发工具 → M5-1);`ChildSpawner`(`SubagentSpawner` → M5-3);`CancellingLlmHandler`/`PanicToolHandler`/
    `CancelScope`(cancel/panic wrappers → M5-2);`RequireApprovalPolicy`(`ToolApprovalPolicy`,两文件重复,当前
    testkit 无对应型,可作后续 fixture 候选);`ChargingLlmHandler`(token 计费 LLM handler,迁移与否取决于计费边界,
    M6 判定)。
- **验证结果**:`cargo fmt --all -- --check` 干净;`cargo clippy --all-targets -- -D warnings` 干净(root +
  testkit);`cargo test -p agent-testkit` 全绿(lib 47 + smoke 2,0 failed)——覆盖三项核对对应的单测。全套
  `cargo test --all --all-targets` 自 M2-4(HEAD=`0875598`)绿后**无任何代码/清单改动**,本 Review 任务仅改
  `TODO.md`/`memory/claude_plan.md` 文档,按「仅文档变更可复用上次绿结果」规则跳过重跑(上次结果:agent-lib 434
  unit + 集成套件全绿,testkit 47 + 2,0 failed,7 个 network-gated ignored)。
- **Review 结论**:Milestone 2 交付的 scripted 层(script model/handlers/scope/machine)可替代现有重复 fake,
  且三项关键不变量(family 对齐、scope 默认不 total、machine 覆盖 driver/pop/subagent)均成立、未被掩盖。可进入
  Milestone 3。未发现需插入前置修复任务的 spec 偏差或失败测试。

---

## Milestone 3 — Cassette 录制与离线重放

### [DONE] M3-1 定义 cassette schema、redactor 与 fingerprint

**前置依赖**:M2-R。

**上下文**:cassette 用于记录真实 agent effect req/resp,供 CI 离线 replay。它是 provider-neutral fixture,不是协议层 HTTP fixture。

**做什么**:

- 在 `cassette.rs` 定义 `Cassette`,包含 schema version、metadata、entries、optional observations。
- 定义 entry 类型:`LlmEntry`、`ToolEntry`、`InteractionEntry`、`ReconfigEntry` 或统一 tagged enum。
- 定义 request fingerprint 函数:canonical JSON + hash。首版可用稳定 JSON 字符串作为 fingerprint,后续再换 hash。
- fingerprint 默认忽略 volatile ids:RequirementId、TraceNodeId、测试分配的 MessageId/StepId 等。
- 定义 `Redactor` trait 与 `DefaultRedactor`。默认保留 message 文本,但 redacts provider extras 中未知字段。
- 定义 schema version 常量,未知版本 deserialize 时分类失败。

**验证**:

- 单测:cassette JSON round-trip 稳定。
- 单测:相同 logical request 在不同 volatile id 下 fingerprint 一致。
- 单测:redactor 会处理 provider extras 未知字段。
- 单测:未知 schema version 失败。
- 跑全套验证命令。

**完成记录**(2026-07-14):

- **产出**:`crates/agent-testkit/src/cassette.rs`(全量实现,替换原 skeleton stub)+ `prelude.rs` 追加导出。
- **schema**:`Cassette { schema_version, metadata: CassetteMetadata, entries: Vec<CassetteEntry>, observations:
  Option<CassetteObservations> }`;常量 `CASSETTE_SCHEMA_VERSION = 1`。`CassetteEntry` 为 internally-tagged
  (`tag = "family"`, snake_case)enum,含 `Llm(LlmEntry)`/`Tool(ToolEntry)`/`Interaction(InteractionEntry)`/
  `Reconfig(ReconfigEntry)`。每个 entry 记 `index`(dispatch 序号)、normalized request、`fingerprint`、
  normalized result(`LlmOutcome`/`ToolOutcome`/`InteractionResponse`/`ReconfigOutcome`)、可选 `summary`。
  entry 构造器自动算 fingerprint。M3-1 **不含** Subagent 家族(其结果 `AgentError` 属 M5)。
- **可序列化结果镜像**:`ToolRuntimeError` 不可序列化 → testkit 内新增 `CassetteToolError`(5 变体镜像,
  `tag = "kind"`)+ 双向 `From` + `Display`(委托 `ToolRuntimeError`);**未改动 agent-lib**,与 `ClientError`
  本已可序列化的先例一致。`LlmOutcome`/`ToolOutcome`/`ReconfigOutcome` 用 serde 默认(external)tag 避免包裹
  枚举时的 internal-tag 限制。
- **fingerprint**:`request_fingerprint<T: Serialize>` → `to_value` → 递归 canonicalize(对象 key 排序 + volatile-id
  key 的字符串值替换为 `<volatile-id>`)→ compact JSON 字符串(v1 直接用 canonical 串)。volatile key 集合含
  `id/tool_call_id/tool_use_id/call_id/step_id/message_id/requirement_id/trace_node_id`;`input`/`input_schema`
  子树视为 opaque(仍排序 key,但不 strip id),保护 tool 输入内语义化的 `id`。
- **redactor**:`Redactor` trait(每 family 一个默认 no-op 方法)+ `DefaultRedactor`(带 allowlist):scrub
  `ChatRequest.provider_extras.fields` 与 `Response.extra` 中非 allowlist key 的值为 `<redacted>`,保留 key 形状与
  message 文本。
- **schema 加载护栏**:`Cassette::from_json_str` 先读并校验 `schema_version` 再 full parse,未知/缺失分别分类为
  `CassetteError::UnsupportedSchemaVersion` / `MissingSchemaVersion`;另有 `to_json_string[_pretty]`。
- **单测(11 个,全绿)**:JSON round-trip 稳定(compact + pretty);同 logical request 不同 volatile id →
  fingerprint 一致,改 tool input / input 内 `id` → fingerprint 变化;key 乱序 fingerprint 稳定;volatile id 被替换为
  占位符;`DefaultRedactor` scrub provider extras / response extra 且保留 allowlist 与 message 文本;未知/缺失
  schema version 分类;family tag + index 暴露;`CassetteToolError` ↔ `ToolRuntimeError` round-trip。
- **验证**:`cargo fmt --all`;`cargo clippy --all-targets -- -D warnings`(clean);`cargo test -p agent-testkit
  cassette`(11 passed);`cargo test --all --all-targets`(全绿,含 agent-testkit 58 + smoke 2);
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p agent-testkit`(clean,`#![warn(missing_docs)]` 满足);
  `git diff --check`(clean)。
- **边界确认**:cassette 仅记 provider-neutral effect req/resp,未含 header/auth/endpoint/provider raw body;
  replay handlers 与 record/verify/update wrapper 分别属 M3-2 / M3-3,本任务未实现。

### [DONE] M3-2 实现 cassette replay handlers

**前置依赖**:M3-1。

**上下文**:replay 模式不调用真实 handler,只按 cassette 返回记录结果。请求不匹配必须给出清晰错误。

**做什么**:

- 实现 `CassetteLlmHandler: LlmHandler`。
- 实现 `CassetteToolHandler: ToolHandler`。
- 实现 `CassetteInteractionHandler: InteractionHandler`。
- 实现 `CassetteReconfigHandler: ReconfigHandler`。
- 每个 handler 按 family + 顺序 + request fingerprint 匹配 entry。
- mismatch 错误包含 cassette path/label、entry index、family、expected fingerprint、actual fingerprint、请求摘要。
- replay handler 与 scripted handler 共享 call log 风格。

**验证**:

- 单测:replay 按顺序返回记录结果。
- 单测:request mismatch 报错信息包含 entry index 与 fingerprint。
- 单测:replay 不调用任何真实 handler。
- 跑全套验证命令。

**完成记录**(2026-07-14):

- **模块化**:`cassette.rs` → `cassette/mod.rs`(schema/fingerprint/redactor 原样保留) + 新增
  `cassette/replay.rs`(replay handlers);`mod.rs` 加 `mod replay; pub use replay::{...}`,公共路径仍是
  `crate::cassette::CassetteLlmHandler` 等,兑现 M3-1 doc "replay 扩展 crate::cassette"。`prelude` 追加导出。
- **四个 replay handler**:`CassetteLlmHandler`/`CassetteToolHandler`/`CassetteInteractionHandler`/
  `CassetteReconfigHandler`,分别实现 `LlmHandler`/`ToolHandler`/`InteractionHandler`/`ReconfigHandler`。每个持
  该 family 的 `Vec<*Entry>`(构造时按 family 过滤克隆)+ `label: Arc<str>` + `Mutex<usize>` cursor + 复用
  `crate::handlers` 的 `LlmCallLog`/`ToolCallLog`/`InteractionCallLog`/`ReconfigCallLog`(与 scripted handler 同
  一 call log 风格,begin/complete)。
- **匹配**:泛型 `ReplayCursor<E>`,算 `request_fingerprint(request)`(忽略 volatile ids),取 cursor 处 entry;
  仅在 fingerprint 相等时 advance,divergent 请求每次都报同一 expected entry。replay 本身是终端 handler,无
  delegate,故"不调用真实 handler"天然成立。
- **`CassettePlayer`**:持 `Arc<Cassette>` + label,`.llm_handler()/.tool_handler()/.interaction_handler()/`
  `.reconfig_handler()` 各造独立 handler,便于整轮 replay(M3-4)。
- **mismatch 清晰错误**:`ReplayMismatch`(pub,含 `kind`/`label`/`family`/`entry_index`/`family_position`/
  `expected_fingerprint`/`actual_fingerprint`/`request_summary`,accessors + `Display`)+ `ReplayMismatchKind`
  (`Fingerprint`/`Exhausted`)。遵循 kit 的 family-alignment:LLM →
  `Llm(Err(ClientError::Other(msg)))`,Tool → `Tool(Err(ExecutionFailed{tool_name,message}))`,Reconfig →
  `Reconfig(Err(InvalidRegistry{message}))`;Interaction 家族 `InteractionResponse` 无 Err 变体,无法 in-band
  折叠 → **panic**(与 `ScriptedInteractionHandler` 的 panic 语义一致)。`Err` 变体经 `Box` 满足
  `clippy::result_large_err`。
- **单测(10 个,全绿,`crates/agent-testkit/src/cassette/replay.rs`)**:按序返回记录结果(user→tool_use→tool→
  final text 整轮)、volatile-id 差异仍匹配、LLM fingerprint mismatch 折叠且报错含 label/entry #0/expected+actual
  fingerprint、LLM 耗尽报错清晰、Tool mismatch 折叠 `ExecutionFailed` 含 index+fingerprint、Tool 记录的
  runtime error 原样重放、Reconfig Ok 重放 + mismatch 折叠 `InvalidRegistry`、Interaction 记录响应重放、
  Interaction mismatch `#[should_panic]`、player/mismatch accessors 暴露。
- **验证**:`cargo fmt --all --check`(clean);`cargo clippy -p agent-testkit --all-targets -- -D warnings`
  (clean);`cargo test -p agent-testkit replay`(10 passed);`cargo test --all --all-targets`(全绿:agent-lib
  434 + agent-testkit 68 + smoke 2,0 failed);`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p agent-testkit`
  (clean,`#![warn(missing_docs)]` 满足);`git diff --check`(clean)。
- **边界确认**:record/verify/update wrapper(M3-3)与首个离线 recorded replay 测试(M3-4)本任务未实现;replay
  handler 仅消费 provider-neutral cassette entry,无 header/auth/endpoint/provider raw body。

### [DONE] M3-3 实现 record / verify / update wrapper

**前置依赖**:M3-2。

**上下文**:record/update 会调用真实 handler,因此默认不能在 CI 中意外运行或覆盖 fixture。verify 模式用于真实 handler 输出和 cassette 对比。

**做什么**:

- 实现 `CassetteRecorder` builder,支持 `record(path)`、`verify(path)`、`update(path)`。
- 提供 wrappers:wrap llm/tool/interaction/reconfig handler,调用真实 handler 后记录或比较 entry。
- update 必须检查显式环境变量,例如 `AGENT_TESTKIT_UPDATE_CASSETTES=1`。
- record 也应显式 opt-in,例如 `AGENT_TESTKIT_RECORD_CASSETTES=1`,否则测试返回 skipped/ignored 风格错误。
- 写入 cassette 时使用临时文件 + atomic rename,避免半写文件。

**验证**:

- 单测:update 未启用环境变量时不会写文件。
- 单测:record 通过 redactor 后写出稳定 JSON。
- 单测:verify 模式 detect result drift。
- 跑全套验证命令。

**完成记录**(2026-07-14):

- **模块化**:新增 `cassette/record.rs`(record/verify/update wrapper),`cassette/mod.rs` 加 `mod record; pub use
  record::{...}` 并更新顶部 doc(新增 `# Record` 段,标注 M3-3 已落地)。公共路径为 `crate::cassette::CassetteRecorder`
  等;`prelude` 追加导出。schema/fingerprint/redactor(M3-1)与 replay(M3-2)原样保留。
- **`CassetteRecorder` builder**:`record(path)` / `verify(path)` / `update(path)` 三个构造器 → `RecorderMode`
  枚举(`Record`/`Verify`/`Update`,`env_var()`/`writes()` 辅助)。builder 方法 `with_redactor<R: Redactor>` /
  `with_metadata` / `with_enabled_override`(显式测试钩子,绕过 env gate)。accessors `path()`/`mode()`/`is_enabled()`/
  `skip_reason()`。
- **env 护栏**:`RECORD_ENV_VAR = "AGENT_TESTKIT_RECORD_CASSETTES"`、`UPDATE_ENV_VAR =
  "AGENT_TESTKIT_UPDATE_CASSETTES"`;写模式仅当 override 为真或对应 env 值 `"1"` 时 `is_enabled()`;Verify 永远启用
  (从不写盘)。`finish()` 对未启用的写模式返回 `Ok(RecorderReport::Skipped)` 且不写任何文件(防御:即便未经
  `is_enabled()` gate 也不会误写)。
- **四个 recording wrapper**:`RecordingLlmHandler`/`RecordingToolHandler`/`RecordingInteractionHandler`/
  `RecordingReconfigHandler`,分别实现对应 handler trait:先调真实 inner handler,再对该 family 结果经 `Redactor`
  归一化(request 与 Ok 结果都脱敏)→ 按共享 `RecorderState`(`Mutex<Vec<CassetteEntry>>`)的全局 dispatch 序号
  `push`(index=当前 len,锁内原子选号+追加)→ 原样返回 inner 结果。指纹在**脱敏后**的 request 上计算(与
  `docs/TESTABILITY.md` §5.4 "provider_extras 的脱敏后 canonical 形状" 一致)。
- **finish 语义**:`finish() -> Result<RecorderReport, RecorderError>`。写模式(启用)→ 构建 `Cassette`(metadata +
  累积 entries),pretty JSON,`write_atomic`(同目录 `.name.tmp.pid.nanos.n` → write → rename,失败清理临时文件;
  必要时 `create_dir_all`)→ `Wrote{path, entry_count}`。Verify → 读盘 `Cassette::from_json_str`,与 live 累积 entries
  逐位比对,无漂移 → `Verified{entry_count}`,否则 `Err(Drift(Vec<EntryDrift>))`。`EntryDrift{position, family,
  detail}` 分类 family / request-fingerprint / result / 计数(missing/unexpected)漂移;`RecorderError{Drift, Load,
  Serialize, Io}` 实现 `Display`/`Error`(size 远低于 clippy `result_large_err` 阈值,无需 Box)。
- **单测(6 个,全绿,`crates/agent-testkit/src/cassette/record.rs`)**:`update_is_disabled_without_env_var`(默认
  未启用 + skip_reason 含 env 名)、`update_without_env_var_writes_no_file`(finish → Skipped 且文件不存在)、
  `record_writes_stable_redacted_json`(un-allowlist 的 provider_extras/response.extra 值 → `<redacted>`,allowlist
  的 `keep` 保留,parse→re-serialize pretty 与文件字节一致 = 稳定 JSON)、`verify_detects_result_drift`(同指纹不同
  结果 → 一条 position #0 的 result drift)、`verify_passes_when_results_match`(→ Verified{1})、
  `update_overwrites_existing_cassette`(覆盖而非追加)、`records_all_families_in_dispatch_order`(共享累积器把
  llm/tool/interaction/reconfig 混合轮次记为全局序 0..3)。用自删除 `TempPath` guard,唯一名避免并行冲突;测试**不**
  修改进程级 env(用 override 走写路径,依赖 env 缺省走禁用路径),避免 flaky。
- **验证**:`cargo fmt --all`;`cargo clippy -p agent-testkit --all-targets -- -D warnings`(clean,修正一处
  `collapsible_if` → let-chain);`cargo test -p agent-testkit record`(12 filtered-in,全绿);`cargo test --all
  --all-targets`(全绿:agent-lib 434 + agent-testkit 75 + smoke 2 + 其余,0 failed);`RUSTDOCFLAGS="-D warnings"
  cargo doc --no-deps -p agent-testkit`(clean,`#![warn(missing_docs)]` 满足);`git diff --check`(clean)。
- **边界确认**:recorder 仅捕获 provider-neutral effect req/resp(无 header/auth/endpoint/provider raw body);写盘仅
  在显式 env/override opt-in 时发生且原子;首个离线 recorded replay 测试(M3-4)本任务未实现。

### [DONE] M3-4 增加首个离线 recorded replay 测试


**前置依赖**:M3-3。

**上下文**:需要一个最小但真实形状的 cassette,证明 CI 不联网也能跑完整 agent turn。cassette 可以先用 synthetic recorded data,但格式必须和 recorder 输出一致。

**做什么**:

- 新增 `tests/cassettes/agent_weather_tool_roundtrip.json` 或 testkit crate 下等价路径。
- cassette 覆盖 user -> LLM tool_use -> tool result -> LLM final text。
- 新增 replay 测试,用 `DefaultAgentMachine` + `CassetteLlmHandler` + `CassetteToolHandler` 跑完整 turn。
- 断言 committed conversation、handler call log、final cursor。
- 确认该测试无需网络、credentials、真实 tool backend。

**验证**:

- 聚焦运行 recorded replay 测试。
- 全套验证命令全部通过。
- 人工检查 cassette JSON 可读,无 auth/endpoint/raw provider body。

**完成记录**(2026-07-14):

- **产出**:新增集成测试 `crates/agent-testkit/tests/cassette_replay.rs`(testkit crate 下等价路径,而非仓库根
  `tests/`,契合 dev-only testkit 拓扑)+ 首个 recorded fixture
  `crates/agent-testkit/tests/cassettes/agent_weather_tool_roundtrip.json`(pretty JSON,`schema_version=1`,3 entry:
  llm#0 → tool#1 → llm#2)。
- **replay 测试 `offline_replay_runs_a_full_weather_turn`**:运行时 `std::fs::read_to_string` 读 fixture(非
  `include_str!`,以便 fixture 未生成时仍可编译并跑 regenerate)→ `Cassette::from_json_str` →
  `CassettePlayer::new` → `llm_handler()`/`tool_handler()`(各持 `Arc` 便于读 `log()`)→ `TestScope`(**仅** llm+tool,
  headless,任何 stray interaction 会以 `UnhandledRequirement` 暴露而非静默 auto-approve)→ `default_machine`(真实
  `DefaultAgentMachine`,NonStreaming,无 approval policy)→ `drain` 跑完整 turn。断言三项:**final cursor**
  `done.cursor().kind() == LoopCursorKind::Done`;**handler call log** llm 2 次(且 completed 2)、tool 1 次(且
  completed 1);**committed conversation** `pending().is_none()`、1 turn、4 消息(User / Assistant `ToolUse{get_weather}`
  / Tool / Assistant `Text` 最终答复),且第 4 条文本 == 预期终答。整条链无 `LlmClient`、无 provider HTTP、无真实 tool
  backend、无 credentials。
- **fixture 生成不靠手写**:同文件的 `regenerate_weather_cassette` 用 M3-3 `CassetteRecorder::update(path)` 包
  `ScriptedLlmHandler`/`ScriptedToolHandler`,跑一遍**同一个真实机器**并 record 到磁盘——这样录制的 LLM 请求正是机器
  实际发出的请求,entry 指纹与 replay 时机器重放的请求天然一致(volatile id 被 fingerprint strip,record/replay 两次
  run 无需 id 相同)。该测试受 `AGENT_TESTKIT_UPDATE_CASSETTES=1` 门禁:默认 CI run `skip_reason()` 命中即早退(断言其
  含 env 名),**永不覆盖已提交 fixture**;opt-in 时写盘并断言 `RecorderReport::Wrote{entry_count: 3}`。本任务即以
  `AGENT_TESTKIT_UPDATE_CASSETTES=1` 跑一次该测试生成committed fixture。
- **人工检查 JSON**:provider-neutral,仅含 effect-boundary `ChatRequest`/`Response`、`ToolCall`/`ToolResponse`;**无**
  header/auth/endpoint/provider raw wire body;`provider_extras: null`;每 entry 带自算 `fingerprint`(tool 请求与第二个
  LLM 请求中的 `id`/`tool_use_id` 已被替换为 `<volatile-id>`)。
- **边界修正**:初版误加 budget 断言(`ctx.budget().used()==17`);实测为 0,因 budget charge 是 **handler 职责**(参考
  `ChargingLlmHandler` 显式 `ctx.charge_tokens`),cassette replay handler 与 scripted handler 均不 charge。已删除该错误
  断言与相应注释;录制 usage 仅作为真实形状 payload 保留(JSON 中可见)。
- **验证**:`cargo fmt --all`(+ `--check` clean);`cargo clippy --all-targets -- -D warnings`(workspace clean);
  聚焦 `cargo test -p agent-testkit --test cassette_replay`(2 passed:regenerate no-op skipped + offline replay 绿);
  `cargo test --all --all-targets`(全绿:agent-lib 434 + agent-testkit 75 + cassette_replay 2 + smoke 2 + 其余,0
  failed);`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p agent-testkit`(clean);`git diff --check`(clean)。


### [DONE] M3-R Milestone 3 Review

**前置依赖**:M3-1..M3-4。

**上下文**:确认 cassette 能支撑真实 req/resp 复用,且没有把协议层 fixture 混入 agent testkit。

**做什么**:

- 核对 cassette schema 是否 provider-neutral。
- 核对 redactor 默认策略。
- 核对 record/update 环境变量护栏。
- 核对 replay 测试在无 credentials 环境可跑。
- 更新 `docs/TESTABILITY.md` 中任何与实现不一致的 cassette 描述。

**验证**:

- 全套验证命令全部通过。
- Review 结论写入完成记录。

**完成记录**(2026-07-14):

- **核对 1 — cassette schema provider-neutral**(通过):`crates/agent-testkit/src/cassette/mod.rs` 的
  `Cassette { schema_version, metadata, entries, observations }` 只承载 effect 边界的 provider-neutral 类型——
  request 侧 `ChatRequest`/`ToolCall`/`Interaction`/`ToolSetRef`,result 侧 `Response`/`ToolResponse`/
  `InteractionResponse`/`ReconfigOutcome`(以及可序列化镜像 `CassetteToolError`)。**无** HTTP header、auth token、
  base URL、provider raw wire body;`ToolRuntimeError` 的镜像在 testkit 内定义,未改 agent-lib。已提交 fixture
  `tests/cassettes/agent_weather_tool_roundtrip.json` 实测 `provider_extras: null`、仅 effect payload,证实录制点在
  handler effect 边界而非协议层。**确认 provider-neutral,未混入协议 fixture。**
- **核对 2 — redactor 默认策略**(通过):`DefaultRedactor` 把 `ChatRequest.provider_extras.fields` 与
  `Response.extra` 中所有**非白名单** key 的**值**替换为 `<redacted>`,**保留 key 形状**与 message 文本;allowlist
  默认空(默认脱敏一切 extras 值),`allow_field(..)` 显式放行。指纹在**脱敏后**的 request 上计算(与 §5.4 一致)。
  相应单测(`record_writes_stable_redacted_json`、mod 内 redactor round-trip)全绿。**确认默认保守、可显式放行。**
- **核对 3 — record/update 环境变量护栏**(通过):`RECORD_ENV_VAR = "AGENT_TESTKIT_RECORD_CASSETTES"`、
  `UPDATE_ENV_VAR = "AGENT_TESTKIT_UPDATE_CASSETTES"`;`Record`/`Update` 仅当显式 override 为真或对应 env 值
  `"1"` 时 `is_enabled()`,`Verify` 从不写盘;`finish()` 对未启用的写模式返回 `RecorderReport::Skipped` 且**不写任何
  文件**(双重防御)。写盘走临时文件 + atomic rename。集成测试 `regenerate_weather_cassette` 在默认(无 env)run 下命中
  `skip_reason()`(断言含 env 名)并早退,**永不覆盖已提交 fixture**。**确认 CI 默认不会意外录制/覆盖。**
- **核对 4 — replay 无 credentials 可跑**(通过):replay handler(`CassetteLlmHandler` 等)是**终端** handler,无
  delegate、不触网、不调用真实 backend,仅按 family + 顺序 + fingerprint 返回记录结果。集成测试
  `offline_replay_runs_a_full_weather_turn` 用真实 `DefaultAgentMachine` + `CassettePlayer` 铸的 llm/tool handler 跑完
  整 user→tool_use→tool→final-text turn,无 `LlmClient`、无 HTTP、无真实 tool、无 credentials。本次聚焦跑
  `cargo test -p agent-testkit --test cassette_replay`(2 passed:regenerate 默认 Skipped + 离线 replay 绿)与
  `cargo test -p agent-testkit cassette`(28 + 1 passed)实证以上四项。**确认离线可跑。**
- **文档并轨(`docs/TESTABILITY.md` §5.4/§6 cassette 描述,与实现对齐)**:
  - 建议 API / 录制-重放示例:`Cassette::load(path)` → 实际 `std::fs::read_to_string(path)` + `Cassette::from_json_str(&json)`
    (无 `load` 方法);`CassetteLlmHandler::replay(cassette)` 等 → 实际 `CassettePlayer::new(cassette, label)` 的
    `.llm_handler()/.tool_handler()/.interaction_handler()`(或 `CassetteXHandler::from_cassette`);§5.4 recorder 示例把
    `wrap_llm(..)` 结果误命名为 `recorder`(实为 `RecordingLlmHandler`),已拆成 recorder + wrapped handler + `finish()`。
    §6 replay 示例补 `use std::sync::Arc;` 并把 handler 以 `Arc::new(..)` 传入 builder(匹配 `TestScopeBuilder::llm` 签名)。
  - 录制内容:删除实现里不存在的 `created_at` 与独立 "run inputs" section;明确 metadata 仅 `test_name`/可选
    `description`(承载 scenario label)/可选 `crate_version`,schema_version 在 cassette 顶层;observations 字段对齐
    (final cursor / notifications / conversation / trace dispositions)。
  - fingerprint:"canonical JSON hash" → "volatile-id 归一化后的 canonical JSON 串(v1 直接以该串为指纹,后续可换 hash)",
    与 `request_fingerprint` 实现及其 doc comment 一致。
  - 脱敏策略:"移除 auth/endpoint 类字段" → 准确表述为「脱敏所有非白名单 extras 字段的**值**、保留 key 形状」,并注明
    `DefaultRedactor::allow_field(..)` 白名单入口。
  - 匹配策略:标注"自定义 key 匹配"为后续扩展,当前实现只做 fingerprint 匹配。
- **验证结果**:`cargo fmt --all --check` 干净;聚焦 `cargo test -p agent-testkit cassette`(28 + 1,0 failed)与
  `cargo test -p agent-testkit --test cassette_replay`(2,0 failed)全绿,实证四项核对。全套
  `cargo test --all --all-targets` 自 M3-4(HEAD=`1946a77`)绿后**无任何代码/清单改动**(本 Review 仅改
  `docs/TESTABILITY.md` 与 `memory/claude_plan.md` 文档),按「仅文档变更可复用上次绿结果」规则跳过重跑(M3-4 上次
  结果:agent-lib 434 + agent-testkit 75 + cassette_replay 2 + smoke 2 + 其余,0 failed)。
- **Review 结论**:Milestone 3 交付的 cassette 层(schema/fingerprint/redactor、replay handler、record/verify/update
  wrapper、首个离线 recorded replay 测试)可支撑真实 req/resp 离线复用;四项关键属性(schema provider-neutral、redactor
  默认保守、record/update 受 env 护栏、replay 无 credentials 可跑)均成立,且未把协议层 fixture 混入 agent testkit。文档
  已与实现并轨。未发现需插入前置修复任务的 spec 偏差或失败测试,可进入 Milestone 4。

---

## Milestone 4 — Step/Drain harness 与断言库

### [DONE] M4-1 实现 `StepHarness`

**前置依赖**:M3-R。

**上下文**:许多基础测试需要手动检查每一步 requirements、notifications、cursor。`StepHarness` 应保持同步,不需要 async。

**做什么**:

- 在 `harness.rs` 实现 `StepHarness<M: AgentMachine>`。
- 支持 `external(input)`、`user(text)`、`pivot(...)`、`resume(id, result)`、`abandon(id)`。
- 每步返回 `StepObservation`,包含 notifications、requirements、quiescent、cursor snapshot。
- 提供 convenience extractor:single_llm、single_tool、single_interaction、requirements_by_tag。
- 错误/断言失败信息包含当前 cursor、outstanding ids、最近一步 label。

**验证**:

- 单测:用 `DefaultAgentMachine` 跑 text-only turn 的 step-by-step。
- 单测:wrong id resume 失败信息包含 cursor/outstanding id。
- 单测:`StepHarness` 本身不使用 async。
- 跑全套验证命令。

**完成记录(2026-07-14)**:

- 在 `crates/agent-testkit/src/harness.rs` 实现同步 `StepHarness<M: AgentMachine>`:
  - 步进 API:`external(input)`、`user(text)`、`pivot(text)`(`PivotSource::Human`)、`resume(id, result)`、`abandon(id)`;`user`/`pivot` 从注入的 `SeqIds` 取 turn/message/step id(`new` 自建、`with_ids` 复用机器同源 id 树)。
  - `resume`/`abandon` 提供 fallible 双胞胎 `try_resume`/`try_abandon`;非 `try_` 版本对错误 `panic!("{error}")`。校验在 harness 层前置:id 必须 outstanding,且 `Requirement::accepts_resolution` 家族对齐,校验失败**不步进**机器。
  - `StepObservation` 暴露 `label`/`notifications`/`requirements`/`is_quiescent`/`cursor`(`LoopCursor` 快照),以及提取器 `single`/`single_llm`/`single_tool`/`single_interaction`/`requirements_by_tag`。
  - `StepHarnessError` 的 `Display` 始终包含当前 cursor、outstanding ids、最近一步 label;提供 `message`/`cursor`/`outstanding`/`last_label` 访问器并实现 `std::error::Error`。harness 自行跟踪 outstanding 集合(pivot 以同一 requirement id 复发时刷新而非重复)。
- `prelude.rs` 追加 `StepHarness`、`StepHarnessError`、`StepObservation` 再导出。
- 单测(harness.rs `#[cfg(test)]`,7 个,全为普通 `#[test]` 证明无 async):text-only turn step-by-step 到 `Done`;tool-use 折叠出中间 `NeedTool` 被暴露;wrong-id resume 报文含 cursor(`StreamingStep`)+ outstanding 真 id + stray id + label 且机器未步进;wrong-family resume 前置拒绝;abandon stray 拒绝 + 真 id 清除;`single_tool` 缺家族诊断含期望/实际家族。
- 验证:`cargo fmt --all --check` 通过;`cargo clippy --all-targets -- -D warnings` 无告警;`cargo test -p agent-testkit harness` 7/7;`cargo test --all --all-targets` 全绿(agent-lib 434、agent-testkit 82 + 集成 4,0 失败);`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`(root + `-p agent-testkit`)通过;`git diff --check` 干净。

### [DONE] M4-2 实现 `DrainHarness`

**前置依赖**:M4-1。

**上下文**:`DrainHarness` 包装 `agent_lib::agent::drain`,用于 e2e 和 scenario 测试,但不应隐藏 `UnhandledRequirement`、trace failure、budget failure。

**做什么**:

- 在 `harness.rs` 实现 `DrainHarness`。
- 支持传入 machine、scope、optional parent pop、RunContext、input。
- 返回 `DrainObservation`:TurnDone、notifications、final cursor、可选 handler logs summary。
- 支持 `run_user(text)` convenience,但内部仍走 `AgentInput::user_message` 与 `SeqIds`。
- 错误直接返回 `AgentError`,不要转换成泛化字符串。

**验证**:

- 单测:local tool scripted turn drain to Done。
- 单测:top unhandled interaction 原样返回 `AgentErrorKind::UnhandledRequirement`。
- 单测:cancelled context 路径不触发 tool handler。
- 跑全套验证命令。

**完成记录(2026-07-14)**:

- 在 `crates/agent-testkit/src/harness.rs` 实现异步 `DrainHarness<'drive, M: AgentMachine>`,包装 `agent_lib::agent::drain`:
  - 拥有 machine(与 `StepHarness` 一致),借用 `scope: &dyn HandlerScope`、`parent: Option<&mut dyn Pop>`、`ctx: &RunContext`,并持有注入的 `SeqIds`(供 `run_user`)。`new` 自建 `SeqIds`、`with_ids` 复用机器同源 id 树。
  - 步进 API:`run(input)`(async,一次性 drain 到本轮结束)与 `run_user(text)` convenience(内部 `user_input(&ids, text)` → `AgentInput::user_message` → `run`)。
  - 错误路径:`run`/`run_user` 直接透传 `drain` 返回的 classified `AgentError`(unhandled requirement / trace failure / budget failure / family mismatch),**不**折叠为泛化字符串。
  - accessors:`machine`/`machine_mut`/`into_machine`(可在 drain 后检视已提交 conversation)/`ids`;`Debug` 概述 machine、cursor、是否有 parent、watched log 名称。
- `DrainObservation`:持 `TurnDone` + `Option<Vec<HandlerLogSummary>>`;暴露 `turn_done`/`into_turn_done`/`notifications`(转发)/`final_cursor`(转发)/`handler_logs`。
- 可选 handler logs summary:新增 object-safe `HandlerCallCounts: Send + Sync`(`begun`/`completed`),对 `CallLog<Req: Send, Res: Send>` 委托 `len()`/`completed_len()`;`DrainHarness::watching(name, Arc<dyn HandlerCallCounts>)` 注册任意家族(llm/tool/interaction/reconfig)的 `Arc<CallLog<..>>`(调用点 unsize 强转),drain 后按 name 汇总为 `HandlerLogSummary { name, begun, completed }`;未注册则 `handler_logs()` 返回 `None`。
- `prelude.rs` 追加 `DrainHarness`、`DrainObservation`、`HandlerCallCounts`、`HandlerLogSummary` 再导出。
- 单测(harness.rs `#[cfg(test)] mod drain_tests`,3 个 `#[tokio::test]`):local tool scripted turn(LLM tool_use → tool ok → LLM text)drain 到 `Done`,watched llm/tool 摘要为 2/2 与 1/1,且 conversation 已提交无 pending;guarded 工具调用在 headless top scope 下原样返回 `AgentError::UnhandledRequirement { kind: Interaction }`(非字符串化)且 tool 未运行;cancel 先于 drain 时首个 requirement 被 abandon,tool handler 与其摘要均为 0。
- 验证:`cargo fmt --all --check` 通过;`cargo clippy --all-targets -- -D warnings`(root + `-p agent-testkit`)无告警;`cargo test -p agent-testkit --lib drain_tests` 3/3;`cargo test --all --all-targets` 全绿(agent-lib 434、agent-testkit 85 + 集成 cassette 2 + smoke 2,0 失败);`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`(root + `-p agent-testkit`)通过;`git diff --check` 干净。

### [TODO] M4-3 实现 assertions 模块

**前置依赖**:M4-2。

**上下文**:减少手写 `conversation.turns()[0].messages()[3]` 和 notification matching。断言必须只读,不修改 machine/context。

**做什么**:

- 在 `assertions.rs` 实现 `assert_conversation` builder:committed_turns、pending_present/none、message_role、message_text、last_assistant_text、tool_result_status、pairing_count、open_call_count。
- 实现 `assert_requirements` / `RequirementObservation` helper:single family、origin、id、request summary。
- 实现 `assert_notifications`:tool started/finished、step boundary count/order、boundary metadata。
- 实现 `assert_trace`:requirement resolved_at_scope、disposition、subagent parent chain。
- 实现 `assert_budget`:steps/tokens/cost。
- 实现 `assert_calls`:handler call count、request count、completion order、peak concurrency。

**验证**:

- 单测:每类 assertion happy path。
- 单测:至少一个 failure message 快照式断言,确保上下文足够。
- 用 assertions 改写一个已有 testkit smoke 测试,证明可读性提升。
- 跑全套验证命令。

### [TODO] M4-R Milestone 4 Review

**前置依赖**:M4-1..M4-3。

**上下文**:确认 harness/assertions 降低样板但不掩盖行为。

**做什么**:

- 检查 `StepHarness` 是否仍能精确暴露中间 requirement。
- 检查 `DrainHarness` 是否保留原始 `AgentError`。
- 检查 assertions 是否只读且 failure message 可诊断。
- 更新下一阶段迁移目标清单。

**验证**:

- 全套验证命令全部通过。
- Review 结论写入完成记录。

---

## Milestone 5 — 并发、取消与 subagent 测试工具

### [TODO] M5-1 实现并发 delay/barrier/peak 工具

**前置依赖**:M4-R。

**上下文**:driver 使用 `FuturesUnordered` 并发兑现本层可处理 requirement。测试需要稳定制造乱序完成,不能使用真实 sleep。

**做什么**:

- 在 `concurrency.rs` 实现 `Delay::yields(n)`、可选 barrier helper。
- 实现 `PeakInFlight` 计数器和 completion log。
- 给 `ScriptedToolHandler` 或 wrapper 增加 delay 与 peak recording 支持。
- 避免 `tokio::time::sleep`;使用 yield、oneshot/barrier 或手动 future。

**验证**:

- 单测:两个 tool call 峰值 in-flight 为 2。
- 单测:通过 delay 稳定得到 out-of-order completion。
- 单测:不依赖真实时间。
- 跑全套验证命令。

### [TODO] M5-2 实现 cancel-on-call 与 panic-on-call wrappers

**前置依赖**:M5-1。

**上下文**:取消路径需要证明某些 IO 没有发生。现有测试手写 panic handler 和 cancelling handler。

**做什么**:

- 实现 `CancelOnCall<H>` wrapper:调用前或调用后 cancel `RunContext`。
- 实现 `PanicOnCall` handler/wrapper,用于断言某 family 不应被触发。
- 支持按第 N 次调用触发 cancel。
- 与 call log 集成,记录 cancel 发生时机。

**验证**:

- 单测:LLM 返回 tool_use 后 cancel,tool handler 未触发。
- 单测:PanicOnCall 在不应触发路径不 panic,在触发路径 panic。
- 跑全套验证命令。

### [TODO] M5-3 实现 scripted subagent spawner 与 parent/child scope helpers

**前置依赖**:M5-2。

**上下文**:subagent 测试目前手写 `MockSpawner`、child machine、child scope。需要统一工具覆盖 headless child pop、depth、budget、cancel。

**做什么**:

- 在 `subagent.rs` 实现 `ScriptedSubagentSpawner: SubagentSpawner`。
- 支持 child_ids deterministic 分配、spawn closure、summary script。
- 提供 `SpawnedChildBuilder`,组合 machine、scope、opening input。
- 提供 parent/child scope helper:headless child、attended child、parent with subagent handler。
- 与 `ScriptMachine`、`SeqIds`、`TestScope` 集成。

**验证**:

- 单测:headless child interaction pop 到 parent interaction handler。
- 单测:depth guard 不调用 spawn。
- 单测:parent cancel 传播到 child abandon。
- 单测:child token charge 进入 parent budget。
- 跑全套验证命令。

### [TODO] M5-R Milestone 5 Review

**前置依赖**:M5-1..M5-3。

**上下文**:确认并发/取消/subagent 工具足以迁移现有复杂 e2e。

**做什么**:

- 核对没有真实 sleep。
- 核对 cancel helpers 不吞掉 never-resume 语义。
- 核对 subagent helpers 不绕过 `DrivingSubagentHandler` 的深度/预算/cancel 强制。
- 列出 M6 要迁移/新增的测试套件文件。

**验证**:

- 全套验证命令全部通过。
- Review 结论写入完成记录。

---

## Milestone 6 — 测试套件迁移与扩展

### [TODO] M6-1 迁移 `tests/agent_effect_e2e.rs` 到 testkit

**前置依赖**:M5-R。

**上下文**:`tests/agent_effect_e2e.rs` 目前包含大量本地 fake 和 helper。迁移目标是保持四个验收语义不变,减少样板。

**做什么**:

- 用 `SeqIds` 替换本地 `SeqIds`。
- 用 fixtures 替换本地 message/response/tool helpers。
- 用 scripted/cassette handlers 或 `ScriptedToolRegistry` 替换本地 `FakeClient`、`FakeToolRegistry`。
- 用 subagent helpers 替换本地 `ChildSpawner` / child scope boilerplate。
- 保持原有四个测试语义:attended parent serves headless child、same child attended resolves in place、batch concurrent、parent cancel propagates。

**验证**:

- 聚焦运行 `cargo test --test agent_effect_e2e`。
- 断言测试行数或本地 fake 类型数量显著下降,完成记录中写明删除了哪些重复类型。
- 全套验证命令全部通过。

### [TODO] M6-2 迁移 reference driver 测试中的重复 fake

**前置依赖**:M6-1。

**上下文**:`src/agent/drive/reference/tests.rs` 里有 fake client/registry/id/approval interaction 等重复逻辑。迁移时保留 reference driver 行为覆盖。

**做什么**:

- 用 `SeqIds` / fixtures 替换 id 和 payload helpers。
- 用 scripted handlers 或 `ScriptedToolRegistry` 替换 fake client/registry。
- 若 reference driver 必须测试 `ReferenceScope` + `LlmClientHandler` / `ToolRegistryHandler`,保留最小 `LlmClient`/`ToolRegistry` adapter,但底层脚本和 call log 来自 testkit。
- 保留 text-only、single tool、parallel tools、tool failure、approval approve/deny、headless unhandled、cancel、新 turn after cancel、reconfig swap 覆盖。

**验证**:

- 聚焦运行 `cargo test --lib agent::drive::reference::tests`。
- 全套验证命令全部通过。
- 完成记录列出保留的 reference-specific fake 与理由。

### [TODO] M6-3 新增 Core Rust suites

**前置依赖**:M6-2。

**上下文**:从增强能力出发,补一组简单但覆盖密度高的 Rust suites。它们应快、稳定、离线,用于基础正确性回归。

**做什么**:

- 新增或整理 `agent_step_basic` suite:NeedLlm emit、resume text、wrong id/kind、abandon。
- 新增或整理 `agent_tool_basic` suite:single tool、parallel tool、tool error、step limit、provider call mismatch。
- 新增或整理 `agent_interaction_basic` suite:approve/deny/timeout/cancel、wrong call/step rejection。
- 新增或整理 `agent_driver_basic` suite:local handler、pop、top unhandled、misaligned result。
- 新增或整理 `agent_trace_budget_basic` suite:resolved_at_scope、never-resumed、budget shared ledger。
- 避免复制已有底层测试;若已有测试等价,用 testkit 重写或在完成记录中标记“已有覆盖,未重复”。

**验证**:

- 每个 suite 可被 cargo test filter 单独运行。
- 全套验证命令全部通过。
- 完成记录给出 coverage map:新增测试对应 `docs/TESTABILITY.md` 中哪一行矩阵。

### [TODO] M6-4 新增 recorded replay suites

**前置依赖**:M6-3。

**上下文**:cassette 能提高真实场景覆盖,但 replay 测试必须默认离线。首批不追求数量,追求 cassette 格式与维护流程可靠。

**做什么**:

- 增加 `agent_replay_text`。
- 增加 `agent_replay_tool`。
- 如已有足够工具,增加 `agent_replay_approval` 或 `agent_replay_regression`。
- 每个 replay suite 都断言 final conversation、handler call log、final cursor。
- 文档说明如何 record/update cassette,以及哪些环境变量启用。

**验证**:

- 在无 credentials 环境运行 replay suites 成功。
- `git diff --check` 确认 cassette JSON 无尾随空白。
- 全套验证命令全部通过。

### [TODO] M6-R Milestone 6 Review

**前置依赖**:M6-1..M6-4。

**上下文**:确认 testkit 已实际降低测试样板并提升覆盖,而不是只新增一层抽象。

**做什么**:

- 对比迁移前后重复 fake 数量、测试行数或可读性变化。
- 核对 Core Rust suites、Scripted Scenario suites、Recorded Replay suites 的覆盖矩阵。
- 核对所有 replay 测试 CI 离线可跑。
- 更新 `docs/TESTABILITY.md` 的现状描述。

**验证**:

- 全套验证命令全部通过。
- Review 结论写入完成记录。

---

## Milestone 7 — Scenario DSL 草案与文档并轨

### [TODO] M7-1 设计 data-only scenario model 草案

**前置依赖**:M6-R。

**上下文**:Rust scripted API 稳定后,才能抽 JSON/TS 可复用的 scenario model。首版只做草案和 spike,不做 NAPI。

**做什么**:

- 定义 `Scenario`、`ScenarioInput`、`ScenarioEffectScript`、`ScenarioExpectation` 数据结构草案。
- 支持 serde round-trip。
- 支持表达 text/tool/approval 三类最小场景。
- 编写一个 runner spike:scenario -> result summary。
- 明确哪些断言进入 summary,哪些仍留在 Rust assertions。

**验证**:

- 单测:scenario JSON round-trip。
- 单测:最小 text/tool/approval scenario 可运行。
- 全套验证命令全部通过。

### [TODO] M7-2 文档、README 与开发指南更新

**前置依赖**:M7-1。

**上下文**:testkit 和 cassette 会成为新测试入口,需要文档说明边界、常用写法、record/update 流程和未来 TS/NAPI 路线。

**做什么**:

- 更新 `docs/TESTABILITY.md`,把已落地模块从规划改为当前状态。
- 更新 `README.md` 当前计划链接,说明当前根 `PLAN.md` / `TODO.md` 是 Testability 阶段。
- 给 `crates/agent-testkit` 添加 crate-level rustdoc,包含 quickstart 示例。
- 记录 cassette record/update 环境变量。
- 记录“不 mock HTTP provider”的边界。

**验证**:

- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` 通过。
- README 中 archive/current plan 链接有效。
- 全套验证命令全部通过。

### [TODO] M7-R Milestone 7 与 Testability 总 Review

**前置依赖**:M7-1..M7-2。

**上下文**:回溯本计划与 TODO,确认 Testability 阶段完整落地,并列出后续 TS/NAPI 或 trait/core crate 拆分条件。

**做什么**:

- 回溯 `PLAN.md`、`TODO.md`、`docs/TESTABILITY.md`。
- 确认 testkit 没有引入 provider wire mock。
- 确认基础 Rust suites 与 recorded replay suites 默认离线可跑。
- 确认 cassette 脱敏与 update 护栏有效。
- 确认 scenario DSL 是否足以作为未来 TS/NAPI 的输入;若不足,列出缺口。
- 总结是否仍无需拆 trait crate;若实际 Cargo 拓扑证明需要拆,提出单独后续计划。

**验证**:

- 全套验证命令全部通过。
- 总 Review 结论与后续项写入完成记录。
