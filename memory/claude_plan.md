# 当前任务：M6-1 迁移 `tests/agent_effect_e2e.rs` 到 testkit

## 定位
- `TODO.md` 第一个未完成任务 = **M6-1**（line 1131，标题 `[TODO]`）。
- 前置 M5-R `[DONE]`（HEAD=ea4f8fd）。
- 工作区：无关未跟踪文件 `docs/external-agent.md`（非本任务产物，不纳入本次提交）。

## 任务要求（TODO.md M6-1）
迁移 `tests/agent_effect_e2e.rs`（1032 行、大量本地 fake）到 agent-testkit，保留四语义、减少样板：
- 用 `SeqIds` 替换本地 `SeqIds`。
- 用 fixtures 替换本地 message/response/tool helpers。
- 用 scripted handlers / `ScriptedToolRegistry` 替换本地 `FakeClient`、`FakeToolRegistry`。
- 用 subagent helpers 替换本地 `ChildSpawner` / child scope boilerplate。
- 保四测试语义：pop 服务 headless child（budget 聚合=18）、attended 就地解决（4 消息）、并发 batch（peak==2）、cancel 传播 abandon（无 IO）。

## 迁移映射
- 本地 SeqIds → `agent_testkit::SeqIds`。
- 本地 fixtures → testkit fixtures + script LlmStep/ToolStep。
- FakeClient → `ScriptedLlmHandler`；charge 由薄 `ChargingLlmHandler` 包 `Arc<dyn LlmHandler>` 保留（host 责任）。
- FakeToolRegistry → `ScriptedToolRegistry` 经 reference `ToolRegistryHandler`。
- ChildSpawner → `ScriptedSubagentSpawner` + `SpawnedChildBuilder`。
- Parent/Child/Empty/Observing Scope → parent_scope_with_subagent/headless_child_scope/attended_child_scope/TestScope。
- ParentBatchMachine → `ScriptMachine`（requirements([NeedTool,NeedSubagent]).done_after_all_resumed）。
- Counting* → ScriptedInteractionHandler::approve_all()/ScriptedToolHandler（log().len()）。
- ConcurrentToolHandler → DelayingToolHandler::with_delay(inner, Delay::yields(2))，peak 经 peak_concurrency()。
- 保留最小本地 RequireApprovalPolicy（spec 细节非 fake）。

## 验证
- fmt --check → clippy --all-targets -D warnings → cargo test --test agent_effect_e2e → 全套 cargo test --all --all-targets → rustdoc → git diff --check。

## 步骤
1. [x] 读 e2e + testkit 各模块。
2. [x] 重写 tests/agent_effect_e2e.rs。
3. [x] fmt + clippy + 聚焦 e2e + 全套 + rustdoc。
4. [x] TODO.md M6-1 标 [DONE] + 完成记录。
5. [x] 提交（M6-1）。停止。

## 备注
- 若迁移暴露真实 bug/失败测试，按策略插前置任务并停。
