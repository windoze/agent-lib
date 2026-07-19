# 执行计划

1. 读取 `TODO.md`，按标题是否带 `[DONE]` 判断第一个未完成任务。
2. 检查最近提交是否明确提到与该任务直接相关的未完成问题；只处理会阻塞当前任务的问题。
3. 阅读当前任务涉及的设计文档、代码和测试，确认验收要求与依赖。
4. 以最小正确改动实现当前任务；如果发现必须新增前置任务才能正确完成，则更新 `TODO.md` 后提交并停止。
5. 按要求运行格式化、lint 和相关测试；若发现未被明确排期的失败测试，修复或把最小前置修复任务加入 `TODO.md`。
6. 更新 `TODO.md`：给当前任务标题加 `[DONE]`，填写完成记录；仅在阶段计划变化时更新 `PLAN.md`。
7. 复查 `git status`、`git diff` 和最近提交，提交本次任务的所有相关改动，然后停止。

## 进度

- 已建立初始执行计划，下一步读取 `TODO.md` 选择第一个未完成任务。
- 已确认第一个未完成任务为 `M5-2`：审批行为与文档对齐 + 流式 `Done.events` 审批事件补齐。

## 当前任务计划：M5-2

1. 检查最近提交是否提到与 M5-2 直接相关的未完成事项，并查看当前工作区状态。
2. 阅读 facade 审批、非流式 run、流式 run、run event/输出文档与现有 parity 测试，确认当前事件织入路径。
3. 采用任务推荐方案：typed tool deny 维持合成 `Denied` tool result 回灌模型，文档明确 `FacadeError::ApprovalDenied` 仅用于 external delegate 审批路径。
4. 修改流式完成路径，使 `Done.events` 与非流式一样调用 `weave_approval_events`。
5. 扩展或新增测试，覆盖带审批的非流式输出与流式 `Done.events` 完全一致且含 `ApprovalRequested`。
6. 同步 `docs/facade-api.md` 与相关 rustdoc，更新 `docs/review-2026-07.md` 对应条目标注。
7. 运行 `cargo fmt --all`、相关 facade 测试、clippy、必要全量测试与 rustdoc；若发现未排期失败按政策处理。
8. 更新 `TODO.md` 标记 `M5-2 [DONE]` 并填写完成记录，提交本次改动后停止。

## 当前进度更新

- 已实现流式审批 recorder：`TapInteractionHandler` 实时 emit 审批事件的同时记录同一请求，终态 `Done.events` 复用 `weave_approval_events` 与非流式路径对齐。
- 已扩展 stream/run_full parity 测试，断言审批场景下 `Done.events` 与非流式 `RunOutput.events` 完全一致且包含 `ApprovalRequested`。
- 已更新 typed tool deny / `ApprovalDenied` 的 rustdoc、`docs/facade-api.md` 与 `docs/review-2026-07.md` 标注。
- 下一步运行格式化、相关测试与 lint/doc 验证。
- 验证已通过：`cargo fmt --all`、`cargo test -p agent-lib --lib facade::`、`cargo clippy --all-targets -- -D warnings`、`cargo clippy --all-targets --features "external-claude-code external-codex external-opencode external-acp" -- -D warnings`、`cargo test --all --all-targets`、`cargo test --features "external-claude-code external-codex external-opencode external-acp" --all-targets`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`。
- 已更新 `TODO.md` 的 M5-2 标题和完成记录，`docs/review-2026-07.md` 对应条目已标注完成。
- 已复查 `git status`、`git diff`、`git diff --check` 和最近提交；下一步提交本次任务改动后停止。
