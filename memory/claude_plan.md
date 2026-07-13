# 执行计划

## 当前约束

- 输出和进度记录使用中文。
- `TODO.md` 是任务顺序和完成条件的唯一权威来源。
- 本轮只完成第一个标题未带 `[DONE]` 的任务，完成后提交 Git 并停止。
- 如遇阻塞，不绕过规格；在 `TODO.md` 插入最小必要前置任务并提交后停止。
- 格式化、clippy、完整测试按要求顺序执行；若只改文档且无代码变化，可复用上一轮绿色结果并记录原因。

## 步骤计划

1. 读取 `TODO.md`，确定第一个未完成任务及其验收要求。
2. 检查当前 Git 状态和最近提交，只关注与当前任务直接相关的未完成事项或阻塞。
3. 阅读当前任务涉及的设计文档、源码和测试，确认既有模式与边界。
4. 以最小、完整、符合现有架构的方式实现当前任务；若发现必须先修复的具体前置问题，更新 `TODO.md` 并停止。
5. 添加或调整聚焦测试，覆盖当前任务要求和相关失败路径。
6. 运行 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets`，并修复所有未被明确排期的失败。
7. 将当前任务标题标记为 `[DONE]`，更新其完成记录；仅在阶段级计划变化时修改 `PLAN.md`。
8. 查看 Git diff，确认没有误改或泄漏；提交本轮全部相关变更。

## 进度

- 已创建本执行计划。
- 已读取 `TODO.md`，本轮第一个未完成任务为 `M3-R Milestone 3 Review`。
- 本轮目标是完成 M3 的人工审查与验证，更新 `TODO.md` 完成记录并提交；不开始 M4。
- 已检查工作树：除本计划文件外存在未跟踪 `docs/agent-effect-model.md`，内容为未采纳设计草稿；本轮不纳入 M3-R 修改。
- 已人工读取 M3 关键实现与文档，初步确认 pivot/reconfig/approval/cancel 路径均通过受检边界接入。
- 已运行 `cargo fmt --all`，通过。
- 已运行 `cargo clippy --all-targets -- -D warnings`，通过。
- 已运行 M3 聚焦测试：`cargo test agent:: --all-targets`、`cargo test conversation::pending::turn --all-targets`、`cargo test conversation::validation --all-targets`，均通过。
- 已运行 `perl -e 'alarm 1800; exec @ARGV' cargo test --all --all-targets`，通过。
- 已运行 `cargo test --doc` 与 `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`，均通过。
- 已将 `TODO.md` 中 `M3-R Milestone 3 Review` 标记为 `[DONE]` 并补充完成记录；未更新 `PLAN.md`，因为无阶段级计划变化。
- 已运行 `git diff --check`，通过。

## 当前任务执行清单

1. 检查 Git 状态和最近提交，确认是否存在与 M3-R 直接相关的未提交/未完成事项。
2. 阅读 M3 相关实现和文档：pivot 注入、reconfig、approval、cancel、Conversation 注入入口、runtime state 边界。
3. 对照 `TODO.md` 的 M3-R 要求进行人工映射，确认无 unchecked pending/raw history 入口，无 turn 内 reconfig 生效，无不可恢复 approval/cancel 漏洞。
4. 运行 M3 聚焦测试与全量验证：`cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、M3/agent/conversation 聚焦测试、`cargo test --all --all-targets`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`、`git diff --check`。
5. 若发现未排期失败或阻塞，修复或在 `TODO.md` 插入最小前置任务并停止。
6. 若审查和验证通过，将 `M3-R` 标题标记为 `[DONE]`，补充完成记录，提交变更后停止。
