# 执行计划

## 约束说明

- 本文件记录可审计的执行计划、决策依据、关键进展和验证结果。
- 不记录不可公开的隐藏推理过程；后续如计划变化或关键步骤完成，会及时更新本文件。
- 本轮只完成 `TODO.md` 中第一个标题未带 `[DONE]` 的任务，然后提交并停止。

## 初始步骤

1. 读取 `TODO.md`，按文件顺序定位第一个标题未带 `[DONE]` 的任务。
2. 查看最新提交信息，仅当最新提交明确提到与当前任务直接相关的未完成问题时，将其纳入当前任务或作为前置任务写回 `TODO.md`。
3. 阅读当前任务相关的 `PLAN.md`、代码、测试和文档上下文，确认实现边界、依赖、验收要求和是否存在阻塞项。
4. 如任务可直接完成，则按现有架构和项目风格实现；如发现必须先修复的具体前置问题，则在 `TODO.md` 中插入最小前置任务，提交后停止。
5. 运行要求的格式化、lint 和测试：优先 `cargo fmt --all`，再 `cargo clippy --all-targets -- -D warnings`，最后在需要时运行 `cargo test --all --all-targets`，完整测试超时不超过 30 分钟。
6. 若验证通过，更新 `TODO.md`：给当前任务标题加 `[DONE]`，填写完成记录；仅在阶段计划实际变化时更新 `PLAN.md`。
7. 检查 git diff，提交本轮全部相关变更，提交信息包含任务编号和简洁说明。
8. 停止，不继续处理下一个任务。

## 当前状态

- 状态：已读取 `TODO.md` 并定位首个未完成任务。
- 当前任务：`M3-1 Conversation step-boundary user 注入入口`。
- 任务目标：在 Conversation pending 层增加受 Boundary/phase 校验的 user 注入入口，允许 canonical Turn 在 tool result 闭合后的 step boundary 接收额外 `Role::User` 消息并继续 assistant，同时保持纯文本 turn 行为和既有 single-user turn 行为不被破坏。

## M3-1 执行步骤

1. 检查最新提交信息，确认是否有与 M3-1 直接相关的未完成问题需要纳入。结果：最新提交为 `[M2-R] Review agent loop step model`，未提到 M3-1 相关未完成缺陷。
2. 阅读 `PLAN.md`、`docs/agent-layer.md` 中 M3-1 相关章节，以及 Conversation pending/validator/metadata 的现有实现。结果：现有 `validate_boundary` 正确地拒绝 pending turn，不能为 M3-1 放宽；需要新增 step-boundary 专用校验。
3. 设计最小公开 API：调用方提供 `Boundary`、`MessageId`、完整 user `Message` 与注入来源 metadata；API 只在合法 pending step boundary 成功。设计：新增 `Conversation::inject_user_message`，内部使用 pending step-boundary 校验、`PendingTurn::inject_user_message` 和 envelope 级 `MessageMeta`。
4. 更新 PendingTurn 状态机和 canonical Turn validator，使 tool results 闭合后的同 turn user 注入合法，并拒绝 active partial、open tool call、非法 role、stale/cross-conversation boundary、重复 message id 等情况。
5. 保存注入来源 metadata，不新增 role，不暴露 raw history 或 unchecked pending mutation。
6. 增加聚焦测试覆盖任务要求的正反路径和失败原子性。结果：`cargo test conversation::pending::turn::tests::injection --all-targets` 已通过，覆盖 8 个新增用例。
7. 运行格式化、严格 clippy、聚焦测试、全量测试、rustdoc 和 diff check。结果：`cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo test conversation::pending::turn --all-targets`、`cargo test conversation::validation --all-targets`、`perl -e 'alarm 1800; exec @ARGV' cargo test --all --all-targets`、`cargo test --doc`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`、`git diff --check` 均已通过；新增第 8 个 injection 测试后已重跑 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo test conversation::pending::turn::tests::injection --all-targets`、完整 `cargo test --all --all-targets` 和 `git diff --check`。
8. 更新 `TODO.md` 完成记录并提交本轮变更。状态：下一步执行。

## M3-1 当前设计决策

- `ConversationMessage` 增加可选 envelope metadata：`MessageMeta { source, extra }`；默认构造保持旧 JSON 形状，注入入口使用 `ConversationMessage::new_with_meta` 保存来源。
- `Boundary::validate_boundary` 继续保持 committed-boundary 语义并拒绝 pending；新增 crate-private `Conversation::resolve_pending_step_boundary` 仅供 pending step 注入使用。
- pending step 注入只接受当前 head boundary，且必须有 active pending turn；Boundary owner、version、position、anchor、fork ceiling 仍逐项校验。
- `PendingTurn` 注入条件为 `AwaitingAssistant` 且当前 pending 位置位于已闭合 tool-result batch 之后；初始 user 后、active partial、awaiting mappings、awaiting results、ready-to-commit 都拒绝。
- closed-turn validator 允许 `assistant(tool_use) -> tool_result+ -> user+ -> assistant`，但仍拒绝初始 `user -> user` 和 final assistant 后的 user。
