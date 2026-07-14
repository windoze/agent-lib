# 当前任务：M1-2 实现 complex tool adapter、tool declarations 与 approval policy helpers

## 定位
- `TODO.md` 第一个未完成任务 = **M1-2**（首个 `[TODO]`）。前置依赖：M1-1（已 `[DONE]`，commit 832e1f1）。
- HEAD=832e1f1，工作树干净。本轮为复杂 Mock 测试与 Plan 依赖语义。

## 目标（TODO.md M1-2）
在 `tests/complex_support/tools.rs` 建立复杂测试 tool 适配层，站在 `RequirementKind::NeedTool` 边界，
不 mock provider wire，不使用真实 ToolRegistry 后端。

### 工具名常量
PLAN_CREATE / PLAN_ADD_TASK / PLAN_CLAIM / PLAN_CLAIM_FIRST_AVAILABLE / PLAN_UPDATE、
BLACKBOARD_POST / BLACKBOARD_READ、DANGEROUS_WRITE / SAFE_READ。

### tool_declarations() -> Vec<Tool>
每个工具的 JSON input schema，供 agent_spec_with_tools 使用。

### ComplexToolHandler（impl ToolHandler）
- 持 Arc<MockPlanBlackboardStore>。
- per-tool call log（name + input + outcome status），可断言 dangerous tool 执行次数与 input。
- 按 ToolCall.name 分发到 store 操作。
- 成功 -> Tool(Ok(ToolResponse{status:Ok}))。
- store 错误 / 参数解析错误 -> Tool(Ok(ToolResponse{status:Error}))（model-visible，不 panic）。
- unknown tool -> Tool(Err(ToolRuntimeError::UnknownTool))（固定选此风格，测试锁定）。
- dangerous_write：post 到 blackboard（sender=dangerous_write）可观察副作用。
- safe_read：read blackboard，auto-allow 普通 tool。

### RequireDangerousWriteApprovalPolicy（impl ToolApprovalPolicy）
- dangerous_write -> ApprovalRequirement::required。其他 -> AutoApprove。

### scenario setup helpers
- complex_agent_machine(ids) -> 带全部 complex tools 声明 + approval policy 的 DefaultAgentMachine。
  （store 不进 machine：machine 只持声明+policy，store 走 tool handler / scope 边界，
  故另设 complex_tool_handler(store)；避免 unused-param 告警，属 helper 人体工学而非 spec 偏离。）
- complex_tool_handler(store) -> Arc<ComplexToolHandler>。
- complex_scope(llm, tool, interaction) -> 组装 TestScope。

### 辅助改动
- plan_blackboard.rs 的 TaskStatus 增加 from_label（tool adapter 解析 status 字符串需要）。
- mod.rs 增加 pub mod tools;。

## 新增单测（tests/agent_complex_support.rs）
1. plan_tools_return_model_visible_errors
2. dangerous_write_requires_approval_and_safe_tools_do_not
3. dangerous_write_call_log_counts_executions
（均 #[tokio::test]，通过 handler.fulfill / policy.approval_requirement 直接驱动。）

## 验证顺序
- cargo fmt --all -- --check
- cargo test --test agent_complex_support <三个测试名>
- cargo clippy --all-targets -- -D warnings
- cargo test --all --all-targets（<=30min）
- RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
- git diff --check

## 完成后
- TODO.md M1-2 标题 [TODO]->[DONE]，补完成记录。提交 [M1-2] ...。停止。

## 进度
- [完成] tools.rs + 3 单测 + from_label + TODO 完成记录，全部验证门通过，待提交。
