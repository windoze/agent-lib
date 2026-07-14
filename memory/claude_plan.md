# 执行计划 — M1-3 provider-neutral fixtures

## 选中的任务
`TODO.md` 第一个未完成任务 = **M1-3**(M1-1、M1-2 已 `[DONE]`)。HEAD=a767125,工作树 clean。
非 Review 任务,不拆分;仅改动 testkit(fixtures.rs + prelude.rs),无 agent-lib 语义变更。

## 目标(TODO.md M1-3)
在 `crates/agent-testkit/src/fixtures.rs` 用 agent-lib 公开构造器实现 provider-neutral fixtures:
- message/content:`text_block`、`user_message`、`user_input(&SeqIds, text)`。
- LLM response:`assistant_text(text, usage)`、`assistant_tool_use(calls, usage)`、`usage(input, output)`。
- tool:`tool_call(provider_id, name, input)`、`tool_response(provider_call_id, text, status)`、
  `tool_ok`、`tool_error_response`。
- declaration:`weather_tool()`、`calendar_tool()`。
- agent:`agent_spec`、`agent_spec_with_tools`、`agent_state`、`default_machine`、`root_context(&SeqIds)`。
- 只用公开构造器,禁 private API / unchecked mutation。
- prelude re-export 常用 fixtures。

## 设计(公开构造器签名)
- `text_block(text) -> ContentBlock::Text`;`user_message(text) -> Message{User,[text]}`。
- `user_input(&SeqIds, text) -> AgentInput`:turn/message/assistant_message/step id 全取自 SeqIds。
- `usage(in,out) -> Usage`;`assistant_text -> Response(end_turn)`;`assistant_tool_use(Vec<ToolCall>) -> Response(tool_use)`。
- `tool_call(id,name,input) -> ToolCall`;`tool_response(id,text,status) -> ToolResponse`;
  `tool_ok`=Ok、`tool_error_response`=Error。
- `weather_tool()/calendar_tool() -> Tool`。
- `agent_spec(&SeqIds)`=空 toolset;`agent_spec_with_tools(&SeqIds, Vec<Tool>)`;
  `agent_state(&SeqIds, AgentSpec) -> AgentState`(conversation id 取自 SeqIds);
  `default_machine(&SeqIds, AgentState) -> DefaultAgentMachine`(RequirementIds+ToolExecutionIds 均用 ids.clone(),NonStreaming);
  `root_context(&SeqIds) -> RunContext::new_root(run_id, unbounded, trace_node("root"))`。

## 步骤
1. [x] 写 fixtures.rs(带项文档,满足 #![warn(missing_docs)])。
2. [ ] prelude re-export 常用 fixtures。
3. [ ] 单测:user_input 产出合法 UserMessage(role=User);assistant text/tool_use 被 DefaultAgentMachine fold
       的最小 smoke(text→提交 turn;tool_use→NeedTool);tool 声明经 ToolSetRef round-trip 稳定。
4. [ ] fmt → clippy -Dwarnings → test -p agent-testkit → test --all --all-targets → doc -Dwarnings → diff --check。
5. [ ] TODO.md 标 M1-3 [DONE] + 完成记录。
6. [ ] 提交,停止。

## 进度/发现
- (进行中)

## 完成(M1-3)
- fixtures.rs 落地全部 message/response/tool/declaration/agent helpers(公开构造器);prelude re-export。
- 6 个单测:user_input role=User;assistant_text→提交 turn;assistant_tool_use→NeedTool;
  tool 状态;ToolSetRef round-trip;root_context depth0/unbounded。
- 全套验证绿(fmt/clippy -Dwarnings/test -p 14+2/test --all 全 ok/doc -Dwarnings/diff --check)。
- 已标 TODO.md M1-3 [DONE] + 完成记录;PLAN.md 无需改。
