# 当前执行计划

## 约束与决策依据

- 本次只完成 `TODO.md` 中按文档顺序出现的第一个标题未带 `[DONE]` 的任务，然后停止。
- `TODO.md` 是任务内容、依赖、验收条件和完成记录的唯一事实来源；只有阶段级计划发生变化时才更新 `PLAN.md`。
- 在选定当前任务前不做开放式历史问题排查；只检查最新提交是否明确提到与当前任务直接相关的未完成问题。
- 若发现直接阻塞当前任务的规格缺口、回归或未排期测试失败，优先完整修复；确实无法在当前任务内完成时，才在 `TODO.md` 中插入最少数量的前置任务，保持当前任务未完成，提交任务编排变更后停止。
- 不采用缩小测试形态、替换既定表示、专用特判、兼容垫片或其他规避规格的办法。
- 不覆盖或回退用户已有改动；若这是上次中断后恢复同一任务，最终提交将包含当前所有未提交文件。
- 所有代码编辑使用小而聚焦的补丁。验证顺序为：格式化、严格 lint、相关测试、完整测试套件；单项测试不得运行超过一分钟，完整 Rust 测试套件超时不超过三十分钟。

## 分步执行计划

1. 读取 `TODO.md`，严格按标题状态定位第一个未完成任务，并阅读该任务的完整要求、依赖与验收条件。
2. 查看最新提交摘要及工作区状态，仅判断是否存在与该任务直接相关的未完成事项，以及是否为中断恢复场景；同时阅读任务明确引用的 `PLAN.md`、规范、代码和测试。
3. 把选定任务、范围、验收条件、相关文件和基线状态补充到本文件。
4. 实现任务要求；每完成关键步骤或因证据需要调整计划时，立即更新本文件。
5. 先运行针对性验证并修复问题；随后按要求运行 `cargo fmt`、`cargo clippy --all-targets -- -D warnings`，最后运行不超过三十分钟的完整测试套件。若项目不是 Rust 项目，则采用仓库定义的等价格式化、lint 和完整测试命令。
6. 检查差异是否完整、无警告、无遗漏规格；将任务标题加上 `[DONE]` 并填写准确的完成记录和验证结果。只有阶段级依赖或完成标准变化时才修改 `PLAN.md`。
7. 更新本文件为完成状态，创建一个清晰描述当前任务的 Git 提交，确认提交成功及工作区状态，然后停止，不开始下一个任务。

## 当前状态

- 状态：已读取 `TODO.md`，首个未完成任务为 `M1-5 [TODO] Message 与 Tool(schema)/ToolCall/ToolResponse`。
- 当前任务范围：
  - 在 `model/message.rs` 实现不含 `MessageId` 的 `Message { role, content: Vec<ContentBlock> }`。
  - 在 `model/tool.rs` 实现 `Tool`、`ToolCall`、`ToolResponse` 与 `ToolStatus`；`ToolResponse.content` 必须支持多模态块，状态覆盖成功、错误、拒绝和取消。
  - 补齐模块公开导出、公共文档、serde round-trip 测试，以及含工具调用/响应的消息序列结构测试。
- 验收门槛：任务要求的结构和 serde 测试通过；格式化、严格 clippy 与完整测试套件全绿；`TODO.md` 标题显式改为 `[DONE]` 并记录验证命令；创建且只创建当前任务的提交。
- 基线检查：最新提交 `054a67a [M1-4] Implement content blocks` 未声明与 M1-5 直接相关的未完成问题；工作区起始时仅本文件因本次执行被修改，不属于中断恢复场景。
- 实现决策：
  - `Message` 仅含公开的 `role: Role` 与 `content: Vec<ContentBlock>`，并用文档明确说明身份由 Conversation 层包装，避免误把 provider/client 数据模型与持久化身份混合。
  - `Tool` 的 `name`、`description` 为字符串，`input_schema` 为 `serde_json::Value`；不提前引入 `schemars`。
  - `ToolCall` 使用 provider/client 可传递的字符串 `id`、工具名和完整 JSON 输入。
  - `ToolResponse` 使用 `tool_call_id`、多模态 `Vec<ContentBlock>` 与 `ToolStatus`；状态按 snake_case wire name 序列化，以稳定表示 `ok/error/denied/cancelled`。
  - 为四个数据结构以及全部状态变体增加 serde 覆盖，并用 assistant tool-use message + tool result message 验证统一消息序列及调用/响应字段关联。
- 已完成实现：
  - `src/model/message.rs` 已加入无 ID 的完整态 `Message` 与 Conversation 层身份边界文档。
  - `src/model/tool.rs` 已加入 `Tool`、`ToolCall`、`ToolResponse`、`ToolStatus`，工具响应内容使用 `Vec<ContentBlock>`。
  - 单元测试已覆盖工具 schema、调用、含多模态内容且分别处于四种状态的响应、稳定状态 wire name，以及带工具调用与工具结果的消息序列 round-trip/关联关系。
- 下一步：运行 `cargo fmt --all`，随后执行 M1-5 针对性测试并检查结果；若通过，再运行严格 clippy 和完整测试套件。
- 验证进展：`cargo fmt --all` 已通过。首次运行消息模块针对性测试时，测试 crate 编译在通用 round-trip helper 处失败：`serde_json::from_str` 无法从后续比较推断泛型 `T`。该问题属于新测试辅助代码的明确类型标注缺失，将把 `decoded` 显式标为 `T`，重新格式化并重跑验证。
- 修复与复验：已为 round-trip helper 增加 `decoded: T` 类型标注；再次运行 `cargo fmt --all` 通过，`cargo test model::message::tests`（2 项）与 `cargo test model::tool::tests`（4 项）均通过。
- 下一步：运行 `cargo clippy --all-targets -- -D warnings`；通过后运行完整 `cargo test --all --all-targets`，再审查差异并更新完成记录。
- 全量验证：`cargo clippy --all-targets -- -D warnings` 已通过且无 warning；带 1800 秒超时保护的 `cargo test --all --all-targets` 已通过（26 passed, 0 failed, 0 ignored）。
- 差异审查：`git diff --check` 已通过；代码仅涉及 M1-5 的消息/工具模型与测试，另有任务完成记录和本进度文件。未发现规格偏差、未排期失败或阶段级计划变化，因此不修改 `PLAN.md`。
- 任务记录：`TODO.md` 中 M1-5 标题已显式改为 `[DONE]`，完成记录包含实现内容及全部验证命令。最后一次完整测试后只有 Markdown 记录变更，按任务规则无需重跑编译测试。
- 当前状态：M1-5 已实现、验证、记录并提交；最终任务提交包含 `TODO.md`、本进度文件及全部代码/测试变更。
- 停止点：仅核验最终提交与工作区清洁状态，然后结束本次调用；不处理首个后续任务 M1-6。
