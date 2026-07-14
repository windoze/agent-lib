# 执行计划 — M2-R Milestone 2 Review

## 选中的任务
`TODO.md` 第一个未完成任务 = **M2-R Milestone 2 Review**(line 480)。
M1-* 与 M2-1..M2-4 均 `[DONE]` 且已提交(HEAD=`0875598` = M2-4),工作树 clean。
Review 任务(`*R`)——**不拆分**,亦不改代码,只做核对 + 迁移目标标记 + 完成记录。

## 任务要求(TODO.md M2-R)
前置依赖 M2-1..M2-4。做什么:
1. 核对 handler result family 是否正确。
2. 核对 `TestScope` 默认不 total。
3. 核对 `ScriptMachine` 能覆盖 driver/pop/subagent 测试所需语义。
4. 标记优先迁移目标:至少列出 `tests/agent_effect_e2e.rs` 与
   `src/agent/drive/reference/tests.rs` 中可删除的 fake 类型。
验证:全套验证命令通过;Review 结论写入完成记录。

## 核对结论(证据)
1. **family 对齐**:`handlers.rs` 每个 `fulfill` 用 `into_result()` 产族内结果;script 耗尽
   (`StrictMode::Error`)按 family 折叠为族内 `Err`(Llm→`Llm(Err(ClientError::Other))`,
   Tool→`Tool(Err(ToolRuntimeError::ExecutionFailed))`,Reconfig→`Reconfig(Err(InvalidRegistry))`);
   `MisalignedHandler` 专门返回 wrong-family 以触发 `drain` 的 `RequirementKind::accepts` 校验。✔
2. **TestScope 不 total**:`scope.rs` 每个 accessor `self.<slot>` → `inner` → `None`,未挂且无 inner
   即 `None`;headless(未挂 interaction)顶层 `NeedInteraction` 暴露 `UnhandledRequirement`。✔
3. **ScriptMachine 覆盖语义**:`machine.rs` step:External 吐固定 batch + 非 terminal waiting cursor
   (streaming_step);Resume 记 order/tag、移 outstanding、全清且 `done_after_all_resumed`→`Done`;
   unknown id→`Error` cursor(不吞);Abandon 计数 + 可配 `abandon_cursor`。是 `ParentBatchMachine`
   (batch=NeedTool+NeedSubagent、resume→Done、abandon→Done)的严格超集,符合 docs §5.7。✔

## 迁移目标(至少列出可删除 fake)
### tests/agent_effect_e2e.rs 可由 M2 层替换
- `SeqIds` → testkit `SeqIds`(ids.rs)
- `EmptyScope` → `TestScope::empty()`
- `ParentScope` → `TestScope::builder().tool/.interaction/.subagent`
- `ParentBatchMachine` → `ScriptMachine`(M2-4 headline;abandon_cursor(Done)+done_after_all_resumed)
- `CountingApproveInteraction` → `ScriptedInteractionHandler::approve_all()` + CallLog 计数
- `CountingToolHandler` → `ScriptedToolHandler`(固定 ToolStep::ok)+ ToolCallLog 计数
- `FakeToolRegistry` → `ScriptedToolRegistry`
- `ChildScope`/`ObservingScope` → `TestScope::builder()`(可 wrapping)
- payload 助手(weather_tool/text_block/user_message/usage/assistant_response→assistant_text/
  tool_use_response→assistant_tool_use/tool_response/agent_id/tool_set_id/conversation_id)→ testkit fixtures + SeqIds
延后(非 M2 层,属 M5/M6):`FakeClient`(LlmClient,testkit 刻意不 mock)、`ChildSpawner`(SubagentSpawner,M5-3)、
`ConcurrentToolHandler`(peak 并发工具,M5-1)、`RequireApprovalPolicy`(ToolApprovalPolicy,无 testkit 型)、`ChargingLlmHandler`(token 计费,待定)。

### src/agent/drive/reference/tests.rs 可由 M2 层替换
- `ScriptedRequirementIds` + `FakeToolIds` → testkit `SeqIds`(RequirementIds+ToolExecutionIds)
- `FakeToolRegistry` → `ScriptedToolRegistry`(经 reference ToolRegistryHandler)
- `ScriptedApprovalInteraction` → `ScriptedInteractionHandler`(反应式 approve/deny 决策)
- `ComposedScope` → `TestScope::builder().interaction(..).wrapping(Arc<ReferenceScope>)`
- fixture 助手(weather_tool/calendar_tool/usage/assistant_response/tool_use_response/tool_response/
  user_message/spec 等)→ testkit fixtures
延后:`FakeClient`(LlmClient,ReferenceScope 适配器测试必需,testkit 不 mock)、
`CancellingLlmHandler`/`PanicToolHandler`/`CancelScope`(M5-2 cancel/panic wrappers)、
`RequireApprovalPolicy`(ToolApprovalPolicy)。

## 步骤
1. [x] 读 M2-1..M2-4 完成记录 + testkit 源(handlers/scope/machine)+ 两个迁移目标文件 + docs §5.7。
2. [x] 运行验证:fmt --check;clippy --all-targets -Dwarnings;test -p agent-testkit。
       全套 `cargo test --all --all-targets` 自 M2-4(HEAD=0875598)绿后无代码改动、本任务仅改 md,
       按规则复用上次绿结果并在完成记录说明跳过。
3. [x] TODO.md 标 M2-R [DONE] + 写 Review 结论/迁移目标到完成记录。
4. [ ] 提交并停止。
