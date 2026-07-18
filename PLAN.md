# 实施计划：Refine 修正

本计划以 [docs/refine.md](docs/refine.md) 为唯一输入，目标是把当前 facade 与 managed external agent 实现中已经识别出的差距收口到可验证、可维护的状态。

旧版计划和任务单已归档到：

- [docs/archive/2026-07-18-facade-api/PLAN.md](docs/archive/2026-07-18-facade-api/PLAN.md)
- [docs/archive/2026-07-18-facade-api/TODO.md](docs/archive/2026-07-18-facade-api/TODO.md)

## 目标

1. 流式运行在被提前丢弃时必须能恢复到一致状态，不留下无法继续使用的 pending turn。
2. 非流式 `Agent::run_full` 的 `RunOutput.events` 必须覆盖审批请求等关键运行事件，与流式路径保持语义一致。
3. `AgentSnapshot` 必须完整保存和恢复协作底座中的 mailbox、blackboard、plan，以及明确 artifact 的保存策略。
4. managed external agent 的 quick start 必须可直接运行，默认 session handler 的装配方式要清晰。
5. external capability 必须区分声明值、用户提供值、探测值和协商值，避免把静态声明误认为运行时验证结果。
6. `Agent::into_parts` 必须成为完整的逃生出口，不能静默丢掉 external delegate、保留会话、协作状态或交互处理器。
7. 文档、测试和实现必须同步，默认测试不依赖真实 CLI 或真实 provider。

## 非目标

1. 不重写 `Conversation`、`AgentMachine` 或 managed external runtime 的核心架构。
2. 不引入新的默认依赖或默认启用任何 external CLI feature。
3. 不把 ignored real e2e 测试改成默认必跑测试。
4. 不改变 secret 处理策略，不在日志、snapshot 或 fixture 中保存凭据。
5. 不顺手做无关重构，除非它是完成本计划的必要前置。

## 里程碑

### M1：流式生命周期恢复

修正 `ChatSession::stream` 和 `Agent::stream` 在提前 drop 时的状态恢复问题。完成后，任何未自然结束的 stream 都不能让会话或 agent 留在不可继续运行的 pending 状态。

重点文件：

- `src/facade/chat.rs`
- `src/facade/chat/stream.rs`
- `src/facade/agent.rs`
- `src/facade/agent/stream.rs`
- 相关 facade 单元测试和 testkit fake client

设计要求：

- `RunStream` drop 必须回滚 chat pending turn。
- `AgentRunStream` drop 或 close 路径必须清理未完成 run 的 pending requirement。
- 已完成的 stream 不能被重复回滚。
- 回滚后下一次 `send`、`run`、`snapshot` 必须可用。

### M2：非流式事件一致性

修正 `Agent::run_full` 对审批事件的遗漏。完成后，非流式和流式路径对于 approval、tool、delegation 这类结构化事件要有同等可观察性。

重点文件：

- `src/facade/agent.rs`
- `src/facade/agent/stream.rs`
- `src/facade/run.rs`
- `src/facade/tool.rs`
- 相关 facade tests

设计要求：

- `run_full` 的 `RunOutput.events` 包含 `ApprovalRequested`。
- 注入的 `InteractionHandler` 仍然按原有优先级工作。
- 审批被拒绝或 headless fallback 时也要记录审批请求事件。
- 文档明确非流式不会产出 token 级 text delta，但会产出关键生命周期事件。

### M3：协作状态 snapshot 和 restore

修正 `AgentSnapshot` 当前只记录协作拓扑、不记录协作内容的问题。完成后，恢复后的 agent 必须保留 mailbox、blackboard、plan 中的可序列化状态。

重点文件：

- `src/agent/collab/mailbox.rs`
- `src/agent/collab/blackboard.rs`
- `src/agent/collab/plan.rs`
- `src/facade/agent/snapshot.rs`
- `src/facade/collab.rs`
- 相关 snapshot tests

设计要求：

- 新增或完善 mailbox、blackboard、plan 的 data-only snapshot API。
- `AgentSnapshot::capture` 必须从 live `CollabState` 捕获内容。
- restore 优先使用 snapshot 中保存的协作内容，缺失时才按 topology 建立空底座。
- artifact 的顶层 snapshot 策略必须明确：要么保存聚合视图，要么文档说明只由 external session snapshot 持有。

### M4：managed external 可用性和 capability 来源

修正 README quick start 中构造 external agent 后没有 session handler 的问题，并让 capability 的来源可见。

重点文件：

- `src/facade/external.rs`
- `src/facade/agent.rs`
- `README.md`
- `docs/managed-external-agent.md`
- `docs/capability-matrix.md`
- external facade tests

设计要求：

- 提供清晰的默认 session handler 装配 API，README 示例能按文档直接运行。
- 默认 feature 下仍不拉入 CLI adapter。
- 启用 external features 时，probe 得到的 capability 必须能标记为 probed。
- capability 来源至少能区分 declared、supplied、probed、negotiated。
- `UnsupportedCapability` 判断必须基于当前 agent 真正持有的 capability view。

### M5：完整逃生出口

修正 `Agent::into_parts` 和 `AgentParts` 丢失状态的问题。完成后，高级调用方能安全拆解 `Agent` 并重新接管重要资源。

重点文件：

- `src/facade/agent.rs`
- `src/facade/agent/snapshot.rs`
- `src/facade/external.rs`
- 相关 facade tests

设计要求：

- `AgentParts` 覆盖 LLM、conversation、tools、instructions、policies、delegate config、external agents、保留 external sessions、协作底座和交互处理器。
- 不泄漏内部不应公开的可变实现细节；如果某些状态不能直接公开，必须提供等价的 data-only 或 handle 形式。
- rustdoc 明确 `into_parts` 的适用边界。

### M6：最终收口

完成文档同步、全量验证和残留风险复核。

重点文件：

- [docs/refine.md](docs/refine.md)
- [README.md](README.md)
- [docs/facade-api.md](docs/facade-api.md)
- [docs/managed-external-agent.md](docs/managed-external-agent.md)
- [docs/capability-matrix.md](docs/capability-matrix.md)
- [PLAN.md](PLAN.md)
- [TODO.md](TODO.md)

设计要求：

- `docs/refine.md` 中的问题状态与实现保持一致。
- quick start、capability、snapshot、stream lifecycle 的文档不互相矛盾。
- 所有默认验证命令通过。
- 启用 external feature 的 clippy 通过。

## 验证策略

每个任务在本地完成后至少运行对应的 focused test。每个里程碑 review 任务必须运行该阶段声明的验证命令。最终验收必须运行：

```bash
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test --all --all-targets
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
cargo clippy --all-targets \
  --features "external-claude-code external-codex external-opencode external-acp" -- -D warnings
```

默认测试不得要求真实 LLM provider、真实 CLI login 或网络。真实 provider 和真实 CLI 相关测试继续保持 `#[ignore]`，并且在未配置环境时必须干净跳过。

## 风险和处理原则

1. **Drop 中不能 await**
   stream drop 修复要优先使用同步状态回滚。如果需要异步收尾，公开 close API 只能作为补充，不能替代 drop 后 agent/chat 可继续使用的基本保证。

2. **snapshot 兼容性**
   新增 snapshot 字段要使用 `#[serde(default)]` 或等价兼容策略，旧 snapshot 必须仍可反序列化并恢复为空协作底座。

3. **capability API 兼容性**
   capability 来源模型要尽量保持现有构造和 accessors 可用。新增 source 信息时，避免让常见调用方必须重写代码。

4. **公开类型边界**
   `AgentParts` 可以扩展，但不能把内部锁、registry 或 runtime 细节变成无法演进的公开承诺。必要时使用封装类型或 data-only view。

5. **测试稳定性**
   对 stream cancellation、approval、snapshot 的测试必须使用 fake client、scripted handler 或 testkit，不依赖 sleep 的时序运气。
