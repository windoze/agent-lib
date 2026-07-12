# 当前任务执行计划

## 执行约束

- 以 `TODO.md` 为任务顺序、依赖、验收要求和完成状态的唯一事实来源。
- 本次只处理第一个标题未带 `[DONE]` 的任务；完成并提交后立即停止。
- 不进行开放式历史缺陷排查；只处理会阻塞当前任务、使当前任务行为失效，或由当前任务直接引入的问题。
- 不记录模型的隐藏逐步思考过程；本文件记录可审计的计划、关键判断、执行进展和验证证据。

## 分步计划

1. 首先读取 `TODO.md`，定位第一个未完成任务，并完整提取其需求、依赖、验收标准和完成记录要求。
2. 检查工作区状态及最新提交；只判断未提交改动或最新提交说明是否与当前任务直接相关，避免覆盖用户已有改动。
3. 阅读当前任务直接涉及的设计文档、源码和测试，确认现有实现边界；如发现具体阻塞前置条件，按要求最小化更新 `TODO.md`、提交并停止。
4. 若无阻塞，按任务原定范围完整实现；使用多个小而聚焦的补丁，并在关键步骤后复查相关文件。
5. 添加或更新覆盖正常路径、边界条件和错误路径的测试；不通过缩小表示范围、私有特例或其他变通方式规避规范要求。
6. 按规定顺序验证：`cargo fmt --all`，然后 `cargo clippy --all-targets -- -D warnings`，最后在不超过 30 分钟的限制内运行 `cargo test --all --all-targets`；再执行当前任务列出的其他验证命令。
7. 所有验收通过后，在 `TODO.md` 的任务标题前添加 `[DONE]` 并填写准确的完成记录；仅当阶段级计划确实变化时才修改 `PLAN.md`。
8. 复查 diff 和 Git 状态，确认没有遗漏与当前任务相关的恢复现场文件；创建清晰的单次任务提交。
9. 记录最终提交和验证结果，然后停止，不开始下一任务。

## 当前进展

- 已建立执行计划。
- 已首先读取 `TODO.md`，确认首个未完成任务是 `M2-2 [TODO] StreamEvent`。
- 当前任务要求：在 `stream/mod.rs` 定义 `MessageStart`、`BlockStart`、`BlockDelta`、`BlockStop`、`ToolInputAvailable`、`Usage`、`MessageStop`、`Error` 八类统一流事件；为各变体记录与 Vercel v5 part 的对应关系；补齐 serde round-trip 并保证编译通过。
- 依赖状态：`M2-1` 已完成，所需 `BlockId`、`BlockKind`、`Delta` 已存在；真实 `ClientError` 明确安排在 `M3-1`，因此本任务按规范使用可序列化的字符串占位，不提前实施后续任务。
- Git 检查结果：除本文件外工作区干净；最新提交 `2f2125e [M2-1] Implement streaming block delta types` 没有声明与当前任务直接相关的未完问题。
- 设计复核结果：`PLAN.md` 与参考文档确认 StreamEvent 只含 LLM wire 真实事件，不加入 approval/abort/pivot；块事件继续使用稳定 `BlockId` 和统一三段式。
- 实现选择：沿用现有流类型的 `snake_case` serde 枚举表示；`Error(String)` 作为任务明确允许的占位，并在文档中说明 M3-1 将替换成 `ClientError`。
- 已在 `src/stream/mod.rs` 实现八类 `StreamEvent`，复用 `Role`、`BlockId`、`BlockKind`、`Delta`、`Usage`、`Normalized<StopReason>` 与 `serde_json::Value`。
- 已为每个事件变体写明 Vercel v5 part 的追溯关系，并明确 Client 层不包含 Agent 层 approval/abort/pivot。
- 已添加覆盖全部变体的 serde round-trip 测试和 `snake_case` 稳定表示断言。
- 已按顺序完成验证：`cargo fmt --all` 通过；`cargo clippy --all-targets -- -D warnings` 通过且无 warning；`cargo test --all --all-targets` 通过（37 passed，0 failed）；`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` 通过。
- 未观察到测试失败、规范偏差或需要新增前置任务的阻塞问题。
- 已将 `TODO.md` 中 `M2-2` 标记为 `[DONE]` 并写入实现与验证记录；阶段级计划和依赖未变化，因此未修改 `PLAN.md`。
- 提交前审计确认差异仅有 `src/stream/mod.rs`、`TODO.md` 和本进度文件；`git diff --check` 通过，没有触碰下一任务 `M2-3` 的实现，也没有遗漏其他未提交文件。
- 下一步复核格式状态并创建单一提交 `[M2-2] Implement normalized stream events`，随后核验提交与干净工作树并停止。
