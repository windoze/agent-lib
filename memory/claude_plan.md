# 执行计划 — M5-2：`SubagentHandler`：派生、再开一层 drain 与作用域强制

## 选中的任务
`TODO.md` 第一个未完成任务 = **M5-2**（M1..M5-1 全 `[DONE]`；M5-2 起为 TODO）。前置 M5-1 已完成。

## 任务目标（TODO.md M5-2）
1. 实现 `SubagentHandler`：接收 `NeedSubagent{spec_ref,brief,result_schema}`（只有 data），
   `RunContext::derive_child` 派生子上下文，构造子机器，`drain` 递归驱动；子机器本内层 scope
   兜不了的 requirement（如 `NeedInteraction`）pop 到外层。
2. 深度上限：每加深一层在 handler 检查，超限分类报错。
3. 预算继承 / cancel 传播：子上下文共享父 budget ledger、继承 cancel 链；父 cancel → 子
   `Abandon` 并 `cancel_pending` 收尾。
4. 子 turn 结束把 `SubagentOutput` 作为 `RequirementResult::Subagent(..)` `Resume` 回父。
5. pop 从外层起（§7.3）：subagent handler 内部 perform 的同类 requirement 不回到它自己。

## 关键设计决策
- trait 签名变更：`SubagentHandler::fulfill` 增加 `outer: &mut dyn Pop`（= 发出 NeedSubagent 那层
  scope 作为 pop 目标 ScopePop{scope,parent}）。子机器 drain 时把 outer 作为 parent，故子的
  NeedInteraction 兜不住时 pop 到父 attended scope 兑现。§7.3 由 drain(先试本层再 pop) 天然满足。
- drain 管道（drive.rs）：fulfill_with_scope 对 NeedSubagent 返回 None；resolve_requirement 特判
  NeedSubagent（有 handler 则构造 outer=ScopePop::new(scope,parent) 调 fulfill，否则 pop）；
  fulfill_batch 把 subagent 从并发集排除，与 popped 一起串行经 resolve_requirement。
- 深度（context.rs）：RunContext 加 depth:u32，new_root=0，derive_child=+1，depth() 访问器；
  handler 检查 ctx.depth()>=max_depth 拒绝。
- 分类报错（event.rs）：新增 AgentErrorKind::Subagent + AgentError::SubagentDepthExceeded{limit}。
- 参考实现（新 drive/subagent.rs）：SubagentSpawner(child_ids/spawn/summarize) +
  SpawnedChild{machine:Box<dyn AgentMachine+Send>,scope:Box<dyn HandlerScope>,opening:AgentInput} +
  DrivingSubagentHandler{spawner,max_depth}。fulfill：深度护栏→derive_child(共享 budget/派生
  cancel/记 sub-agent trace)→spawn→drain(child,opening,child_scope,Some(outer),&child_ctx)→
  summarize→Subagent(Ok/Err)。预算继承/cancel 传播由 child_ctx 共享父 ledger+派生 cancel 天然获得。

## 聚焦测试（drive/subagent/tests.rs）
1. attended 父 scope 经 drain 驱动 mock 父机(发 NeedSubagent)→handler 驱动 mock 子机(发
   NeedInteraction, headless 子 scope)→子 interaction pop 到父兑现(counting==1)，父/子完成。
2. 深度超限：ctx.depth()==max_depth 调 fulfill→Subagent(Err(SubagentDepthExceeded))。
3. cancel 传播：父 ctx 已 cancel，fulfill 用真实 DefaultAgentMachine 子机→drain 见 cancel→
   Abandon→cancel_pending→子 cursor 落 Idle、子 LLM handler 调用 0 次。
4. budget 继承：真实 DefaultAgentMachine 子机正常完成，子 LLM handler 在 ctx 上 charge_tokens(N)→
   父 ctx budget snapshot tokens==N（derive_child 共享 ledger）。

## 验证
cargo fmt --all → clippy --all-targets -D warnings → test --all --all-targets → RUSTDOCFLAGS=-D
warnings cargo doc --no-deps → git diff --check。每测试 <1min。

## 进度（恢复上一轮中断的工作）
- [x] 选中 M5-2，读 TODO/PLAN/迁移文档 §7.2/§7.3/§8、drive/context/requirement/nested/spec
- [x] context.rs depth（已在工作区）
- [x] event.rs 深度错误分类（已在工作区）
- [x] drive.rs trait 签名 + 管道（已在工作区，但引入生命周期编译错误）
- [x] drive/subagent.rs 参考实现 + 导出（已在工作区，声明 `mod tests;` 但文件缺失）
- [ ] **修复编译错误**：ScopePop 单一 `'a` 让 scope 与 parent 可变引用 pointee 统一（invariant）→
      在 resolve_requirement 内构造 ScopePop 无法统一。解法：ScopePop 加第二个生命周期 `'p`
      解耦 parent pointee；resolve_requirement 恢复独立省略生命周期。
- [x] **修复编译错误**：ScopePop 加第二个生命周期 `'p` 解耦 parent pointee；
      resolve_requirement 恢复独立省略生命周期。lib 编译通过。
- [x] 补齐 drive/subagent/tests.rs 聚焦测试 4 个（mock 机器/scope）：全绿
- [x] 全套验证：fmt(clean)→clippy(0 warn)→test(lib 432/0)→doc(-D warnings clean)→diff --check(clean)
- [x] TODO.md M5-2 标 [DONE] + 完成记录
- [x] 一次性提交所有未提交文件（恢复上一轮中断的工作）

## 状态：M5-2 完成，等待提交。
