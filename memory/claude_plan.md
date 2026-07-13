# 执行计划

## 约束

- 输出和过程记录使用中文。
- `TODO.md` 是任务顺序和完成状态的权威来源。
- 本轮只完成第一个标题未带 `[DONE]` 的任务，完成后提交并停止。
- 不做开放式历史问题扫描；只处理当前任务及其直接阻塞项。
- 不公开内部推理链；本文件记录可审计的执行计划、依据、进度和变更。

## 初始计划

1. 读取 `TODO.md`，按标题是否带 `[DONE]` 判断第一个未完成任务。
2. 查看与该任务直接相关的 `PLAN.md`、源码和测试，确认范围、依赖和验收要求。
3. 检查当前工作区状态，避免覆盖用户已有改动。
4. 实现该任务；如果发现当前任务被具体前置缺陷阻塞，则把最小前置任务插入 `TODO.md`，提交并停止。
5. 按要求先运行 `cargo fmt --all`，再运行 `cargo clippy --all-targets -- -D warnings`，最后在需要时运行 `cargo test --all --all-targets`，完整测试超时不超过 30 分钟。
6. 若测试发现未被后续任务明确覆盖的失败，修复失败或把最小修复任务排到当前任务完成前。
7. 更新 `TODO.md`：任务完成时在标题前加 `[DONE]`，并填写完成记录；只有阶段级计划变化时才更新 `PLAN.md`。
8. 提交本轮所有相关改动，提交信息包含任务编号和清晰说明。
9. 停止，不继续下一个任务。

## 进度

- 已创建本计划文件。
- 已读取 `TODO.md`，确认第一个未完成任务是 `M3-2 Pivot queue 与 interject 软转向`。

## 当前任务计划：M3-2 Pivot queue 与 `interject` 软转向

1. 检查最近提交信息与当前工作区状态，确认是否存在与 M3-2 直接相关的未完成前置问题或用户改动。
2. 阅读 `docs/agent-layer.md` 中 pivot/interject/step-boundary 相关段落，以及 `src/agent`、`src/conversation` 中 M2 loop、M3-1 injection、state queue/event 定义。
3. 设计并实现 Agent runtime 的 pivot queue 与 `AgentLoop::interject` 边界，保证入队是 thread-safe/async-safe，且只接受合法 `PivotMessage`。
4. 在默认 loop 的合法 step boundary 求值点应用 pivot：无 pending turn 时转为下一 turn 初始 user input；tool result 后合法 boundary 时调用 Conversation 的 user injection API 注入同一 pending turn。
5. 增加事件或 step-boundary metadata 记录 pivot accepted/applied/rejected 结果，按 `TODO.md` 要求覆盖延迟生效、顺序、text turn 下一轮、tool-result 同 turn 注入、queue cancel/drop 和非法消息。
6. 按要求运行格式化、严格 clippy、聚焦测试、全量测试、rustdoc 和 diff check。
7. 成功后更新 `TODO.md` 的 M3-2 标题为 `[DONE]` 并填写完成记录，提交本轮改动后停止。

## 当前实现选择

- `interject` 继续只接收并校验 `PivotMessage`，在 `DefaultAgentLoop` 的共享 `AgentState`
  mutex 内按 FIFO 入队，不直接打断 active LLM stream。
- tool-result 全部回灌之后、下一次 assistant request 之前作为 turn 内 step boundary；此处会
  drain queued pivots 并调用 `Conversation::inject_user_message` 注入同一 pending turn。
- final assistant commit 后没有 pending turn；此处只在 `StepBoundary` metadata 记录 queued pivots
  已推迟到下一 turn，队列不被移除。
- 新增显式 queued-pivot feed 输入用于下一 turn：调用方仍提供 `TurnId`、assistant message id
  和 `StepId`，loop 从队列 FIFO 取出 pivot 的 `MessageId`/payload 作为该 turn 的初始 user。
- 若 pivot 已成功注入 pending，随后该 pending 因错误被 discard，则该 pivot 随 pending 一起丢弃；
  未到达边界或只是 final boundary deferred 的 pivot 保留在队列中。

## 已完成步骤

- 已扩展 `AgentInput`，新增 queued-pivot turn 输入，让调用方显式提供下一 turn 所需的
  `TurnId`、assistant message id 与 `StepId`。
- 已在 `DefaultAgentLoop` 中实现 FIFO pivot 出队、tool-result step boundary 注入、final
  boundary deferred metadata，以及非法 queued pivot 入队错误。
- 已新增/调整聚焦测试，覆盖 streaming text 延迟生效、queued pivot 下一 turn、tool-result
  同 turn FIFO 注入、rejected pivot 记录和 invalid role 拒绝。
- 已通过：`cargo fmt --all`；`cargo test agent::loop_driver::default --all-targets`；
  `cargo test agent::event --all-targets`。
- 已通过严格 lint：`cargo clippy --all-targets -- -D warnings`。
- 已通过全量验证：`cargo test agent::loop_driver::default --all-targets`；
  `cargo test agent::event --all-targets`；
  `perl -e 'alarm 1800; exec @ARGV' cargo test --all --all-targets`；
  `cargo test --doc`；`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`；`git diff --check`。
- 已将 `TODO.md` 中 M3-2 标题改为 `[DONE]` 并补充完成记录；`PLAN.md` 无阶段级变化，未更新。
