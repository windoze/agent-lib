# 当前任务：M6-2 迁移 reference driver 测试中的重复 fake

## 定位
- `TODO.md` 第一个未完成任务 = **M6-2**（line 1173，标题 `[TODO]`）。M6-1 已 `[DONE]`（HEAD=32267d1）。
- 无关未跟踪文件 `docs/external-agent.md`（external-agent 设计草案，非本任务产物，不纳入提交）。

## 关键阻塞（已实证）
`src/agent/drive/reference/tests.rs` 是 **单元测试模块**（`#[cfg(test)] mod tests;`）。
探针实测：单元测试构建下 `agent-lib`（test-cfg 实例）与 `agent-testkit` 所链接的 `agent-lib`
（plain 实例）是**两个不同 crate 实例**（"multiple different versions of crate agent_lib"），
testkit 产出的类型（machine/spec/ctx/scope）无法喂给 `crate::` 的 `drive_turn` 等。dev-dep 依赖环
对单元测试**不可用**，无解。→ 迁移必须把这些测试**迁到集成测试层**（`tests/`，单一 plain 实例，
类型统一，与 M6-1 相同模式）。这是 testkit 的既定消费面，不是 workaround。

## 方案
1. 新建 `tests/reference_driver.rs`（集成测试），`use agent_testkit::prelude::*;`，迁移全部 11 个测试。
2. 替换映射：
   - `FakeClient` → 本地最小 `ScriptedLlmClient`（`LlmClient` adapter，脚本+call log 来自 testkit
     `Script<LlmStep>` + `LlmCallLog`）。`ReferenceScope` 需 `LlmClient`，testkit 无 `LlmClient`，故保留。
   - `FakeToolRegistry` → `ScriptedToolRegistry`（testkit `ToolRegistry`），call 计数读 `log()`。
   - `ScriptedRequirementIds` + `FakeToolIds` → `SeqIds`；payload helpers → fixtures + `LlmStep`/`ToolStep`/`tool_call`。
   - `CancellingLlmHandler` → `CancelOnCall::before(ScriptedLlmHandler)`；`PanicToolHandler` → `PanicOnCall`；
     `CancelScope` → `TestScope::builder().llm(..).tool(..)`。
   - `ScriptedApprovalInteraction` + `ComposedScope`（deny）→ `TestScope::wrapping(ReferenceScope).attended(ScriptedInteractionHandler::sequence([Deny,Timeout,Cancel]))`。
   - approve 测试保留 `ApprovalInteractionHandler::approve()`（reference 自有组件，非 fake）。
   - 保留最小本地 `RequireApprovalPolicy`（spec 级策略，非 effect fake；同 e2e）。
   - `assert_text` / `assert_tool_result` 小工具保留。
   - 精确 seed id 断言 → 结构断言（pairings 链接 result_msg==messages[i].id()、registry log 名称/计数、
     notification 序列、status 序列）。coverage 不减。
3. testkit 增强（迁移暴露的 gap）：`InteractionDecision` 增 `Timeout(Option<String>)` / `Cancel(Option<String>)`
   + testkit 单测。deny 测试需覆盖全部四种 `ApprovalDecision`。
4. 删除 `src/agent/drive/reference/tests.rs` 与 `reference.rs` 的 `#[cfg(test)] mod tests;`。
5. TODO.md：M6-2 标 `[DONE]`，修正验证命令（`--lib ...reference::tests` → `--test reference_driver`），
   完成记录写明阻塞、迁移理由、保留件。

## 校验顺序
fmt --check → clippy --all-targets -D warnings → cargo test --test reference_driver → testkit 聚焦 →
全套 cargo test --all --all-targets（≤30min）→ rustdoc → git diff --check → commit（M6-2）。停止。

## 进度
- [x] 读 reference tests + testkit 各模块 + 实证阻塞
- [ ] 扩展 testkit InteractionDecision（Timeout/Cancel）+ 单测
- [ ] 写 tests/reference_driver.rs
- [ ] 删单元测试模块 + reference.rs mod 声明
- [ ] fmt/clippy/聚焦/全套/rustdoc
- [ ] TODO.md 标 [DONE] + 完成记录
- [ ] 提交（M6-2）。停止。

## 备注
- deny 序列映射依赖 fulfill_batch 的 FuturesUnordered push-order；现有 parallel/tool_failure 测试已依赖同一
  决定性，一致，运行验证。
- 若迁移暴露真实 bug/失败测试，按策略插前置任务并停。

## Progress — M6-2 COMPLETE (2026-07-14)

- [x] Extended testkit `InteractionDecision` (Timeout/Cancel) + testkit unit test — passing.
- [x] Wrote `tests/reference_driver.rs` (11 migrated tests, 826 lines) — all 11 pass.
- [x] Deleted `src/agent/drive/reference/tests.rs` (git rm) + removed `#[cfg(test)] mod tests;`.
- [x] Validation: fmt --check clean; clippy --all-targets -D warnings clean; `cargo test --test reference_driver` 11/11; full `cargo test --all --all-targets` 0 failed; rustdoc -D warnings ok; `git diff --check` clean.
- [x] TODO.md M6-2 marked [DONE], validation cmd fixed to `--test reference_driver`, completion record written.
- [x] Commit (excluding docs/external-agent.md). STOP after commit.
