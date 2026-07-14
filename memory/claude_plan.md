# 当前任务：M1-R Milestone 1 Review

## 定位
- `TODO.md` 第一个未完成任务 = **M1-R**（首个 `[TODO]`，行 250）。前置依赖：M1-1..M1-3（均 `[DONE]`）。
- HEAD=c9d325f（[M1-3]），工作树干净。属于 Milestone 1「Support 与 Mock Vertical Features」。
- Review 任务，**不拆分**。产出=核对 + 验证命令全绿 + 结论写入完成记录。

## Review 检查项（TODO.md M1-R）
1. MockPlanBlackboardStore 的 dependency / claim / claim-first / blackboard append-only 语义。
2. tool adapter 只站在 ToolHandler/ToolRegistry effect 边界。
3. approval policy 只 guard dangerous_write，不影响 safe tools。
4. helper 失败信息含 store ops / role sequence / handler log。
5. 支持层仍留在 tests/complex_support/，未提前移到 agent-testkit。

## 代码核对结论（已读源码）
- plan_blackboard.rs：dependency（UnknownTask/SelfDependency/DependencyCycle）、claim（版本 CAS + owner + 可转移 + 依赖完成，失败不改状态）、claim_first_available（stable order，仅 Todo+无 owner+依赖满足）、blackboard append-only（offset=len 单调，无删/改路径）。✓
- tools.rs：ComplexToolHandler 通过 ToolHandler::fulfill 的 NeedTool 边界派发；store/arg 错误折成 ToolStatus::Error，未知工具 -> ToolRuntimeError::UnknownTool；无 provider wire mock。✓
- RequireDangerousWriteApprovalPolicy 只对 DANGEROUS_WRITE required，其余 AutoApprove。✓
- assertions.rs：plan/board 断言失败打印 ops_summary；role_sequence/pivot 打印会话摘要；tool/interaction 断言打印调用日志。✓
- 位置：全部在 tests/complex_support/，未移入 crate。✓
- 单测覆盖（agent_complex_support.rs）：dependency/claim/claim-first/append-only/tool errors/approval-gating/call-log/assertions helpers 均有测试。✓

## 验证顺序
- cargo fmt --all -- --check
- cargo clippy --all-targets -- -D warnings
- cargo test --test agent_complex_support
- cargo test --all --all-targets（<=30min）
- RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
- git diff --check

## 完成后
- TODO.md M1-R 标题 [TODO]->[DONE]，补 Review 结论完成记录。提交 [M1-R] ...。停止。

## 进度
- [完成] 代码核对 + 全部验证命令通过(fmt/clippy/agent_complex_support 10 passed/全量 620 passed 0 failed 7 credential-gated ignored/doc/diff --check)。TODO.md M1-R 标记 [DONE] 并写入 Review 结论。待提交 [M1-R]。
