# 执行计划 — M2-2 实现 scripted effect handlers

## 选中的任务
`TODO.md` 第一个未完成任务 = **M2-2**(M1-* 与 M2-1 均 `[DONE]`)。
HEAD=2ca5c5b(`[M2-1] Implement script model...`),工作树 clean。非 Review 任务,单一可验证单元
(handlers 层),**不拆分**。

## 任务要求(TODO.md M2-2)
- `handlers.rs`:`ScriptedLlmHandler: LlmHandler`、`ScriptedToolHandler: ToolHandler`、
  `ScriptedInteractionHandler: InteractionHandler`(提供 `approve_all`/`deny_all`/按顺序决策 helper)、
  `ScriptedReconfigHandler: ReconfigHandler`。
- 可选 `ScriptedToolRegistry: ToolRegistry`(供 reference-scope 经 `ToolRegistryHandler` 的测试)。
- 常规错误必须返回对应 family 的 `RequirementResult::*(Err(..))`,不得用 wrong family 表达失败。
- 提供 `MisalignedHandler`/测试 wrapper,专门触发 `drain` 的 result family 校验错误。
- 从 `prelude` re-export 常用 handler。
- 验证:handler 结果被对应 `RequirementKind::accepts` 接受;四个 family 的错误路径都保留在正确 family;
  misaligned wrapper 触发 `drain` misaligned 错误;全套验证命令。

## 设计
- 每个 scripted handler 持有 `Arc<Script<Step>>` + `Arc<CallLog<Req, RequirementResult>>`,
  以便测试 clone Arc 在 drain 后读取 call log。构造:`new(Arc<Script>)` 与 `from_steps(iter)`;
  accessor:`script()`、`log()`。
- `into_result()` 保证 family 对齐;脚本耗尽(StrictMode::Error)按 family 折叠为在族内的 Err:
  - LLM → `Llm(Err(ClientError::Other(script_err)))`。
  - Tool → `Tool(Err(ToolRuntimeError::ExecutionFailed { tool_name=call.name, message=script_err }))`。
  - Reconfig → `Reconfig(Err(ToolRuntimeError::InvalidRegistry { message=script_err }))`。
  - StrictMode::Panic 时 `next_step` 自身 panic(opt-in),handler 不折叠。
- Interaction 无 Err family,故 handler 采用 reactive 决策模型(与 reference 的
  `ApprovalInteractionHandler` 一致):
  - `approve_all()` / `deny_all(msg)`:对每个 approval 反应式构造 `ApprovalResponse`
    (地址取自 request 的 step_id/call_id,保证 `accepts_response` 通过);Question->answer(""),Choice->Choice(0)。
  - `sequence(decisions)`:`InteractionDecision`(Approve/ApproveWith/Deny/Answer/Choice/Response)
    按 dispatch 顺序消费;耗尽时按 StrictMode(Error->可配 default disposition,默认 Deny;Panic->panic)。
  - log:`CallLog<Interaction, InteractionResponse>`。
- `ScriptedToolRegistry: ToolRegistry`(+ Debug):`declarations()` 返回声明的 tools;
  `execute()` pop `Script<ToolStep>`,耗尽折叠为 `ToolRuntimeError::ExecutionFailed`。可配 call log。
- `MisalignedHandler`:持有一个 wrong-family `RequirementResult`,同时实现四个 handler trait,
  `fulfill` 恒返回该 result。构造 `returning(result)` + 便捷 `llm_as_tool()` 等。
- prelude 追加导出。

## 步骤
1. [x] 实现 crates/agent-testkit/src/handlers.rs(含 12 个单测)。
2. [x] prelude.rs 追加 6 handler + InteractionDecision + 4 CallLog 别名导出。
3. [x] 验证全绿:fmt --check;clippy -Dwarnings(root + testkit);test -p agent-testkit(38 lib + 2 smoke);
       test --all --all-targets(agent-lib 434 + testkit 38+2,0 failed,7 network-gated ignored);
       doc -Dwarnings(root + testkit,修一处 redundant link 与一处 Deny(None) 误判后干净);diff --check 干净。
4. [x] TODO.md 标 M2-2 [DONE] + 完成记录。
5. [ ] 提交并停止。

## 进度/发现
- interaction 采用反应式 `InteractionDecision`(非直接包 `Script<InteractionStep>`):interaction family 无
  `Err`,且 approval 响应须寻址活 request 的 step_id/call_id;approve_all/deny_all/sequence 均反应式构造。
- ScriptedToolHandler 与 ScriptedToolRegistry 共用 `tool_step_result` 折叠;registry `execute` 从
  `RequirementResult::Tool` 解出内层 `Result`(其余分支 `unreachable!`)。
- MisalignedHandler 同实现四 trait,返回固定 wrong-family result,经内联 `LlmScope` + `drain` 断言
  `AgentError::Other` 含 "misaligned"。
