# 执行计划 — M1-R Milestone 1 Review

## 选中的任务
`TODO.md` 第一个未完成任务 = **M1-R**(M1-1/M1-2/M1-3 均已 `[DONE]`)。HEAD=c4d0d38,工作树 clean。
这是 Review 任务,**不拆分**。核对 M1 拓扑/id source/fixtures 是否形成稳定基础且未引入 provider wire mock 或运行时语义变化。

## Review 核对项(TODO.md M1-R)
1. Cargo 拓扑:确认采用 `crates/agent-testkit`(工作区成员)还是过渡支持模块,并记录理由。
2. testkit 只依赖 `agent-lib` 公开 API。
3. `SeqIds` 覆盖 `RequirementIds`、`ToolExecutionIds` 与常用 Agent/Conversation id。
4. fixtures 只产生 provider-neutral 类型。
5. 更新 PLAN.md / docs/TESTABILITY.md 中与实际拓扑不一致的描述。

## 核对结论(代码巡检)
- 拓扑:root `Cargo.toml` = `[workspace] members=[".","crates/agent-testkit"] resolver="3"`;
  testkit 单向 `agent-lib = { path="../.." }`,agent-lib 无反向 dev-dep → 无依赖周期。采用首选 crate 形态,
  非过渡模块。→ 与 PLAN.md 建议目录一致。
- 只依赖公开 API:testkit `use agent_lib::{agent::*, client::*, conversation::*, model::*}` 全部公开路径;
  Cargo.toml 无 mockall/proptest/insta,仅复用基础依赖。Rust 跨 crate 只能访问 `pub`,天然保证。
- `SeqIds`:`impl RequirementIds`(next_requirement_id)+`impl ToolExecutionIds`
  (tool_call_id/tool_result_message_id/next_assistant_message_id/next_step_id);inherent helper 覆盖
  run/agent/tool_set/conversation/turn/message/tool_call/step/trace_node/requirement id。clone 共享 counter、
  fork 新子树、named 重贴 label、exhausted/with_budget 失败模式、requirement_log 保序可查。
- fixtures:全部经公开构造器产出 provider-neutral 类型(Message/Response/ToolCall/ToolResponse/Tool/
  AgentSpec/AgentState/RunContext/DefaultAgentMachine),无 Anthropic/OpenAI wire JSON,无 private API。

## 文档一致性动作
- PLAN.md 过渡门(`允许先以 tests/support/agent_testkit 过渡`)已定案 → 追加“已定案:crate 形态”一句。
- docs/TESTABILITY.md §5.0(实现路径)候选项 → 补一句“实际已落地 crates/agent-testkit 工作区成员”。

## 步骤
1. [x] 巡检 Cargo.toml / lib.rs / prelude.rs / ids.rs / fixtures.rs。
2. [x] 巡检 PLAN.md / docs/TESTABILITY.md。
3. [ ] 最小文档更新(PLAN.md + TESTABILITY.md 各一处 resolved 注记)。
4. [ ] 验证:fmt --check → clippy -Dwarnings → test -p agent-testkit → doc -Dwarnings → diff --check。
       全量 `cargo test --all` 自 M1-3(c4d0d38,全绿)以来无代码变更,本任务仅改文档 → 复用上次绿结果。
5. [ ] TODO.md 标 M1-R [DONE] + 完成记录(写 review 结论)。
6. [ ] 提交,停止。

## 进度/发现
- [x] 文档更新完成:PLAN.md 过渡门定案注记;TESTABILITY.md §4 补 crate 形态 resolved 注记 + subagent.rs。
- [x] 验证全绿:fmt --check;clippy -Dwarnings(root + testkit);test -p agent-testkit(14+2);
      cargo test --all --all-targets(agent-lib 434 + testkit 14+2,0 failed,7 network-gated ignored);
      doc -Dwarnings(root + testkit);diff --check 干净。
- [x] TODO.md M1-R 标 [DONE] + 完成记录(review 结论、拓扑决策、无偏离)。
- [ ] 提交并停止。
