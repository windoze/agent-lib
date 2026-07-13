# 执行计划

## 当前约束

- 输出、记录和后续进展说明使用中文。
- `TODO.md` 是任务顺序、任务要求、验证要求和完成记录的唯一权威来源。
- 本轮只完成第一个标题未带 `[DONE]` 的任务，然后停止。
- 不做开放式历史问题扫描；只处理当前任务所需范围内的问题。
- 若遇到阻塞当前任务的既有缺陷、规格不匹配或测试失败，先修复；若无法在本轮直接修复，则在 `TODO.md` 中插入最小必要前置任务并提交后停止。
- 不接受绕过式实现；若实现无法匹配规格，必须把缺口转化为明确任务。
- 代码改动后按要求先格式化，再严格 lint，再运行相关/完整测试；完整 Rust 测试不超过 30 分钟。
- 完成任务后必须在 `TODO.md` 的任务标题前加 `[DONE]`，更新完成记录，并提交 Git commit。

## 执行步骤

1. 读取 `TODO.md`，按文件顺序找出第一个标题没有 `[DONE]` 前缀的任务。
2. 检查最新 commit 信息是否明确提到与该任务直接相关的未完成问题；只在直接相关时纳入当前任务或作为前置任务写入 `TODO.md`。
3. 读取当前任务引用的设计、计划、测试和代码上下文；必要时读取 `PLAN.md`，但不把它作为日常任务日志。
4. 明确当前任务的实现边界、依赖、验证要求和可能影响的模块，并更新本文件记录已确定的任务目标。
5. 在编辑前说明即将修改的文件和原因；使用小而集中的补丁逐步实现。
6. 为新增或变更行为补充聚焦测试；若发现同类根因影响多个场景，一并修复该类问题。
7. 按顺序运行 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`，再根据任务要求运行相关测试或完整 `cargo test --all --all-targets`。
8. 若测试失败且失败未被后续任务明确排期，立即修复；若必须新增前置任务，则更新 `TODO.md` 后提交并停止。
9. 验证通过后，在 `TODO.md` 中将当前任务标题标记为 `[DONE]`，更新完成记录；仅在阶段级计划变化时更新 `PLAN.md`。
10. 查看 Git 状态，确认包含本轮必要文件，提交一个清晰描述当前任务的 commit。
11. 停止，不继续处理下一个任务。

## 进展记录

- 初始计划已写入。下一步读取 `TODO.md` 以确定本轮第一个未完成任务。
- 已读取 `TODO.md`：Milestone 1 与 `M2-1` 均已标记 `[DONE]`，本轮首个未完成任务是
  `M2-2 [TODO] 基础 LLM step 驱动与 Conversation pending 集成`。
- 已查看最新提交：`42fa9f5 [M2-1] Add agent loop event stream contract`，提交信息未声明与
  `M2-2` 直接相关的未完成阻塞项。
- 下一步读取 `PLAN.md`、`docs/agent-layer.md` 中与 M2 loop 相关的内容，以及现有
  `src/agent`、`src/conversation`、`src/client` API，确定默认 loop driver 应复用的公共边界。

## 本轮任务目标：M2-2

- 实现默认 loop driver 的 text-only 非流式与流式基础路径。
- 由 `AgentInput` 构造 user `Message`，调用 `Conversation::begin_turn`。
- 将非流式 `Response` 交给 `Conversation::start_assistant_response`；将流式
  `StreamEvent` 交给 Conversation pending/accumulator 路径。
- 当 `finish_assistant` 返回 `AssistantFinish::ReadyToCommit` 时提交 pending turn，重新取得合法
  `Boundary`，并发出 `AgentEvent::StepBoundary` 与最终 `Done`。
- Client 或 Conversation 错误必须转换为分类 `AgentError`，且不能留下 partial committed state。
- 增加 fake `LlmClient` 聚焦测试，覆盖 text-only 非流式、流式、事件顺序、committed turns、
  usage、boundary version 和失败原子性。

## 已确认的实现决策

- Client 请求上下文使用 `Conversation::effective_view()` 的 committed projection，再显式追加
  `Conversation::pending_context()` 中已冻结的 pending user payload；不直接读取或复制 raw history。
- 默认 driver 需要在 `finish_assistant` 时绑定 assistant `MessageId`。当前 `AgentInput::UserMessage`
  没有该 id，若由 driver 生成或复用其他 id 会违反“id 由调用方注入”的约束。因此本轮会扩展
  `AgentUserInput`，要求调用方显式提供 `assistant_message_id`。
- 本轮只处理 text-only 的单次 LLM step：若 assistant response 包含 tool use，则返回分类错误并丢弃
  本次 pending，M2-3 再实现 tool mapping/execution/result 回灌。

## 当前进展

- 已扩展 `AgentUserInput`，新增调用方显式提供的 `assistant_message_id`，并同步 serde restore 与测试。
- 已新增 `DefaultAgentLoop` 与 `LlmStepMode`，支持 text-only 非流式和流式基础 LLM step。
- 默认 driver 会从 `effective_view()` 加 `pending_context()` 构造 `ChatRequest`，使用 spec model/tool
  配置，优先采用 Conversation system，缺省时回退到 AgentSpec system。
- 成功路径会调用 Conversation pending/freeze/commit API，提交后重新取得 `head()` 作为
  `StepBoundary` 的合法 `Boundary`，并发出 `Done(Completed)`。
- 失败路径会丢弃本次 pending 并把 cursor 回到 `Idle`，避免留下半提交状态。
- 已新增 fake `LlmClient` 聚焦测试，覆盖非流式成功、流式成功、client 失败原子性和无效 assistant
  响应失败原子性。
- 已通过：`cargo fmt --all`；`cargo clippy --all-targets -- -D warnings`；
  `cargo test agent::loop_driver`。
- 完整验证已通过：`perl -e 'alarm 1800; exec @ARGV' cargo test --all --all-targets`；
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`；`git diff --check`；`cargo test --doc`。
- 已更新 `TODO.md`：`M2-2` 已标记 `[DONE]` 并写入完成记录。
- 已再次通过 `git diff --check`。
- 下一步提交本轮 M2-2 相关变更，然后停止。
