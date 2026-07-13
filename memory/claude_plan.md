# 执行计划

> 说明：本文件记录本轮工作的可审计计划、关键决策和进度更新；不记录逐字内部推理。

## 初始计划

1. 读取 `TODO.md`，严格按标题是否带 `[DONE]` 判断第一个未完成任务。
2. 检查最近提交信息，仅在其明确提到且直接影响当前任务的未完成事项时，将其纳入当前任务或作为前置项记录到 `TODO.md`。
3. 阅读当前任务关联的代码、测试、文档和阶段计划上下文；避免无关历史问题扫查。
4. 在不拆分任务的前提下实现第一个未完成任务；只有遇到真正阻塞当前任务的缺失前置项时，才向 `TODO.md` 添加最小前置任务并停止。
5. 按要求先运行 `cargo fmt --all`，再运行 `cargo clippy --all-targets -- -D warnings`，最后在需要时运行 `cargo test --all --all-targets`，完整测试超时不超过 30 分钟。
6. 若发现未明确排期的失败测试，修复它或把最小修复任务加入 `TODO.md`，且不把当前任务标为完成。
7. 完成任务后，更新 `TODO.md` 标题为 `[DONE]` 并填写完成记录；仅在阶段计划真实变化时更新 `PLAN.md`。
8. 提交本轮所有相关变更，提交信息包含任务编号和清晰说明，然后停止，不继续下一个任务。

## 当前状态

- 已创建本轮执行计划。
- 已读取 `TODO.md`，首个未完成任务为 `M1-3 AgentState、唯一活动 Conversation 与 LoopCursor`。
- 已检查最近提交：`f29e08d [M1-2] Add run context handles`，没有明确留下与 `M1-3` 直接相关的未完成事项。
- 已确认当前工作区除本计划文件外无未提交代码变更。
- 已阅读 `docs/agent-layer.md`、`PLAN.md`、现有 `agent::{id,spec,context}` 和 `Conversation::snapshot`/`Conversation::restore`。

## M1-3 具体执行计划

1. 新增 `src/agent/state.rs`，定义字段私有的 `AgentState`，内部持有完整 `AgentSpec`、唯一 live `Conversation`、active skill ids、pivot/reconfig 队列和 `LoopCursor`。
2. 为 `AgentState` 实现自定义 serde：序列化时调用 `Conversation::snapshot`；反序列化时必须调用 `Conversation::restore`，并对 active skills、队列和 cursor 做数据校验。
3. 定义 `LoopCursor` 的 data-only 状态和受检构造/状态转换，覆盖 idle、streaming step、awaiting tool、awaiting approval、cancel recovery、done、error。
4. 定义 data-only 的 `QueuedPivot` 与 `QueuedReconfig`；pivot 只接受 `Role::User` 消息，reconfig 只作为 turn-boundary 意图记录，不包含 runtime registry。
5. 定义单独的泛型 `AgentRuntimeHandles` holder，用于承载 client/tool registry/MCP/approval/task 等 live handle；不为其实现 serde，也不放入 `AgentState`。
6. 从 `agent::mod` 导出新增类型并更新模块文档；按需更新 crate 文档/README 中的当前能力说明。
7. 添加聚焦测试覆盖 state serde round-trip、Conversation restore 门、唯一 conversation 保留、runtime handle 排除、非法 cursor transition、pivot role 校验、active skill 去重。
8. 运行 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、聚焦 state/cursor 测试、全量测试、rustdoc 与 `git diff --check`。

## 进度更新

- 已新增 `src/agent/state.rs`，包含 `AgentState`、`LoopCursor`、queued pivot/reconfig、runtime handle holder 和聚焦测试。
- 已从 `agent::mod` 导出新增类型，并更新 crate 根文档与 `README.md` 的当前 Agent 能力说明。
- 已运行并通过：`cargo fmt --all`；`cargo clippy --all-targets -- -D warnings`；`cargo test agent::state`；`perl -e 'alarm 1800; exec @ARGV' cargo test --all --all-targets`；`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`；`git diff --check`。
- 复查发现初版 `state.rs` 过长；已拆分为 `state.rs` 聚合模块和 `state/cursor.rs`、`state/queue.rs`、`state/runtime.rs`、`state/tests.rs`。
- 已将 `TODO.md` 中 `M1-3` 标记为 `[DONE]` 并补充完成记录。
