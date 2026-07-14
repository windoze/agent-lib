# TODO：复杂 Mock 测试与 Plan 依赖语义任务单

> 依据 [`PLAN.md`](PLAN.md) 与 [`docs/complex-tests.md`](docs/complex-tests.md)。任务按实现顺序编号;
> coding agent 每次只执行首个标题带 `[TODO]` 的任务,完成后把该标题的 `[TODO]` 改为 `[DONE]`,并在
> 任务末尾补充完成记录。

通用约束:不得改变 `agent-lib` 运行时语义;不得 mock provider HTTP/SSE/raw JSON;不得依赖真实 sleep、网络或
credentials;复杂测试应默认离线、稳定、可单独过滤运行。复杂 helper 优先放在 `tests/complex_support/`,不要引入
新的 scenario DSL。

---

## Milestone 1 — Support 与 Mock Vertical Features

### [DONE] M1-1 建立复杂测试支持模块与 `MockPlanBlackboardStore`

**前置依赖**:无。

**上下文**:

`docs/complex-tests.md` §3.2 与 §8 要求复杂测试先用 mock plan/blackboard vertical feature,因为生产级
plan/blackboard API 尚未落地。当前代码只有 `PlanId` / `BlackboardId` identity,所以支持层必须独立于生产实现,
但语义要对齐 `docs/agent-layer.md` §6.2:plan item 支持 dependency、claim 前置完成检查、claim-first。

**做什么**:

- 新建 `tests/complex_support/mod.rs`、`tests/complex_support/plan_blackboard.rs`、`tests/agent_complex_support.rs`。
- 在 `plan_blackboard.rs` 定义最小数据模型:
  - `MockPlanBlackboardStore { plan: Mutex<PlanState>, board: Mutex<Vec<BoardMessage>>, ops: Mutex<Vec<StoreOp>> }`。
  - `PlanState { id: PlanId, version: u64, task_order: Vec<String>, tasks: BTreeMap<String, TaskState> }`。
  - `TaskState { status: TaskStatus, owner: Option<String>, depends_on: Vec<String> }`。
  - `TaskStatus`:至少包含 `Todo`、`InProgress`、`Completed`、`Blocked`、`Cancelled`。
  - `BoardMessage { offset: u64, sender: String, text: String }`。
  - `StoreOp`:记录 create/add/claim/claim_first/update/post/read/error 等操作,失败也要记录。
- 实现 plan 操作:
  - `create_plan` 初始化 version=0、空 task_order/tasks。
  - `add_task(id, depends_on)` 校验依赖引用已知 task、不得自依赖、不得形成环;成功后追加到 `task_order`,递增 version。
  - `claim(task_id, owner, expected_version)` 检查 version/status/owner/dependencies;依赖未完成返回 dependency-blocked 错误且不修改状态。
  - `claim_first_available(owner, expected_version)` 按 `task_order` 扫描,跳过 completed/已有 owner/依赖未完成 item,原子认领第一个可用 item;无可用项返回 `NoAvailableItem`。
  - `update_status(task_id, owner, status, expected_version)` 检查 owner/version/合法状态转换;成功递增 version。
- 实现 blackboard 操作:
  - `post(sender, text)` append-only,返回 offset。
  - `read_from(offset)` 返回 offset 之后消息。
- 错误类型使用测试内自定义 enum,但 tool adapter 需要能把错误转成 model-visible tool error text。
- 所有 public helper 失败时 panic 信息要包含 `ops` 日志摘要,方便复杂测试定位。

**验证**:

- `cargo fmt --all -- --check`。
- `cargo test --test agent_complex_support plan_dependencies_reject_unknown_self_and_cycles`。
- `cargo test --test agent_complex_support claim_rejects_unfinished_dependencies_atomically`。
- `cargo test --test agent_complex_support claim_first_available_skips_blocked_and_claimed_items`。
- `cargo test --test agent_complex_support blackboard_is_append_only_and_offsets_are_monotonic`。
- `cargo clippy --all-targets -- -D warnings`。
- `cargo test --all --all-targets`。
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`。
- `git diff --check`。

**完成记录**:

- 新建 `tests/complex_support/mod.rs`、`tests/complex_support/plan_blackboard.rs`、
  `tests/agent_complex_support.rs`。`mod.rs` 声明 `pub mod plan_blackboard;`;因支持层随
  里程碑增量落地、每个复杂测试 crate 独立编译只用其子集,`mod.rs` 加 `#![allow(dead_code)]`
  (传播到子模块),避免尚未被本 crate 使用的 helper 触发 `dead_code`。
- `plan_blackboard.rs` 按设计实现数据模型:`MockPlanBlackboardStore { plan: Mutex<PlanState>,
  board: Mutex<Vec<BoardMessage>>, ops: Mutex<Vec<StoreOp>> }`、`PlanState`、`TaskState`、
  `TaskStatus`(Todo/InProgress/Completed/Blocked/Cancelled 全含)、`BoardMessage`,以及
  `StoreOp { kind: OpKind, outcome: Result<String,String> }`(create/add/claim/claim_first/
  update/post/read 均记录,成功与失败都入 `ops`)。
- plan 操作:`create_plan`(重置 version=0、空 task_order/tasks)、`add_task`(校验 duplicate/
  self-dep/unknown-dep,并跑共享 `detect_cycle` 防御环;成功追加 task_order 并 version+1)、
  `claim`(先校验 version→owner→status→依赖完成,任一失败原子不改 owner/status/version;依赖
  未完成返回 `DependencyBlocked`)、`claim_first_available`(按 task_order 跳过 completed/已认领/
  依赖未完成,认领首个可用;无则 `NoAvailableItem`)、`update_status`(校验 version/owner/合法
  转换,成功 version+1)。
- blackboard 操作:`post`(append-only,offset 从 0 单调递增,返回 offset)、`read_from`(返回
  offset 及之后消息)。无 delete/update 路径。
- 错误 `StoreError` enum 自定义;`Display` 产出紧凑 model-visible 文本,供 M1-2 tool adapter
  转成 tool-error text。store 在锁 poison 时 panic 且携带 `ops_summary()` 摘要;`ops()` /
  `ops_summary()` 供 M1-3 断言 helper 在失败时打印操作日志。
- 依赖环语义说明:`add_task` 要求 `depends_on` 只能引用已存在 task(spec「引用已知 task」),
  因此按插入顺序天然是 DAG,公开 add-only 路径无法构造多节点环;`detect_cycle` 设为 `pub` 并在
  `add_task` 内部防御性调用,`plan_dependencies_reject_unknown_self_and_cycles` 用手工构造的
  `a<->b` 图直接断言其能识别真实环,同时断言已接受的真实图无环。
- 验证结果(全部通过):`cargo fmt --all -- --check`;四个指定测试
  (`plan_dependencies_reject_unknown_self_and_cycles`、
  `claim_rejects_unfinished_dependencies_atomically`、
  `claim_first_available_skips_blocked_and_claimed_items`、
  `blackboard_is_append_only_and_offsets_are_monotonic`)单独运行通过;
  `cargo clippy --all-targets -- -D warnings` 无告警;
  `cargo test --all --all-targets` 全绿(仅 credential-gated 集成测试 ignored);
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` 无告警;`git diff --check` 干净。

### [DONE] M1-2 实现 complex tool adapter、tool declarations 与 approval policy helpers

**前置依赖**:M1-1。

**上下文**:

复杂场景需要模型通过 tool 操作 mock plan/blackboard,并通过 dangerous tool 触发 approval。不要使用真实
`ToolRegistry` 后端或 provider wire mock;tool adapter 应实现 `ToolHandler` 或 `ToolRegistry`,直接站在
`RequirementKind::NeedTool` 边界。

**做什么**:

- 在 `tests/complex_support/tools.rs` 定义工具名常量:
  - `PLAN_CREATE`, `PLAN_ADD_TASK`, `PLAN_CLAIM`, `PLAN_CLAIM_FIRST_AVAILABLE`, `PLAN_UPDATE`。
  - `BLACKBOARD_POST`, `BLACKBOARD_READ`。
  - `DANGEROUS_WRITE`, `SAFE_READ`。
- 提供 `complex_tools(ids/store)` 或 `tool_declarations()` 返回 `Vec<Tool>`,供 `agent_spec_with_tools` 使用。
- 实现 `ComplexToolHandler`:
  - 持 `Arc<MockPlanBlackboardStore>`。
  - 记录 per-tool call log,至少能断言 dangerous tool 执行次数与 input。
  - 按 `ToolCall.name` 分发到 store 操作。
  - 成功返回 `ToolStatus::Ok` 的 `ToolResponse`。
  - store 错误返回 `ToolStatus::Error` 的 model-visible tool result,不要 panic。
  - unknown tool 返回 `ToolRuntimeError::UnknownTool` 或 model-visible error,按现有 testkit 风格选择一种并在测试中固定。
- 实现 `RequireDangerousWriteApprovalPolicy`:
  - `dangerous_write` 返回 `ApprovalRequirement::required`。
  - 其他 tool 返回 no approval / auto allow。
- 提供 scenario setup helper:
  - `complex_agent_machine(ids, store)` 创建带所有 complex tools 的 `DefaultAgentMachine`。
  - `complex_scope(llm, tool, interaction)` 组装 `TestScope`。
- 在 `tests/agent_complex_support.rs` 中补 adapter 单测。

**验证**:

- `cargo fmt --all -- --check`。
- `cargo test --test agent_complex_support plan_tools_return_model_visible_errors`。
- `cargo test --test agent_complex_support dangerous_write_requires_approval_and_safe_tools_do_not`。
- `cargo test --test agent_complex_support dangerous_write_call_log_counts_executions`。
- `cargo clippy --all-targets -- -D warnings`。
- `cargo test --all --all-targets`。
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`。
- `git diff --check`。

**完成记录**:

- 新建 `tests/complex_support/tools.rs`,`mod.rs` 增加 `pub mod tools;`。工具站在
  `RequirementKind::NeedTool` 边界(实现 `ToolHandler`),不 mock provider wire,不接真实
  `ToolRegistry` 后端。
- 工具名常量全部落地:`PLAN_CREATE`/`PLAN_ADD_TASK`/`PLAN_CLAIM`/`PLAN_CLAIM_FIRST_AVAILABLE`/
  `PLAN_UPDATE`、`BLACKBOARD_POST`/`BLACKBOARD_READ`、`DANGEROUS_WRITE`/`SAFE_READ`(均为
  `pub const &str`)。`tool_declarations() -> Vec<Tool>` 为每个工具给出 JSON input schema,供
  `agent_spec_with_tools` 使用。
- `ComplexToolHandler` 持 `Arc<MockPlanBlackboardStore>`,按 `ToolCall.name` 分发到 store 操作:
  plan_create/add_task/claim/claim_first_available/update 与 blackboard post/read。成功返回
  `Tool(Ok(ToolResponse{status:Ok}))`;store 错误或参数解析错误返回
  `Tool(Ok(ToolResponse{status:Error}))`(model-visible,不 panic);unknown tool 返回
  `Tool(Err(ToolRuntimeError::UnknownTool))`(固定选此风格,`plan_tools_return_model_visible_errors`
  锁定)。`dangerous_write` 向 blackboard(sender=`dangerous_write`)追加消息,使"已批准写确实执行"
  在 store 中可观察;`safe_read` 为 auto-allow 的普通读。
- per-tool call log:`ToolInvocation { name, input, outcome }`,只在 handler 真正被调用(即审批通过后
  执行)时入日志;提供 `calls()`/`calls_named()`/`execution_count()`,可断言 dangerous tool 执行次数与
  input。
- `RequireDangerousWriteApprovalPolicy` 实现 `ToolApprovalPolicy`:`dangerous_write` 返回
  `ApprovalRequirement::required(reason)`,其余工具 `AutoApprove`。
- scenario setup helper:`complex_agent_machine(ids)` 构造带全部 complex tools 声明 + 上述 approval
  policy 的 `DefaultAgentMachine`;`complex_tool_handler(store)` 构造 `Arc<ComplexToolHandler>`;
  `complex_scope(llm, tool, interaction)` 组装 `TestScope`(interaction 可选,`None` 保持 headless)。
  说明:store 不进 machine——machine 只承载 tool 声明与 approval policy,store 走 tool handler / scope
  边界,因此拆成 `complex_tool_handler(store)`,避免机器持有它导致 unused 参数;这是 helper 人体工学选择,
  非 spec 偏离(machine 与 scope 仍分别覆盖 TODO 所列职责)。
- 支持层辅助:`TaskStatus::from_label` 新增于 `plan_blackboard.rs`,供 adapter 解析 `plan_update` 的
  `status` 字符串参数(未知 label 返回 `None` → model-visible error)。
- `tests/agent_complex_support.rs` 补三个 `#[tokio::test]` adapter 单测,直接驱动
  `handler.fulfill` / `policy.approval_requirement`:
  `plan_tools_return_model_visible_errors`(依赖阻塞、未知依赖、缺参 → status Error 且携带 store/参数
  文本;未知工具 → `UnknownTool`)、
  `dangerous_write_requires_approval_and_safe_tools_do_not`(危险工具 RequireApproval 且 reason 稳定,
  其余 AutoApprove)、
  `dangerous_write_call_log_counts_executions`(两次危险写 + 一次 safe_read,断言执行计数、input 序列与
  blackboard 副作用)。
- 验证结果(全部通过):`cargo fmt --all -- --check`;三个指定测试与整个
  `cargo test --test agent_complex_support`(7 passed)通过;`cargo clippy --all-targets -- -D warnings`
  无告警;`cargo test --all --all-targets` 全绿(仅 4 个 credential-gated 集成测试 ignored,无 failure);
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` 无告警;`git diff --check` 干净。

### [TODO] M1-3 实现复杂测试断言 helper

**前置依赖**:M1-2。

**上下文**:

复杂测试失败时,只看普通 `assert_eq!` 很难定位。`docs/complex-tests.md` §6 要求失败信息包含 role sequence、tool
result status、store ops、outstanding ids / child log。helper 只应读观察对象,不得修改 machine/store/context。

**做什么**:

- 新建 `tests/complex_support/assertions.rs`。
- 提供 plan/blackboard 断言:
  - `assert_task_status(store, id, status)`。
  - `assert_task_owner(store, id, owner)`。
  - `assert_task_depends_on(store, id, expected)`。
  - `assert_no_task_owner(store, id)`。
  - `assert_board_messages(store, expected_substrings_in_order)`。
  - 失败时打印 `store.ops()`。
- 提供 conversation helper:
  - `role_sequence(conversation, turn_index)`。
  - `assert_pivot_after_tool_result(conversation, pivot_text)`。
  - 可以复用 `agent_testkit::assert_conversation`,不要重写已有 assertion 能力。
- 提供 handler log helper:
  - `assert_tool_executions(log/store, tool_name, count)`。
  - `assert_interaction_decisions(log, expected_count)`。
- 在 `mod.rs` re-export 支持层常用类型和 helper。

**验证**:

- `cargo fmt --all -- --check`。
- `cargo test --test agent_complex_support assertions_report_store_ops_on_failure`。
- `cargo test --test agent_complex_support role_sequence_and_pivot_helpers_find_expected_messages`。
- `cargo clippy --all-targets -- -D warnings`。
- `cargo test --all --all-targets`。
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`。
- `git diff --check`。

### [TODO] M1-R Milestone 1 Review

**前置依赖**:M1-1..M1-3。

**上下文**:

确认复杂测试支持层足够表达 P0/P1 场景,且没有把测试 mock 做成生产 API 或 provider wire mock。

**做什么**:

- 核对 `MockPlanBlackboardStore` 的 dependency、claim、claim-first、blackboard append-only 语义。
- 核对 tool adapter 是否只站在 `ToolHandler`/`ToolRegistry` effect 边界。
- 核对 approval policy 只 guard `dangerous_write`,不影响 safe tools。
- 核对 helper 失败信息是否包含 store ops / role sequence / handler log 关键上下文。
- 若支持层已经明显可复用,确认是否仍留在 `tests/complex_support/`;不要提前移动到 `agent-testkit`。

**验证**:

- `cargo fmt --all -- --check`。
- `cargo clippy --all-targets -- -D warnings`。
- `cargo test --test agent_complex_support`。
- `cargo test --all --all-targets`。
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`。
- `git diff --check`。
- Review 结论写入本任务完成记录。

---

## Milestone 2 — 主复杂 Flow：多轮、Plan/Blackboard、Approval、Pivot

### [TODO] M2-1 实现 `agent_complex_flow` 主场景

**前置依赖**:M1-R。

**上下文**:

`docs/complex-tests.md` §4.1 的 P0-1 是最高价值组合场景:同一 turn 内包含 plan dependency、blackboard post、
dangerous tool approve、post-tool pivot、第二个 dangerous tool deny、final LLM。pivot 必须用 `StepHarness` 在合法
边界手动插入,不能用 `DrainHarness` 一口气跑过。

**做什么**:

- 新建 `tests/agent_complex_flow.rs`,引入 `#[path = "complex_support/mod.rs"] mod complex_support;`。
- 构造 `SeqIds`、`MockPlanBlackboardStore`、`RunContext`、带 complex tools 的 `DefaultAgentMachine`。
- 设置 approval policy:仅 `dangerous_write` 需要 approval。
- 使用 `StepHarness::with_ids` 手动推进:
  - user 输入“实现功能 A”。
  - resume 第 1 次 LLM:tool_use `plan_create`、`plan_add_task(design)`、`plan_add_task(implement depends_on [design])`。
  - 对 emit 的 tool requirement 调用 `ComplexToolHandler` 并 resume。
  - resume 第 2 次 LLM:tool_use `blackboard_post` + `dangerous_write`。
  - 对 dangerous tool approval requirement 调用 `ScriptedInteractionHandler::sequence([Approve, Deny(...)])`,approve 后再执行 tool handler。
  - 在第一次 dangerous tool result 后、下一次 LLM resume 前调用 `harness.pivot("先不要改文件,只给方案")`。
  - resume re-rendered LLM requirement:返回第二个 `dangerous_write`。
  - 第二次 approval 返回 deny;断言 dangerous tool handler 不执行第二次。
  - resume final LLM text,turn 到 `Done`。
- 断言:
  - committed turn 数为 1,无 pending。
  - role sequence 中 pivot user message 出现在第一次 tool result 后。
  - `implement.depends_on == [design]`,且 design 未完成前 implement 不可 claim。
  - blackboard offset 单调,包含开始处理与 pivot 后改变策略。
  - dangerous tool execution count == 1。
  - interaction count == 2,顺序为 approve/deny。
  - 最后一次 LLM request 包含 pivot 文本与 denied/cancelled tool result。

**验证**:

- `cargo fmt --all -- --check`。
- `cargo test --test agent_complex_flow complex_turn_combines_plan_blackboard_approval_deny_and_pivot`。
- `cargo clippy --all-targets -- -D warnings`。
- `cargo test --all --all-targets`。
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`。
- `git diff --check`。

### [TODO] M2-2 补充主 flow 的负向断言与防回归用例

**前置依赖**:M2-1。

**上下文**:

主场景证明 happy-path 组合,但还需要把 plan dependency 和 approval deny 的错误面固定住,避免未来 helper 或机器改动
把错误静默吞掉。

**做什么**:

- 在 `tests/agent_complex_flow.rs` 增加聚焦测试:
  - `claim_dependency_block_returns_tool_error_and_does_not_mutate_task`:直接通过 `ComplexToolHandler` 调 `plan_claim` 试图 claim `implement`,前置 `design` 未 completed,应返回 `ToolStatus::Error`,owner/status/version 不变。
  - `denied_dangerous_write_does_not_execute_tool`:构造 guarded dangerous tool round-trip,interaction deny 后 final LLM,断言 dangerous execution log 为 0。
- 这些测试可以用 `DrainHarness` 或直接调用 handler;选择最少样板方式。
- 失败信息必须包含 store ops 或 handler log。

**验证**:

- `cargo fmt --all -- --check`。
- `cargo test --test agent_complex_flow claim_dependency_block_returns_tool_error_and_does_not_mutate_task`。
- `cargo test --test agent_complex_flow denied_dangerous_write_does_not_execute_tool`。
- `cargo clippy --all-targets -- -D warnings`。
- `cargo test --all --all-targets`。
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`。
- `git diff --check`。

### [TODO] M2-R Milestone 2 Review

**前置依赖**:M2-1..M2-2。

**上下文**:

确认主复杂 flow 能稳定覆盖多轮、plan dependency、approval approve/deny 与 pivot,且失败信息足够定位。

**做什么**:

- 核对主场景是否真的经过至少 4 次 LLM 往返和两次 interaction。
- 核对 pivot 是否在合法 post-tool boundary 注入,并被后续 LLM request 看到。
- 核对 deny 后 dangerous tool 未执行,且 turn 继续到 final。
- 核对 plan dependency blocked 是 model-visible tool error,不是 panic。
- 检查测试是否过长;如单测不可读,抽取 setup/helper,但不要新建 DSL。

**验证**:

- `cargo fmt --all -- --check`。
- `cargo clippy --all-targets -- -D warnings`。
- `cargo test --test agent_complex_flow`。
- `cargo test --all --all-targets`。
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`。
- `git diff --check`。
- Review 结论写入本任务完成记录。

---

## Milestone 3 — Subagent、Scope Pop 与 Cancel

### [TODO] M3-1 实现 subagent + parent approval pop + shared plan/blackboard 场景

**前置依赖**:M2-R。

**上下文**:

`docs/complex-tests.md` §4.2 要求 headless child 的 approval/interaction pop 到 parent,同时 child 操作共享
plan/blackboard。现有 `agent-testkit` 已有 `ScriptedSubagentSpawner`、`headless_child_scope`、`parent_scope_with_subagent`,
应优先复用。

**做什么**:

- 新建 `tests/agent_complex_subagent.rs`。
- 构造 shared `Arc<MockPlanBlackboardStore>` 并预置 plan:
  - `design` 已 completed。
  - `review` depends_on `[design]`,status `Todo`。
  - `implement` depends_on `[review]`,status `Todo`。
- child 使用 headless scope:有 LLM/tool handler,无 interaction handler。
- child LLM 脚本:
  - 第一次 tool_use `plan_claim_first_available` + `blackboard_post("review started")`。
  - 第二次 tool_use `dangerous_write` 或需要 approval 的 review tool。
  - 第三次 tool_use `plan_update(review, Completed)` + `blackboard_post("review done")`。
  - final text。
- parent scope 挂 subagent handler 与 attended interaction handler,interaction 返回 approve。
- parent 可以用 `ScriptMachine` emit `NeedSubagent`,也可以用 `DefaultAgentMachine` 通过 LLM/tool 化 spawn;优先选择更少样板且能验证 `DrivingSubagentHandler` 的方式。
- 断言:
  - `spawn_calls == 1`,`summarize_calls == 1`。
  - parent interaction log 收到 child approval;child interaction log 为空或不存在。
  - `plan_claim_first_available` claim 到 `review`,没有 claim `implement`。
  - blackboard 消息 started -> done 顺序 append, sender 可区分 parent/child。
  - child token/budget charge 反映到 parent `RunContext`。
  - trace 或 available assertion 能看到 child requirement resolved at parent scope/subagent resumed。

**验证**:

- `cargo fmt --all -- --check`。
- `cargo test --test agent_complex_subagent complex_subagent_updates_shared_plan_and_pops_approval_to_parent`。
- `cargo clippy --all-targets -- -D warnings`。
- `cargo test --all --all-targets`。
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`。
- `git diff --check`。

### [TODO] M3-2 实现 cancel during subagent/tool wait 场景

**前置依赖**:M3-1。

**上下文**:

`docs/complex-tests.md` §4.3 要求证明 cancel 是 never-resume:已经发生的 side effect 不回滚,未发生的 side effect
不补跑;child outstanding requirement 被 abandon;cancel 后 parent/conversation 可继续新 turn。

**做什么**:

- 新建 `tests/agent_complex_cancel.rs`。
- 构造 shared store,预置并 claim 一个 task,post blackboard “started”。
- child 使用 `ScriptMachine` 或 `DefaultAgentMachine` emit 一个 `NeedTool`/`NeedLlm`。
- 用 `PanicOnCall`、`CancelOnCall`、`Barrier` 或 `Delay` 稳定控制 cancel 时机;不要使用真实 sleep。
- 在 parent `RunContext.cancellation().cancel()` 后驱动 subagent handler/drain。
- 断言:
  - child log `abandon_count == 1`,`resume_count == 0`。
  - 被 cancel 后不应运行的 handler call count 为 0,或 begun 未 complete,按选择的 cancel 注入时机固定。
  - plan task 不会变成 `Completed`,只能保持 `InProgress` 或被后续标记 `Cancelled/Blocked`。
  - blackboard 已有 started,后续追加 cancelled,没有重复 started。
  - parent/cancelled machine 后续可接受新 user turn 并 commit。
  - trace disposition 为 `NeverResumed` 或现有 assertion 可观察的等价 cancel disposition。

**验证**:

- `cargo fmt --all -- --check`。
- `cargo test --test agent_complex_cancel complex_cancel_abandons_child_and_preserves_committed_state`。
- `cargo clippy --all-targets -- -D warnings`。
- `cargo test --all --all-targets`。
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`。
- `git diff --check`。

### [TODO] M3-R Milestone 3 Review

**前置依赖**:M3-1..M3-2。

**上下文**:

确认 subagent 动态作用域、共享 side effect、budget/cancel 传播都被复杂测试覆盖。

**做什么**:

- 核对 child headless interaction 是否确实 pop 到 parent。
- 核对 `plan_claim_first_available` 在 child 场景中跳过 dependency-blocked item。
- 核对 shared store side effect 只发生一次,没有因 retry/resume 重复写。
- 核对 cancel path 是 abandon/never-resume,不是 wrong-family error 或 panic。
- 核对 cancel 后继续新 turn 的断言真实检查了 committed conversation。

**验证**:

- `cargo fmt --all -- --check`。
- `cargo clippy --all-targets -- -D warnings`。
- `cargo test --test agent_complex_subagent`。
- `cargo test --test agent_complex_cancel`。
- `cargo test --all --all-targets`。
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`。
- `git diff --check`。
- Review 结论写入本任务完成记录。

---

## Milestone 4 — P1 回归补强与文档并轨

### [TODO] M4-1 实现 plan claim conflict / dependency-blocked recovery 场景

**前置依赖**:M3-R。

**上下文**:

P1 场景用于补齐多 worker 或模型选错 task 时的恢复路径。claim conflict 和 dependency-blocked 都应作为
model-visible tool result 返回给 LLM,而不是 panic 或破坏 plan 状态。

**做什么**:

- 在 `tests/agent_complex_flow.rs` 或新文件中添加:
  - `complex_plan_claim_conflict_or_dependency_block_recovers_through_blackboard`。
- 构造两个 worker/child 或两个 tool call:
  - 第一个成功 claim `task-a`。
  - 第二个 claim 同一 task 返回 version conflict。
  - 或第二个 claim `task-b` 但前置未完成,返回 dependency-blocked。
- 第二个 worker post blackboard “claim conflict” 或 “dependency blocked”。
- LLM 恢复后调用 `plan_claim_first_available`,应选择另一个可用 task 或返回 `NoAvailableItem`。
- 断言同一 task 只有一个 owner,blocked task 未被修改,blackboard conflict 消息保留。

**验证**:

- `cargo fmt --all -- --check`。
- `cargo test --test agent_complex_flow complex_plan_claim_conflict_or_dependency_block_recovers_through_blackboard`。
- `cargo clippy --all-targets -- -D warnings`。
- `cargo test --all --all-targets`。
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`。
- `git diff --check`。

### [TODO] M4-2 实现 approval cancel vs context cancel 区分场景

**前置依赖**:M4-1。

**上下文**:

Approval `Cancel` 与 `RunContext` cancel 容易混淆。前者只取消单个 guarded tool call,后者 abandon 整个 in-flight
continuation。两者都应有测试固定。

**做什么**:

- 新增测试 `complex_approval_cancel_does_not_cancel_context_unless_driver_cancels`。
- 第一段:interaction 返回 `InteractionDecision::Cancel(Some("not now"))`,dangerous tool 不执行,LLM 继续并可调用 safe tool;断言 `ctx.is_cancelled() == false`。
- 第二段:显式 `ctx.cancellation().cancel()`,后续 outstanding requirement 被 abandon;断言 handler 不继续执行。
- 使用不同 store/ids 或重建 machine,避免两个段落状态互相污染。
- 断言 trace/handler log 中两种 cancel 可区分;若现有 trace helper 不足,至少断言 context cancellation flag 与 handler execution count。

**验证**:

- `cargo fmt --all -- --check`。
- `cargo test --test agent_complex_cancel complex_approval_cancel_does_not_cancel_context_unless_driver_cancels`。
- `cargo clippy --all-targets -- -D warnings`。
- `cargo test --all --all-targets`。
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`。
- `git diff --check`。

### [TODO] M4-3 实现 pivot 后 subagent brief 使用重渲染 request 场景

**前置依赖**:M4-2。

**上下文**:

P1 场景用于证明 pivot 不只是写进 conversation,还会影响后续 LLM request 和 subagent opening brief。否则模型可能按
pivot 前旧目标 spawn child 或执行危险 tool。

**做什么**:

- 新增测试 `complex_pivot_then_subagent_uses_rerendered_brief`。
- 用 `StepHarness` 在首轮 tool result 后注入 pivot:“改由 reviewer 子 agent 处理”。
- 断言 pivot 后 re-rendered LLM request 包含该文本。
- 后续 LLM 触发 `NeedSubagent` 或 tool 化 `spawn_reviewer`;选择实现成本更低且能观察 brief 的方式。
- `ScriptedSubagentSpawner` 捕获 child opening input/brief,断言包含 pivot 文本且不只包含旧目标。
- 断言 pivot 前旧目标对应 dangerous tool 未执行。

**验证**:

- `cargo fmt --all -- --check`。
- `cargo test --test agent_complex_subagent complex_pivot_then_subagent_uses_rerendered_brief`。
- `cargo clippy --all-targets -- -D warnings`。
- `cargo test --all --all-targets`。
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`。
- `git diff --check`。

### [TODO] M4-4 更新文档、运行说明与测试矩阵

**前置依赖**:M4-3。

**上下文**:

实现完成后,`docs/complex-tests.md`、`docs/TESTABILITY.md` 或 README 中应能看出复杂测试已落地到哪些文件,如何单独运行,
哪些 P1 仍 deferred。

**做什么**:

- 更新 `docs/complex-tests.md`:
  - 标注已落地测试文件与测试名。
  - 标注 mock store 仍是测试支持层,非生产 plan API。
  - 若某个 P1 场景未落地,写明 deferred 原因。
- 如 `docs/TESTABILITY.md` 的 scripted scenario suites 状态已过时,补一段现状说明。
- 可选更新 README 的测试运行示例,仅在现有 README 已有相关测试说明时修改。

**验证**:

- `git diff --check`。
- 如果只改文档,无需运行 Rust 构建;若同时改测试代码,执行全套验证门。

### [TODO] M4-R Milestone 4 总 Review

**前置依赖**:M4-1..M4-4。

**上下文**:

收尾 review 确认复杂 mock 测试设计已按计划落地,并且新测试没有引入 flakiness 或过度抽象。

**做什么**:

- 核对 P0 场景全部落地:P0-1 主 flow、P0-2 subagent、P0-3 cancel。
- 核对 P1 场景落地或明确 deferred:P1-1 claim conflict/dependency block、P1-2 cancel 区分、P1-3 pivot 后 subagent brief。
- 核对所有复杂测试可单独运行,文件命名与 `docs/complex-tests.md` 一致。
- 核对没有真实 sleep、网络、credentials、provider wire mock。
- 核对 failure diagnostics 包含 store ops / handler log / role sequence / outstanding ids 等上下文。
- 核对 helper 没有扩张成通用 DSL;如果某 helper 被三处以上复用,记录是否后续提到 `agent-testkit`。
- 更新本任务完成记录,给出最终测试命令和结果。

**验证**:

- `cargo fmt --all -- --check`。
- `cargo clippy --all-targets -- -D warnings`。
- `cargo test --test agent_complex_support`。
- `cargo test --test agent_complex_flow`。
- `cargo test --test agent_complex_subagent`。
- `cargo test --test agent_complex_cancel`。
- `cargo test --all --all-targets`。
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`。
- `git diff --check`。
- Review 结论写入本任务完成记录。
