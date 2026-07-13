# 执行计划

## 当前约束

- 以 `TODO.md` 为唯一任务顺序来源，先识别第一个标题未带 `[DONE]` 的任务。
- 本次只完成第一个未完成任务，完成后更新 `TODO.md`、验证、提交 Git，然后停止。
- 若遇到阻塞当前任务的真实前置问题，不绕过；在 `TODO.md` 插入最小前置任务并提交后停止。
- `PLAN.md` 只在阶段级计划、依赖或完成标准变化时更新。
- 输出和进度记录使用中文。

## 步骤计划

1. 读取 `TODO.md`，定位第一个未完成任务，并记录任务编号、目标、依赖和验证要求。
2. 查看最近提交信息，判断是否明确提到与该任务直接相关的未完成问题。
3. 按任务要求读取必要代码、文档和测试，限定范围在当前任务及其直接依赖内。
4. 实现当前任务，优先沿用仓库既有模块边界、数据模型和测试风格。
5. 按要求更新或新增测试，先运行格式化，再运行 clippy，再运行相关测试和必要的完整测试。
6. 如发现未被排期的测试失败，修复它；若它是当前任务的必要前置且无法同次完成，则插入前置任务并停止。
7. 在 `TODO.md` 中把当前任务标题加上 `[DONE]`，填写完成记录、验证命令和结果。
8. 检查工作区差异，提交所有本次任务相关变更。
9. 发送最终摘要并停止，不继续下一个任务。

## 进度

- 已创建本计划文件。
- 已读取 `TODO.md`，第一个未完成任务是 `M1-1 Agent identity、AgentSpec 与静态配置模型`。
- 已查看最近提交 `21ca3ce [M7-1] Start agent layer planning`，未发现直接阻塞 M1-1 的未完成实现项。
- 已读取 `docs/agent-layer.md` §1.1、`PLAN.md` 的关键决策和现有 `conversation::id` 风格。

## M1-1 具体执行计划

1. 新增 `src/agent/mod.rs`、`src/agent/id.rs`、`src/agent/spec.rs`，并从 `src/lib.rs` 导出 `agent`。
2. 在 `agent::id` 中按现有 Conversation identity 风格定义 UUID newtype：`AgentId`、`RunId`、`StepId`、`ToolSetId`、`SkillId`、`PlanId`、`BlackboardId`。
3. 在 `agent::spec` 中定义字段私有的静态配置模型：`AgentSpec`、`WorktreeRef`、`ToolSetRef`、`ModelRef`、`LoopPolicy`，只保存可 serde 数据。
4. 为公开类型补 rustdoc，明确 `AgentSpec` 是模板/配方，不持有 `Conversation`、`LlmClient`、`ToolRegistry` 或 runtime handle。
5. 添加聚焦单测覆盖 id serde/parse round-trip、非法 UUID、newtype 不能误用的 compile_fail 文档测试、`AgentSpec` serde 保留外部值和 getter 只读访问。
6. 运行 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、聚焦 agent 测试、全量测试、rustdoc 和 `git diff --check`。
7. 更新 `TODO.md` 完成记录，提交本任务变更。

## 当前执行状态

- 已新增 `agent` 模块、UUID identity newtype、`AgentSpec`、`WorktreeRef`、`ToolSetRef`、`ModelRef`、`LoopPolicy` 与聚焦单测。
- 已更新 crate 根文档和 `README.md`，说明当前 Agent 模块只暴露 data-only 静态配置。
- 已运行 `cargo fmt --all`，格式化完成。
- 已运行并通过 `cargo clippy --all-targets -- -D warnings`。
- 已运行并通过 `cargo test agent::`。
- 已运行并通过 30 分钟上限包装的 `cargo test --all --all-targets`。
- 已运行并通过 `cargo test --doc`，覆盖 Agent identity 的 compile-fail newtype 误用示例。
- 已运行并通过 `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`。
- 已运行并通过 `git diff --check`。
- 已将 `TODO.md` 中 M1-1 标记为 `[DONE]` 并补充完成记录。
- 下一步复查 git 状态并提交本任务变更。
