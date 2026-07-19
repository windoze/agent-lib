# 执行计划

## 当前约束

- 以 `TODO.md` 为唯一任务顺序和完成状态来源。
- 只完成第一个标题未带 `[DONE]` 的任务，然后停止。
- 若遇到阻塞当前任务的既有缺陷、规格不匹配或测试失败，优先修复；若无法直接修复，则在 `TODO.md` 中插入最小必要前置任务并提交后停止。
- 完成任务后必须更新 `TODO.md`，在任务标题前加 `[DONE]`，填写完成记录，并提交 Git commit。
- 验证顺序为 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets`，若涉及外部适配器功能再运行相应 feature 的 clippy。

## 步骤

1. 读取 `TODO.md`，按文件顺序找出第一个标题未带 `[DONE]` 的任务。
2. 查看该任务的范围、依赖、验证要求和完成记录；必要时查看最近提交，确认是否有与该任务直接相关的未完成事项。
3. 检查相关源码、测试和文档，确定最小正确实现范围。
4. 实现该任务；若发现阻塞当前任务的真实前置问题，更新 `TODO.md` 并停止在前置任务提交处。
5. 运行格式化、lint 和相关测试；如发现未排期失败，修复或把最小前置修复任务排入 `TODO.md`。
6. 更新 `TODO.md` 的任务标题与完成记录；仅在阶段级计划发生变化时更新 `PLAN.md`。
7. 检查 git diff/status，提交本次任务涉及的所有未提交变更。
8. 停止，不继续处理下一个任务。

## 进度记录

- 已写入初始计划；下一步读取 `TODO.md` 识别第一个未完成任务。
- 已读取 `TODO.md`，第一个标题未带 `[DONE]` 的任务为 `M5-3 结构化错误 kind 替代字符串匹配分类（M-ERR-5）`。
- 本次只处理 M5-3：把 facade 对 loop step limit 的分类从错误消息子串匹配改为结构化 kind，并补相应测试、文档、TODO 完成记录与提交。
- 下一步检查最近提交和当前工作树，确认没有直接影响 M5-3 的未完成事项或冲突变更。
- 最近提交为 M5-2，不含与 M5-3 直接相关的未完成事项；当前工作树只有本次变更。
- 已实现主方案：`ErrorCursor` 新增 `ErrorCursorKind`，旧 wire 缺字段默认 `Other`；facade `classify_error` 改按 kind 匹配，message 仅用于普通 `AgentError::Other` 展示；已补 state/facade 单元测试和文档标注。
- 下一步先运行格式化和目标测试，修复发现的问题后再更新 `TODO.md` 完成记录并跑全量门禁。
- 验证已完成并通过：`cargo fmt --all`、定向 `facade::agent` 与 `agent::state` 测试、默认 clippy、默认全量测试、external feature clippy、rustdoc。
- `TODO.md` 已将 M5-3 标记为 `[DONE]` 并写入完成记录；下一步复查 diff/status 后提交。
