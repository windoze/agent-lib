# 执行计划

## 说明

我不会记录完整的隐藏推理链路；这里记录可审计的任务理解、决策依据、执行步骤和进度。

## 初始约束

- 以 `TODO.md` 为任务顺序和完成状态的唯一权威来源。
- 首先定位第一个标题未带 `[DONE]` 的任务；只完成这一项，然后停止。
- 在确认当前任务前不做开放式历史问题扫描。
- 如遇阻塞当前任务的缺陷、测试失败或规格不匹配，先修复；若不能直接修复，则在 `TODO.md` 中插入最小必要前置任务并提交后停止。
- 完成后必须更新 `TODO.md`，将任务标题显式加上 `[DONE]` 并填写完成记录。
- 按要求先运行 `cargo fmt`，再运行 `cargo clippy --all-targets -- -D warnings`，再运行完整测试；若仅文档变化且已有可复用的绿色结果，可按规则说明跳过。
- 完成当前任务后提交 Git commit，不继续下一个任务。

## 步骤

1. 读取 `TODO.md`，识别第一个未完成任务及其验收要求。
2. 检查最新提交信息是否明确提到与该任务直接相关的未完成问题。
3. 阅读当前任务涉及的代码、测试和文档上下文。
4. 根据任务要求做最小但完整的实现或审查修复。
5. 补充或调整相关测试，避免绕过规格。
6. 运行格式化、clippy 和测试验证。
7. 更新 `TODO.md` 的任务标题与完成记录；仅在阶段计划变化时更新 `PLAN.md`。
8. 查看 Git diff，确认只包含当前任务相关变更。
9. 提交所有当前任务相关未提交文件。
10. 停止并汇报本次完成内容、验证结果和 commit。

## 当前状态

- 状态：已定位首个未完成任务：`M2-R Milestone 2 Review`。

## 当前任务计划：M2-R

1. 阅读 `docs/agent-layer.md`、`PLAN.md` 中与 M2 Agent loop、事件流、tool 回灌、pending/commit
   边界相关的要求。
2. 阅读 `src/agent/event.rs`、`src/agent/loop_driver.rs`、`src/agent/tool.rs` 及其测试，确认
   `AgentLoop::feed` stream 契约、重入保护、事件顺序、错误分类和 tool result 回灌是否符合 M2。
3. 人工映射 text-only、streaming、single/parallel tool、tool error、client/conversation failure
   路径，确认不会留下半提交 state，且 pairing 校验仍由 Conversation 承担。
4. 明确 M3 可挂接的 step boundary、approval waiting 和 cancel hook 点，并记录在 `TODO.md`
   的 M2-R 完成记录中。
5. 按 M2-R 验证要求运行 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、
   M2 聚焦测试、完整测试、rustdoc 和 `git diff --check`。
6. 如未发现必须先修复的问题，将 `M2-R` 标题标记为 `[DONE]` 并补充完成记录。
7. 检查 diff 并提交本任务变更，然后停止。

## 进度记录

- 已确认首个未完成任务为 `M2-R Milestone 2 Review`，最新提交 `[M2-3] Add agent tool execution loop`
  未明确提到与 M2-R 直接相关的未完成问题。
- 已读取 M2 相关设计要求和实现文件：`docs/agent-layer.md`、`PLAN.md`、`src/agent/event.rs`、
  `src/agent/loop_driver.rs`、`src/agent/loop_driver/default.rs`、`src/agent/tool.rs` 及默认 loop
  聚焦测试索引。
- 初步人工审查结论：`feed` stream guard、text/stream 基础路径、tool result 回灌、Conversation
  pending/commit 顺序和失败清理均与 M2 边界一致；暂未发现需要插入前置任务的阻塞问题。
- 已通过：`cargo fmt --all`。
- 已通过：`cargo clippy --all-targets -- -D warnings`。
- 已通过：`cargo test agent::event`。
- 已通过：`cargo test agent::loop_driver --all-targets`。
- 已通过：`perl -e 'alarm 1800; exec @ARGV' cargo test --all --all-targets`。
- 已通过：`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`。
- 已通过：`git diff --check`。
- 已将 `TODO.md` 中 `M2-R Milestone 2 Review` 标记为 `[DONE]`，并补充 review 结论、M3 hook 点和验证记录。
- 下一步：最终 diff/status 检查并提交本任务。
