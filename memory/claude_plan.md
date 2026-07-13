# 执行计划

本文件记录本次调用的可审计执行计划、关键进度和计划变更。内容聚焦任务依据、操作步骤、验证方式和结果记录，不包含私有推理链。

## 初始计划

1. 先读取 `TODO.md`，按标题是否带 `[DONE]` 判断第一个未完成任务；同时查看最新提交信息，仅在其明确提到与该任务直接相关的未完成事项时纳入当前任务或新增前置任务。
2. 阅读当前任务在 `TODO.md` 中的完整要求、依赖、验收标准和完成记录；必要时查阅 `PLAN.md` 以理解阶段边界，但不把 `PLAN.md` 作为日常任务日志。
3. 检查工作区状态，识别已有未提交改动；不回滚用户改动，只在当前任务需要时与之协同。
4. 针对第一个未完成任务阅读相关源码、测试和文档，确定最小但完整的实现范围。
5. 实现任务；如发现阻塞当前任务的规格不匹配、缺陷或未排期失败测试，则优先修复，或在 `TODO.md` 中新增最小前置任务并停止。
6. 按要求先运行 `cargo fmt --all`，再运行 `cargo clippy --all-targets -- -D warnings`，最后在需要时运行 `cargo test --all --all-targets`（完整测试超时不超过 30 分钟）。若本次仅文档变更且上次完整测试仍可复用，则在完成记录中说明跳过原因。
7. 更新 `TODO.md`：只有任务完整实现并验证后，才在任务标题前加 `[DONE]` 并填写完成记录。仅当阶段计划发生真实变化时才更新 `PLAN.md`。
8. 提交本次变更，提交信息包含任务编号和清晰说明。
9. 完成第一个未完成任务后停止，不继续处理后续任务。

## 进度记录

- 已完成：读取 `TODO.md` 标题列表，确认首个未完成任务是 `M3-4 Approval 挂起、responder 与 cancel 贯穿闭合`。
- 已检查：最新提交为 `[M3-3] Implement turn-boundary reconfig queue`，与当前任务的直接前置任务一致；未发现需要在当前任务前额外插入的最新提交遗留事项。
- 已注意：工作区已有未提交项 `docs/agent-effect-model.md`，当前先视为既有用户/外部改动；后续只在确认其与 M3-4 直接相关或必须纳入任务完成记录时处理，不回滚。

## 当前任务计划：M3-4

1. 阅读 `docs/agent-layer.md` 中 tool approval/cancel 相关规范，以及 `src/agent` 中 loop、event、state、context、tool 的现有边界。
2. 识别现有 `AwaitingApproval` 事件、`ApprovalRequest`、`LoopCursor::AwaitingApproval`、`RunContext` cancellation、Conversation cancel API 的可复用点。
3. 已完成：新增 `approval` 模块，定义 approval policy、requirement、response/decision 和分类 error；事件 payload 继续保持 data-only，live responder 通过 `AgentLoop::respond_approval` 提交。
4. 已完成：approval 决策已接入默认 loop；approve 后执行 tool，deny/timeout/cancel 生成 provider-neutral `ToolStatus::Denied`/`Cancelled` result 并继续模型恢复。
5. 已完成：`RunContext` cancellation token 已接入 LLM stream 与 tool future；active partial 走 discard closure，open tool call 走 `CancelDisposition::ResumeTurn` 合成 cancelled results，并可用新的 runtime context 继续 feed。
6. 已完成：awaiting approval 使用 data-only `LoopCursor::AwaitingApproval`，live responder 留在 runtime waiter map，不进入 serde。
7. 已完成：新增聚焦测试覆盖 approve、deny、timeout、approval cancel、stream 挂起不结束、active partial cancel、open tool cancel 后 resume feed，以及父 cancel 传播到 child context/tool future。
8. 已完成：最终验证链通过，`TODO.md` 已将 `M3-4` 标记为 `[DONE]` 并补充完成记录；下一步提交本次 M3-4 相关改动。

## 计划调整

- 为了满足“AwaitingApproval 挂起 stream、不结束 feed”，非流式 driver 也需要在 `feed()` 返回后由事件流推进实际 LLM/tool 工作；否则 `feed()` 会在等待 approval 前无法把 responder 暴露给调用方。接下来会把非流式路径改成懒执行 event stream，并调整相关错误测试从 `feed()` 错误改为 stream item 错误。

## 验证记录

- 通过：`cargo fmt --all`
- 通过：`cargo clippy --all-targets -- -D warnings`
- 通过：`cargo test agent:: --all-targets`
- 通过：`perl -e 'alarm 1800; exec @ARGV' cargo test --all --all-targets`
- 通过：`cargo test --doc`
- 通过：`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`
- 通过：`git diff --check`
