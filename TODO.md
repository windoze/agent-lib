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

### [DONE] M1-3 实现复杂测试断言 helper

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

**完成记录**:

- 新建 `tests/complex_support/assertions.rs`(只读断言层)。plan/blackboard 断言
  `assert_task_status` / `assert_task_owner` / `assert_no_task_owner` /
  `assert_task_depends_on` / `assert_board_messages` 全部在失败时把 `store.ops_summary()`
  编号操作日志嵌进 panic 文本;`assert_board_messages` 用「长度相等 + 逐条 `contains`」同时充当
  no-duplicate-side-effect 守卫。
- conversation 断言 `role_sequence(conversation, turn_index)`(越界 panic 带会话摘要)与
  `assert_pivot_after_tool_result(conversation, pivot_text)`(按会话顺序扫描 committed+pending
  消息,定位 tool result 之后含 pivot 文本的 `Role::User` 消息,失败打印 role 序列);沿用
  `agent_testkit::assert_conversation` 的坐标级能力,只补 role-sequence / pivot 位置查询,不重写。
- handler/interaction 断言 `assert_tool_executions(handler, tool, count)`(读
  `ComplexToolHandler::execution_count`,失败打印每工具调用日志,`count=0` 即「dangerous tool 未执行」
  检查)与 `assert_interaction_decisions(log, expected)`(读 `InteractionCallLog::completed_len`)。
- `mod.rs` 增加 `pub mod assertions;` 并 re-export 支持层常用类型/helper(store、tools 常量与类型、
  assertions helper);因各复杂测试 crate 只用子集,`#![allow(dead_code, unused_imports)]` 传播到子模块
  避免误报。
- 新增三个单测(`tests/agent_complex_support.rs`):
  `assertions_report_store_ops_on_failure`(通过路径 + `catch_unwind` 验证 owner/board 失败文本含
  `store operations:` 与记录的 Claim/Post op)、
  `role_sequence_and_pivot_helpers_find_expected_messages`(用 `StepHarness` 驱动
  `complex_agent_machine` 走 user→tool_use(safe_read)→tool result→真正的 mid-turn `pivot`→final text,
  断言 role_sequence(0)=[User,Assistant,Tool,User,Assistant] 且 pivot 命中)、
  `handler_and_store_assertions_hold_after_approved_dangerous_write`(`DrainHarness` 端到端跑
  dangerous_write→approval(Approve)→执行→final text,断言 `assert_tool_executions`(DANGEROUS_WRITE=1,
  SAFE_READ=0)、`assert_interaction_decisions`=1、`assert_board_messages` 单一副作用)。
- 验证结果(全部通过):`cargo fmt --all -- --check`;两个指定测试
  (`assertions_report_store_ops_on_failure`、
  `role_sequence_and_pivot_helpers_find_expected_messages`)单独运行通过;
  `cargo clippy --all-targets -- -D warnings` 无告警;
  `cargo test --all --all-targets` 全绿(仅 4 个 credential-gated 集成测试 ignored);
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` 无告警;`git diff --check` 干净。

### [DONE] M1-R Milestone 1 Review

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

**完成记录**:

Milestone 1 支持层审阅通过,五项检查点全部确认,无需返工。

- **Store 语义(`plan_blackboard.rs`)**:
  - dependency:`add_task` 拒绝 `DuplicateTask` / `SelfDependency` / `UnknownTask`(依赖必须指向已知任务),
    并跑 `detect_cycle` 做防御性无环检查;`depends_on` 存 typed 边集。
  - claim:先做 `version` CAS(`VersionConflict`),再原子校验 owner(`AlreadyClaimed`,同 owner 幂等)、
    状态可转移(`can_transition_to`)、依赖全部 `Completed`(`DependencyBlocked` 携带 `unfinished`);任一校验
    失败都不改 owner/status/version——dependency-blocked claim 确实是 no-op。
  - claim-first:`claim_first_available` 按 `task_order` 稳定序扫描,只认 `Todo` + 无 owner + 依赖满足的第一个
    任务,否则 `NoAvailableItem`;跳过 completed/claimed/blocked 项。
  - blackboard:`post` 以 `offset = board.len()` 单调递增追加,只有 `post` / `read_from` / `board_snapshot`,
    无删除或改写路径,append-only 成立。
  - 每个操作(成功/失败)都进 `StoreOp` 日志,`ops_summary()` 渲染成带编号的转录。
- **Tool adapter 边界(`tools.rs`)**:`ComplexToolHandler` 仅经 `ToolHandler::fulfill` 的
  `RequirementKind::NeedTool` 边界派发到 store;store/参数错误折成 model-visible `ToolStatus::Error`(不 panic),
  未知工具返回 `ToolRuntimeError::UnknownTool`。没有 mock 任何 provider wire(HTTP/SSE/raw JSON),`ToolRegistry`
  后端也未被伪造。已执行调用才进 call log(approval 未通过者不留痕),`execution_count` 因此等于真实执行次数。
- **Approval policy**:`RequireDangerousWriteApprovalPolicy` 只对 `dangerous_write` 返回 `RequireApproval`,
  其余工具 `AutoApprove`;safe tools 不受影响。`dangerous_write_requires_approval_and_safe_tools_do_not`
  测试固定住该边界。
- **Helper 失败信息(`assertions.rs`)**:plan/board 断言失败嵌入 `store.ops_summary()`;`role_sequence` /
  `assert_pivot_after_tool_result` 失败打印会话 role 序列摘要;`assert_tool_executions` 打印每工具调用日志,
  `assert_interaction_decisions` 打印 begun/completed 计数。§6 要求的定位上下文齐备。
- **位置**:支持层仍完整位于 `tests/complex_support/`(`mod.rs` + `plan_blackboard.rs` + `tools.rs` +
  `assertions.rs`),未提前迁入 `agent-testkit`;`#![allow(dead_code, unused_imports)]` 传播使 staged-for-later
  的 re-export 不在未使用的复杂测试 crate 里告警。
- **验证结果(全绿)**:`cargo fmt --all -- --check` 无 diff;`cargo clippy --all-targets -- -D warnings` 无告警;
  `cargo test --test agent_complex_support` 10 passed;`cargo test --all --all-targets` 620 passed / 0 failed /
  7 ignored(全部为 `#[ignore = "requires ..."]` 的 credential/network-gated 集成测试,符合离线约束,非被屏蔽的失败);
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` 无告警;`git diff --check` 干净。
- **结论**:Milestone 1 交付物满足 P0/P1 场景的表达需求,未把测试 mock 做成生产 API 或 provider wire mock;
  可进入 Milestone 2。

---

## Milestone 2 — 主复杂 Flow：多轮、Plan/Blackboard、Approval、Pivot

### [DONE] M2-1 实现 `agent_complex_flow` 主场景

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

**完成记录**:

- 新建 `tests/agent_complex_flow.rs`,`#[path = "complex_support/mod.rs"] mod complex_support;`
  复用 M1 支持层。单测 `complex_turn_combines_plan_blackboard_approval_deny_and_pivot`
  为 `#[tokio::test]`(complex tool/interaction handler 的 `fulfill` 为 async,需在 await 边界
  产出 `RequirementResult` 再喂给 `StepHarness::resume`)。
- 用 `StepHarness::with_ids` 手动逐步推进整轮,pivot 精确落在合法 post-tool → NeedLlm 边界:
  1. `user("实现功能 A")` → NeedLlm;
  2. 第 1 次 LLM 返回 `plan_create` + `plan_add_task(design)` + `plan_add_task(implement,[design])`,
     三个 auto-approve 工具作为一个 NeedTool 批;`resume_tool_batch` 按发射(模型)顺序经
     `ComplexToolHandler` 逐个 fulfill+resume,最后一次推进到下一 NeedLlm;
  3. 第 2 次 LLM 返回 `blackboard_post`(auto)+ `dangerous_write#1`(gated):先 fulfill 自动 post,
     再对 `dangerous_write#1` 的 NeedInteraction 调 `ScriptedInteractionHandler::sequence([Approve,
     Deny(..)])` 取 Approve,批准后 machine 发 `dangerous_write#1` 的 NeedTool 并执行(落一条 board);
  4. 在第一次 dangerous 结果后、下一次 LLM resume 前 `harness.pivot("先不要改文件,只给方案")`,
     断言重渲染的 NeedLlm 复用同一 id;
  5. 重渲染 LLM 返回 `blackboard_post`(pivot 后策略)+ `dangerous_write#2`:自动 post 落 board,
     第二次 approval 取 Deny → machine 合成 `Denied` 工具结果、不发 NeedTool、drain 到 final NeedLlm
     (先捕获其 `ChatRequest` 再 resume);
  6. 最终 LLM 文本收尾,cursor 到 `Done`。
- 断言(全部通过):committed turns==1 且 pending none;`assert_pivot_after_tool_result` 定位 pivot 落在
  首个 tool result 之后;`implement.depends_on==[design]`、`design` 仍为 `Todo`,且
  `store.claim("implement",…,version)` 返回 `DependencyBlocked{unfinished:[design]}`;
  `assert_board_messages` 断言 board 恰为 `["start processing feature A","apply the risky change
  to file A","changed strategy after pivot"]`(单调、无重复副作用);
  `assert_tool_executions(DANGEROUS_WRITE,1)`(仅批准的那次执行);
  `assert_interaction_decisions(log,2)` 且 records 顺序为 `Approve`→`Deny`;
  最后一次 LLM `ChatRequest.messages` 同时包含 pivot 文本的 `Role::User` 消息与
  `ToolStatus::Denied` 的 `ToolResult`。
- 验证结果(全部通过):`cargo fmt --all -- --check`;
  `cargo test --test agent_complex_flow complex_turn_combines_plan_blackboard_approval_deny_and_pivot`
  (1 passed);`cargo clippy --all-targets -- -D warnings` 无告警;
  `cargo test --all --all-targets` 全绿(lib 423 + testkit 131 + 各集成 crate 全通过,仅
  credential-gated 集成测试 ignored);`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`
  无告警;`git diff --check` 干净。

### [DONE] M2-2 补充主 flow 的负向断言与防回归用例

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

**完成记录**:

- 在 `tests/agent_complex_flow.rs` 追加两个 `#[tokio::test]` 防回归用例,复用 M1 支持层,未新建 DSL;
  模块顶层 doc 增补一句说明 M2-2 固定的两个错误面。
- `claim_dependency_block_returns_tool_error_and_does_not_mutate_task`:走最少样板的**直接 handler** 路径。
  先用 store helper 造依赖图(`create_plan`→v0、`add_task("design",[])`→v1、`add_task("implement",["design"])`
  →v2),记录 `version_before`;再用 `handler.fulfill(ids.tool_call_id(), &tool_call(PLAN_CLAIM,{task:implement,
  owner:worker,expected_version:2}), &ctx)` 触发 claim。断言:`RequirementResult::Tool(Ok(resp))` 且
  `resp.status == ToolStatus::Error`;错误文本(取 `ContentBlock::Text`)含被阻塞依赖 `design`(证明是
  model-visible 错误而非静默降级);`implement` 仍 `Todo`、无 owner(`assert_task_status`/`assert_no_task_owner`)、
  `store.version()` 未变。所有 panic 分支打印 `store.ops_summary()`。
- `denied_dangerous_write_does_not_execute_tool`:走最少样板的 **`DrainHarness`** 路径。脚本 LLM 先请求
  `dangerous_write` 再收尾 `text`,`ScriptedInteractionHandler::deny_all` 拒批,`ComplexToolHandler` 作为
  tool 后端,`complex_scope(llm, handler, Some(interaction))` 组装 attended scope,`complex_agent_machine`
  仅 gate `dangerous_write`。断言:turn `Done`;`assert_interaction_decisions(log,1)`(一次 deny);
  `assert_tool_executions(handler, DANGEROUS_WRITE, 0)`(handler 日志为空 → 被拒工具从未执行);
  `assert_board_messages(store,&[])`(共享 store 未被触碰);`assert_conversation` 断言 committed=1、
  pending none、`c-danger` tool_result 为 `ToolStatus::Denied`、末条 assistant 文本收尾。失败信息由
  `assert_tool_executions` 打印 handler 逐工具调用日志承载。
- 验证结果(全部通过):`cargo fmt --all -- --check`;两个指定测试各 1 passed;整文件 3 tests 全通过;
  `cargo clippy --all-targets -- -D warnings` 无告警;`cargo test --all --all-targets` 全绿(lib 423 +
  testkit 131 + 各集成 crate 全通过,仅 credential-gated 集成测试 ignored);
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` 无告警;`git diff --check` 干净。

### [DONE] M2-R Milestone 2 Review

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

**完成记录**:

- Review 结论:主复杂 flow 稳定覆盖 M2 目标,失败信息足够定位,无需返工。逐项核对如下。
- **≥4 次 LLM 往返 + 2 次 interaction**:`complex_turn_combines_plan_blackboard_approval_deny_and_pivot`
  依次 resume 四个 `NeedLlm`——`llm_open`(开轮)→`plan_llm`(建 plan 后)→`pivot_llm`(pivot 重渲染)
  →`final_llm`(收尾),且 pivot 前的 `pre_pivot_llm` 被重渲染为同 id 的 `pivot_llm`(第 5 个渲染点),
  满足“至少 4 次往返”。interaction 恰两次:`approval_one`(Approve)、`approval_two`(Deny),
  `recorded_decisions` 断言顺序为 `Approve`→`Deny`,`assert_interaction_decisions(log,2)` 固定计数。
- **pivot 落在合法 post-tool boundary 且被后续 LLM request 看到**:pivot 在首个 dangerous 结果
  (`after_danger_one`)之后、下一次 LLM resume 之前注入;`assert_eq!(pre_pivot_llm, pivot_llm)`
  证明 pivot 是对未决 `NeedLlm` 的原地重渲染(复用同 id,而非新增待决步);`final_request` 断言其
  `messages` 含带 `PIVOT_TEXT` 的 `Role::User` 消息,证明后续模型 request 确实看到 pivot。
- **deny 后 dangerous tool 未执行且 turn 继续到 final**:第二次 approval 取 `Deny` 后 machine 不发
  `NeedTool`,直接 drain 到 `final_llm`,末轮文本把 cursor 推到 `LoopCursorKind::Done`;
  `assert_tool_executions(DANGEROUS_WRITE, 1)` 证明仅被批准的那次执行(handler 日志仅一条),
  `final_request` 含 `ToolStatus::Denied` 的 `ToolResult`。M2-2 的 `denied_dangerous_write_does_not_execute_tool`
  以 `DrainHarness` 独立复核:deny 一次、执行零次、共享 store 未被触碰、turn `Done`。
- **plan dependency blocked 是 model-visible tool error 而非 panic**:主场景断言
  `store.claim("implement", …)` 返回 `StoreError::DependencyBlocked{unfinished:[design]}`;M2-2 的
  `claim_dependency_block_returns_tool_error_and_does_not_mutate_task` 直接经 `ComplexToolHandler.fulfill`
  证明 `plan_claim` 返回 `ToolStatus::Error`(文本含被阻塞依赖 `design`),且 `implement` 仍 `Todo`、
  无 owner、plan version 不变——错误经工具结果面暴露给模型,不是 handler panic 或静默降级。
- **测试可读性**:`tests/agent_complex_flow.rs` 共 576 行,主场景已抽 `fulfill_tool`/`fulfill_interaction`/
  `resume_tool_batch`/`message_text`/`llm_request_messages`/`recorded_decisions` 等聚焦 helper,断言经
  M1 支持层的 `assert_*`/`role_sequence` 承载;无新增 DSL、无冗余样板,阶段边界注释清晰,无需重构。
- **验证结果(全部通过)**:`cargo fmt --all -- --check` 干净;`cargo clippy --all-targets -- -D warnings`
  无告警;`cargo test --test agent_complex_flow`(3 passed);`cargo test --all --all-targets` 全绿
  (lib 423 + testkit 131 + 各集成 crate 全通过,仅 credential-gated 集成测试 ignored,无 failed);
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` 无告警;`git diff --check` 干净。
- 本次 review 未改动任何被测/生产代码,仅更新 `TODO.md`(标记 `[DONE]` + 本记录)与 `memory/claude_plan.md`;
  Milestone 2 至此签收,后续进入 Milestone 3。

### [DONE] M3-1 实现 subagent + parent approval pop + shared plan/blackboard 场景

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

**完成记录**:

- 新建 `tests/agent_complex_subagent.rs`,单测
  `complex_subagent_updates_shared_plan_and_pops_approval_to_parent`,复用 `agent-testkit` 的
  `ScriptedSubagentSpawner` / `DrivingSubagentHandler` / `headless_child_scope` /
  `parent_scope_with_subagent` 与 M1 支持层的 `MockPlanBlackboardStore` / `complex_agent_machine` /
  `complex_tool_handler`。parent 用 `ScriptMachine` emit 单个 `NeedSubagent`(最少样板且完整驱动
  `DrivingSubagentHandler`);child 是真实 `DefaultAgentMachine`,scope headless(仅 llm+tool,无
  interaction),故其 dangerous-write 审批必须 pop 到 parent。
- **共享 plan**:parent 直接向同一 `Arc<MockPlanBlackboardStore>` 预置 `design→review→implement`
  依赖链并把 `design` 置 `Completed`(seeding 后 version=5,已断言)。child 首步
  `plan_claim_first_available(worker, ev=5)` 跳过已完成的 `design` 与依赖未满足的 `implement`,原子
  认领 `review`(v6);末步 `plan_update(review, Completed, ev=6)`(v7)。断言:`review` owner=worker
  且 `Completed`;`implement` 仍 `Todo`、无 owner(仅一次 first-available claim,未误领);`design`
  `Completed`;`review`/`implement` 的 `depends_on` 关系不变。
- **parent approval pop**:child 第二步 `dangerous_write` 经 `RequireDangerousWriteApprovalPolicy`
  触发 `NeedInteraction`,headless child 无法就地应答,pop 一层到 parent 的 attended
  `ScriptedInteractionHandler::approve_all()`。断言 `parent_interaction_log.len()==1` 且
  `assert_tool_executions(DANGEROUS_WRITE, 1)`——唯有批准该工具才执行(deny 会是零次)。
- **共享 blackboard**:parent 先以 sender `parent` append 一条,child 以 sender `child` append
  `review started`/`review done`,dangerous-write 以 sender `dangerous_write` append。
  `assert_board_messages` 固定 4 条按序且长度精确(顺带排除重复 side effect),再逐条断言 sender 可区分
  parent/child/工具。
- **budget 传播**:child 的 LLM 用 `ChargingLlm` 包装把 usage 计入 run context;因 child context 由
  parent 派生,四步用量 `(5+3)+(4+2)+(6+3)+(3+2)=28` 聚合到 parent 共享 ledger,断言
  `ctx.budget().snapshot().used().tokens()==28`。
- **subagent 生命周期 + trace**:`spawner.ids_calls()/spawn_calls()/summarize_calls()` 均为 1;
  `parent_log.resume_tags()==[Subagent]` 证明 parent 被 subagent 输出 resume;`assert_trace` 断言
  `subagent_count==1`、parent `NeedSubagent` 节点 `resolved_at_scope==0` 且 `Resumed`,child interaction
  节点 `resolved_at_scope==1`(pop 一层到 parent)且 `Resumed`——因 child context 与 parent 共享 trace
  记录(`Arc<Mutex<Vec<TraceRecord>>>`),故在 parent ctx 快照即可观察。
- **验证结果(全部通过)**:`cargo fmt --all -- --check` 干净;`cargo clippy --all-targets -- -D warnings`
  无告警(修正一处 `redundant_guards`);`cargo test --test agent_complex_subagent`(1 passed);
  `cargo test --all --all-targets` 全绿(lib 423 + testkit 131 + 各集成 crate 全通过,仅
  credential-gated 集成测试 ignored,无 failed);`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps
  --workspace` 无告警;`git diff --check` 干净。

### [DONE] M3-2 实现 cancel during subagent/tool wait 场景

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

**完成记录**:

- 新建 `tests/agent_complex_cancel.rs`,单测
  `complex_cancel_abandons_child_and_preserves_committed_state`,复用 `agent-testkit` 的
  `ScriptedSubagentSpawner` / `DrivingSubagentHandler` / `SpawnedChildBuilder` /
  `headless_child_scope` / `ScopePop` 与 M1 支持层的 `MockPlanBlackboardStore` /
  `complex_tool_handler` / `complex_agent_machine` / `complex_scope`。测试分两段跑在同一个
  `Arc<MockPlanBlackboardStore>` 上。
- **机制核对**:`src/agent/drive.rs::drain` 在每轮循环顶部、`fulfill_batch` 之前检查
  `ctx.is_cancelled()`;命中则把首个 pending requirement 记为 `NeverResumed`@`resolved_at_scope==0`、
  向 machine 喂 `StepInput::Abandon`、随后 break 并返回 `Ok(TurnDone)`。
  `DrivingSubagentHandler::fulfill` 先过 depth guard,再 `child_ids`→`derive_child`(继承 parent 的
  budget 与 cancellation、共享 trace `Arc`)→`spawn`→`drain(child, child_ctx)`→`summarize`。因此
  “先 cancel parent ctx 再驱动 subagent handler”会让 child_ctx 一开始就是 cancelled,child drain 在第一个
  fulfill 前就 abandon 掉 child 的 outstanding requirement——`resume_count==0` 且 handler 从不执行。
  这与参考单测 `src/agent/drive/subagent/tests.rs::parent_cancel_propagates_and_abandons_child` 同构。
  已确认 `CancelOnCall` 无法达成该形状(它在 fulfill 内部 cancel,requirement 仍会被 Resume),故采用参考写法。
- **Phase A(cancel abandons child)**:seed `create(v0)`→`add_task(review,[])(v1)`→
  `claim(review,"worker",1)(v2, InProgress)`、`post("worker","review started")`,断言 seeded version==2。
  child 是 `ScriptMachine`,emit 单个 `NeedTool`——`plan_update(review, worker, completed, ev=2)`
  (若执行会把 `review` 置 `Completed`,正是不该发生的 side effect);`.idle_on_abandon()`。child scope
  headless,tool = 共享 store 的 `ComplexToolHandler`。`ctx.cancellation().cancel()` 后
  `handler.fulfill(&spec_ref,&brief,None,&mut outer,&ctx)`→`Subagent(Ok(_))`(cancel 是有序收尾而非错误)。
  断言:`ids_calls/spawn_calls/summarize_calls==1`;`child_log.abandon_count()==1` 且 `resume_count()==0`;
  `assert_tool_executions(child_tool,PLAN_UPDATE,0)` 且 `calls().is_empty()`;`review` 仍 `InProgress`、
  owner=`worker`;`assert_board_messages(&store,["review started"])`(无重复、无 completed side effect);
  `assert_trace(&ctx).subagent_count(1)`、child `NeedTool` 节点 `resolved_at_scope(0).never_resumed()`。
- **Phase B(cancel 后仍可用)**:fresh `cleanup_ctx`,parent = `complex_agent_machine`
  (真实 `DefaultAgentMachine`),scripted LLM ① tool_use `blackboard_post("parent","review cancelled")`
  + `plan_update(review, worker, cancelled, ev=2)`(`InProgress`→`Cancelled` 合法转换,沿用 worker 的
  claim)② text;`drain(...)`→`LoopCursorKind::Done`。断言 `review`==`Cancelled`;
  `assert_board_messages(&store,["review started","review cancelled"])`(无重复 started);
  `assert_conversation(cleanup.state().conversation()).committed_turns(1).pending_none()`——证明 cancel
  只作用于其所在 run,store 与 machine 之后仍可提交新 turn。
- **确定性**:cancel 时机由显式 `ctx.cancellation().cancel()` 固定在驱动 subagent 之前,不用真实 sleep、
  网络或时钟,离线稳定;单测 <1s。
- **验证结果(全部通过)**:`cargo fmt --all -- --check` 干净;`cargo clippy --all-targets -- -D warnings`
  无告警;`cargo test --test agent_complex_cancel complex_cancel_abandons_child_and_preserves_committed_state`
  (1 passed);`cargo test --all --all-targets` 全绿(lib 423 + testkit 131 + 各集成 crate 全通过,仅
  credential-gated 集成测试 ignored,无 failed);`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`
  无告警;`git diff --check` 干净。

### [DONE] M3-R Milestone 3 Review

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

**完成记录**:

- **性质**:纯 review 任务,未改动任何被测/生产代码或测试代码;仅更新 `TODO.md`(标 `[DONE]` + 本记录)与
  `memory/claude_plan.md`。逐条核对 M3-1(`tests/agent_complex_subagent.rs`)、M3-2
  (`tests/agent_complex_cancel.rs`)及共享支持层(`tests/complex_support/`)。
- **核对 1 — child headless interaction pop 到 parent**:确认。child scope 用
  `headless_child_scope()`(仅 llm+tool,无 interaction backend),其 `dangerous_write` 经
  `RequireDangerousWriteApprovalPolicy` 触发 `NeedInteraction`,只能 pop 一层到 parent 的
  `ScriptedInteractionHandler::approve_all()`。断言真实检查:`parent_interaction_log.len()==1`、
  `assert_tool_executions(DANGEROUS_WRITE, 1)`(唯有批准才执行,deny 会是 0 次)、trace 上该 interaction
  requirement `resolved_at_scope(1).resumed()`(恰好跨一层)。
- **核对 2 — `plan_claim_first_available` 跳过 dependency-blocked item**:确认。store 实现
  (`plan_blackboard.rs::claim_first_available`)按稳定 `task_order` 扫描,谓词为
  `status==Todo && owner.is_none() && dependencies_satisfied(depends_on)`,故跳过已 `Completed` 的
  `design` 与依赖未满足的 `implement`,原子认领 `review`。测试断言 `review` owner=worker 且最终
  `Completed`;`implement` 仍 `Todo` 且 `assert_no_task_owner`(仅一次 first-available claim,未误领);
  依赖边 `review→[design]`、`implement→[review]` 不变。
- **核对 3 — shared store side effect 只发生一次,无 retry/resume 重复写**:确认。
  `assert_board_messages` 强制 `board.len()==expected.len()`(见 `assertions.rs:103`),任何重复/补跑
  的 blackboard 写入都会使断言失败;subagent 场景固定 4 条按序、逐条断言 sender 可区分
  parent/child/工具;plan 状态用精确 `assert_task_status`。
- **核对 4 — cancel path 是 abandon/never-resume,不是 wrong-family error 或 panic**:确认。cancel
  场景先 `ctx.cancellation().cancel()` 再驱动 subagent handler,child_ctx 继承 cancel,drain 在首个
  fulfill 前 abandon child 的 outstanding requirement。断言:handler 返回
  `RequirementResult::Subagent(Ok(_))`(有序收尾而非错误)、`child_log.abandon_count()==1` 且
  `resume_count()==0`、`assert_tool_executions(PLAN_UPDATE, 0)` 且 `calls().is_empty()`(工具从不执行)、
  `review` 仍 `InProgress`、trace 上该 tool requirement `resolved_at_scope(0).never_resumed()`。无 panic、
  无 wrong-family error。
- **核对 5 — cancel 后继续新 turn 的断言真实检查 committed conversation**:确认。Phase B 用 fresh
  `cleanup_ctx` + 真实 `DefaultAgentMachine` 跑一整轮(`InProgress`→`Cancelled` 合法转换),
  `assert_conversation(cleanup.state().conversation()).committed_turns(1).pending_none()` 直接读机器
  的 conversation state,证明 committed 1 轮且无 pending;board 增至 `["review started","review cancelled"]`
  无重复 started。
- **核对 6 — 仅 mock agent effect 边界,无 provider wire mock**:确认。两测均用
  `ScriptedLlmHandler` / `ScriptMachine` / `DefaultAgentMachine` / `ScriptedInteractionHandler` /
  `ScriptedSubagentSpawner` 等 effect 边界替身,无任何 Anthropic/OpenAI HTTP、SSE 或 raw JSON wire mock;
  无真实 sleep/网络/时钟,离线确定性(每测 <1s)。
- **核对 7 — 无过早抽象成 DSL**:确认。新增 helper 仅 `ChargingLlm`(证明 child usage 计入 parent
  ledger)与两个 trace/id 小工具,均为可读性服务,未引入 scenario DSL。
- **验证结果(全部通过)**:`cargo fmt --all -- --check` 干净;`git diff --check` 干净;
  `cargo clippy --all-targets -- -D warnings` 无告警;`cargo test --test agent_complex_subagent`(1 passed);
  `cargo test --test agent_complex_cancel`(1 passed);`cargo test --all --all-targets` 全绿(29 个测试
  二进制 `test result: ok`,0 failed,仅 credential-gated 集成测试 ignored);
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` 无告警。
- **结论**:Milestone 3 的 subagent 动态作用域、共享 plan/blackboard side effect、budget 传播、
  cancel never-resume 与 cancel 后继续新 turn 均被复杂测试真实覆盖且断言到位,签核通过。可进入 Milestone 4。

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
