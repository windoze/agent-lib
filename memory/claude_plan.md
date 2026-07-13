# 执行计划 — M2-R Milestone 2 Review

## 选中的任务
`TODO.md` 第一个未完成任务是 **M2-R**(M2-1..M2-4 已 `[DONE]`)。这是审阅任务,不拆分。

## 审阅目标(TODO M2-R "做什么")
1. 审计 `machine.rs`(整个 `src/agent/machine/`):`step` 及调用链**无 `await`、无 client/tool/进程调用**。用 grep 断言。
2. 核对 requirement/notification 二分正确;turn 结束由 `quiescent + cursor`(Done/Error)表达,无 `Done` 事件。
3. 核对乱序回灌一批 tool result 的确定性(BTreeMap<RequirementId, ToolSlot> 路由);approval 三态(approve/deny/timeout)语义与旧 loop 等价(共享 `approval_response_for_decision`)。
4. 核对所有 tool result / assistant message 仍走 Conversation 受检 append(start_assistant_response/finish_assistant/register_tool_calls/append_tool_response/commit_pending),未新造 bypass。

## 验证命令(全套)
- `cargo fmt --all`(check)
- `cargo clippy --all-targets -- -D warnings`
- `cargo test --lib agent::machine`(聚焦)
- `cargo test --all --all-targets`(≤30min)
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`
- `git diff --check`

## 说明
纯 review 任务:不改代码除非发现 spec 违背/缺陷。若审阅全绿,写结论进 TODO.md 完成记录,标 `[DONE]`,提交并停。

## 进度
- [ ] 审计 1:sans-io 纯度(grep await/client/tool)
- [ ] 审计 2:requirement/notification 二分,无 Done 事件
- [ ] 审计 3:乱序回灌确定性 + approval 三态
- [ ] 审计 4:Conversation 受检 append,无 bypass
- [ ] 运行全套验证命令
- [ ] 写完成记录,标 [DONE],提交

## 执行结果(已完成)
四项审计全部通过,纯审阅零代码改动:
- ① sans-io 纯度:grep 无 await/async/tokio/spawn/channel(仅形近误报);无 client/tool/process 调用。
- ② 二分 + 无 Done 事件:Notification 无 Done 变体;turn 结束走 cursor Done/Error + quiescent;机器不造 AgentEvent::Done。
- ③ 乱序确定性:running BTreeMap 按 RequirementId 路由 + 预分配 result_message_id;approval 三态复用 legacy approval_response_for_decision。
- ④ 受检 append:仅公共 API,无 history_mut/push_message bypass。
验证:fmt/clippy/machine(26)/full(417)/doc/git diff --check 全绿。
M2-R 已标 [DONE] 并写完成记录。下一次调用处理 M3-1。
