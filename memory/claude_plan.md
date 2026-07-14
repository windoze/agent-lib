# 当前任务：M4-3 实现 assertions 模块

## 目标（TODO.md M4-3，前置 M4-2 已 DONE）
在 `crates/agent-testkit/src/assertions.rs`（改为目录模块）实现只读高层断言：
- `assert_conversation`：committed_turns、pending_present/none、message_role、
  message_text、last_assistant_text、tool_result_status、pairing_count、open_call_count。
- `assert_requirements` / `RequirementView`：single family、origin、id、request summary。
- `assert_notifications`：tool started/finished、step boundary count/order、boundary metadata。
- `assert_trace`：requirement resolved_at_scope、disposition、subagent parent chain。
- `assert_budget`：steps/tokens/cost。
- `assert_calls`：handler call count、request count、completion order、peak concurrency。
- 附带 `assert_done(&TurnDone)` 便捷断言（DESIGN 示例用到）。

断言必须只读、不修改 machine/context；失败信息含足够上下文。

## 关键 API 事实（已核对）
- Conversation.turns()/pending()/tool_call_index(); Turn.messages()/pairings()/id()。
- ConversationMessage.payload()->&Message; Message{role,content:Vec<ContentBlock>}。
- ContentBlock::{Text{text},ToolUse{id,name},ToolResult{tool_use_id,status},...}。
- PendingTurn.open_calls() 迭代未配对调用。
- Notification::{Llm,StepBoundary,ToolCallStarted,ToolCallFinished}; 各带 step_id/call_id/metadata。
- RunContext.trace().records()->Vec<TraceRecord>; budget().snapshot()->BudgetSnapshot。
- 需求 trace 节点 id = requirement.id.to_string(); TraceNodeKind::Requirement{kind_tag,resolved_at_scope,disposition}。
- BudgetSnapshot.used()->BudgetUsage{steps,tokens,cost_micros}。
- Requirement{id,origin:AgentPath,kind}; kind.tag(); AgentPath::is_root()。
- CallLog.len()/completed_len()/with_records/requests; 需新增 peak_concurrency 追踪。
- TurnDone.cursor().kind()->LoopCursorKind::{Done,Error,..}; 处理器 log()->&Arc<CallLog>。

## 设计决策
- CallLog 增强(script.rs): CallLogState 增 in_flight/peak; begin 更新峰值, complete 首次完成自减;
  新增 pub fn peak_concurrency(&self)->usize。assert_calls 峰值断言的必要前置能力，非 workaround。
- assertions 目录模块: assertions.rs(root re-export) + assertions/{conversation,requirements,
  notifications,trace,budget,calls,done}.rs, 各带 #[cfg(test)]。
- builder 持引用 Copy, 链式返回 Self, 失败 panic!("{msg}") String payload 便于快照测试。
- prelude 追加所有 assert_* 入口与 view 类型。

## 步骤
1. [x] 写 plan。
2. [x] script.rs 增 CallLog peak(+2 单测)。
3. [x] 建 assertions 目录模块与 root。
4. [x] 各子模块 + 单测(7 子模块,17 单测)。
5. [x] prelude 再导出。
6. [x] 改写 tests/cassette_replay.rs offline_replay 用 assertions。
7. [x] fmt -> clippy(-D warnings 无告警)-> 聚焦 -> 全量(434 + 104 全绿)-> rustdoc(-D warnings)-> git diff --check 干净。
8. [x] TODO.md 标 [DONE] + 完成记录。
9. [ ] 提交并停止。

## 备注
- 无已知阻塞 spec 偏差；未发现未排期失败测试。
- 每类断言 happy path + 至少一个 failure message 快照测试(catch_unwind)。
