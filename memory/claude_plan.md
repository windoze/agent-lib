# 执行计划 — M3-3 参考 driver:复跑现有 loop 集成测试

## 选中的任务
`TODO.md` 第一个未完成任务 = **M3-3**(M3-2 及之前全部 `[DONE]`)。里程碑 3 阶段 2 验收。
非 review 实现任务,不拆分。前置 M3-2 已完成(drive.rs 有 drain / Pop / ScopePop / TurnDone)。

## 目标(TODO M3-3 "做什么")
1. 在 `src/agent/drive/reference.rs` 提供参考 driver:单层 `HandlerScope`,
   llm = `LlmClient` 包装、tool = `ToolRegistry` 包装、interaction = approval 决策后端;
   对一个 `AgentInput` 调 `drain`(parent=None)跑完一个 turn,返回 `TurnDone`(通知 + 终态)。
2. 复用 default 测试里可迁移的 fake(FakeClient/FakeToolRegistry/FakeToolIds/RequireApprovalPolicy)
   与 builder(assistant_response/tool_use_response/...)到参考 driver 测试;逐一对照
   text-only / single tool / parallel tool / tool failure self-heal / approval approve / approval deny
   的 Conversation 终态与通知序列。
3. 保留 `DefaultAgentLoop` 与其原测试不动(并存)。

## 设计
- `src/agent/drive.rs` 顶部加 `mod reference; pub use reference::{...}`(file-module 子模块解析到
  `src/agent/drive/reference.rs`)。
- `reference.rs`(生产代码):
  - `LlmClientHandler { client: Arc<dyn LlmClient> }` impl `LlmHandler`(chat / chat_stream+collect,
    mode 由 requirement 传入)。
  - `ToolRegistryHandler { registry: Arc<dyn ToolRegistry> }` impl `ToolHandler`。
  - `ApprovalInteractionHandler { decision, message }` impl `InteractionHandler`:对 approval 交互用固定
    `ApprovalDecision` 应答(attended UI / unattended 默认处置)。approve()/deny()/new()。
  - `ReferenceScope { llm, tool, interaction: Option<ApprovalInteractionHandler> }` impl `HandlerScope`。
    `new(client, registry)` + `with_interaction(handler)`。
  - `drive_turn(machine, input, scope, ctx) -> Result<TurnDone, AgentError>` = `drain(.., None, ..)`。
- `agent/mod.rs` 扩 `pub use drive::{...}` 增 `ApprovalInteractionHandler, LlmClientHandler,
  ReferenceScope, ToolRegistryHandler, drive_turn`。
- 测试 `src/agent/drive/reference/tests.rs`(`#[cfg(test)] mod tests;`):复制最小 fake +
  ScriptedRequirementIds/ScriptedToolIds(seed 与 legacy 对齐),`DefaultAgentMachine` 驱动,断言
  committed Conversation 与通知序列与 legacy 一致。deny turn 用按 call_id 脚本化交互 handler
  复现 deny/timeout/cancel。

## 验证命令(顺序)
1. `cargo fmt --all`
2. `cargo clippy --all-targets -- -D warnings`
3. `cargo test --lib agent::drive`(聚焦)
4. `cargo test --all --all-targets`(≤30min)
5. `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`
6. `git diff --check`

## 进度
- [x] reference.rs 生产代码 + 文档(编译通过,rustdoc clean)
- [x] drive.rs 声明子模块 + re-export
- [x] mod.rs re-export
- [x] reference/tests.rs 等价测试(6 类 turn,全部通过)
- [x] 全套验证(fmt/clippy/lib/all-targets/doc/diff --check 全绿,433 passed)
- [x] TODO.md 标 [DONE] + 完成记录,提交

## 结果
M3-3 完成。`src/agent/drive/reference.rs` 提供单层参考 driver(`LlmClientHandler` /
`ToolRegistryHandler` / `ApprovalInteractionHandler` / `ReferenceScope` / `drive_turn`),
6 个等价性测试对照 `DefaultAgentLoop` 的 text/single-tool/parallel/failure-self-heal/
approval-approve/approval-deny 用例,Conversation 终态与通知序列一致。全套验证全绿
(425 lib + 8 integration = 433 passed)。下一个未完成任务 = M3-R(本次不启动)。
