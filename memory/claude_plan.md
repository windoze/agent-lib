# 当前执行计划

## 约束

- 以 `TODO.md` 为任务顺序和完成状态的唯一权威来源。
- 只完成第一个未标记 `[DONE]` 的任务，完成后提交并停止。
- 不记录隐藏推理过程；本文件记录可审阅的执行计划、关键进度和结果。
- 如遇阻塞，优先在 `TODO.md` 中加入最小必要前置任务并提交，不绕过规格要求。

## 初始步骤

1. 读取 `TODO.md`，定位第一个标题未以 `[DONE]` 标记的任务。
2. 查看最近提交是否明确提到与该任务直接相关的未完成问题。
3. 读取该任务相关源码、测试和文档，确认范围与验收要求。

## 执行步骤

1. 按任务要求做最小且完整的实现或修复。
2. 增加或更新覆盖当前任务行为的测试。
3. 运行格式化、lint 和相关测试；必要时运行完整验证。
4. 将任务标题标记为 `[DONE]`，并更新 `TODO.md` 的完成记录。
5. 若阶段计划没有变化，不更新 `PLAN.md`。
6. 检查工作区差异，提交所有与本任务相关的变更。

## 当前状态

- 已读取 `TODO.md`，第一个未完成任务为 `M9-5 [TODO] 终审 review：全计划收口`。
- 已读取 `PLAN.md`、`docs/review-2026-07.md` 与最近提交。最近提交 `[M9-4] Synchronize documentation review closeout` 未提到与 M9-5 直接相关的未完成事项。
- `docs/review-2026-07.md` 的审查条目均已有最终状态标注；`PLAN.md` 的五个目标可由 M1-M9 完成记录逐项对应。
- M9-5 要求的门禁已通过：`cargo fmt --all`、默认 clippy、external features clippy、默认全量测试、external features 全目标测试、rustdoc。
- `PLAN.md` 已写入最终收口结论；M9-5 已标记 `[DONE]` 并补完成记录；根 `PLAN.md` / `TODO.md` 已归档到 `docs/archive/2026-07-19-review-fixes/`。
- 非归档文档中指向根计划/任务单的 markdown 链接已改到归档位置；`docs/review-2026-07.md` 顶部状态已更新为 M9-5。
- 下一步：检查 git 差异，提交本任务变更并创建 `endtag`。

## M9-5 执行步骤

1. 核对最近提交是否显式留下与 M9-5 直接相关的未完成事项。
2. 核对 `docs/review-2026-07.md` 是否仍有未标注条目。
3. 核对 `PLAN.md` 中五个目标的达成情况，并在收尾记录中写明结论。
4. 按任务要求运行全量门禁：`cargo fmt --all`、默认 clippy、external features clippy、`cargo test --all --all-targets`、rustdoc。
5. 将 M9-5 标记为 `[DONE]`，补完成记录。
6. 按 M9-5 收尾要求归档 `PLAN.md` 和 `TODO.md` 到 `docs/archive/2026-07-19-review-fixes/`。
7. 检查 git 差异，提交本任务所有变更；若全部任务完成，创建 `endtag`。
