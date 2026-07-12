# 当前任务执行计划

## 目标与边界

- 以 `TODO.md` 为唯一的任务顺序与完成状态来源。
- 只处理第一个标题未带 `[DONE]` 的任务；完成并提交后立即停止，不进入下一项。
- `PLAN.md` 仅在阶段级顺序、依赖、假设或完成标准发生变化时更新。
- 不进行开放式历史缺陷扫描；只处理当前任务的明确要求、直接阻塞项、当前任务引入的回归，以及验证中发现且未被后续任务明确排期的测试失败。

## 执行步骤

1. 读取 `TODO.md`，定位第一个未完成任务，并完整提取其需求、依赖、约束、验收命令和完成记录要求。
2. 检查最新提交说明；仅当它明确提到与当前任务直接相关的未完成问题时，将其纳入当前任务，或按规则在 `TODO.md` 中增加最小前置任务后提交并停止。
3. 检查工作区状态与当前任务相关文件，保护用户已有改动；若这是意外中断后的同任务续作，则在最终提交中纳入全部现有未提交文件。
4. 阅读当前任务直接涉及的设计文档、实现和测试，建立需求到代码与验证项的对应关系；不做无关的广泛排查。
5. 按小而聚焦的补丁完整实现任务；每完成关键步骤或计划发生变化时更新本文件。
6. 补充或调整测试，覆盖任务要求的正常路径、边界条件、错误分类和不变量；若同一根因影响一类场景，则修复整类问题而非单点绕过。
7. 按规定顺序验证：先运行 `cargo fmt --all`，再运行 `cargo clippy --all-targets -- -D warnings`，然后在不超过 30 分钟的超时下运行 `cargo test --all --all-targets`；根据任务要求运行额外测试与 `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`。任何未明确排期的失败都必须修复，或新增最小前置/后续任务并保持当前任务未完成。
8. 更新文档：在 `TODO.md` 的当前任务标题前加 `[DONE]`，填写可复核的完成记录（实现内容、测试结果、关键文件）；仅在阶段计划实际变化时修改 `PLAN.md`。
9. 复查差异、工作区状态和任务边界，确认没有秘密、临时文件、规避方案、警告或未调度失败。
10. 用包含任务编号的清晰消息提交本次全部相关改动；确认提交成功与工作区状态后停止，不处理下一任务。

## 可能的停止条件

- 若当前任务被一个未跟踪且必须先独立落地的具体前置问题阻塞：在 `TODO.md` 中把最小前置任务放到正确位置，补充显式依赖，必要时更新阶段计划，提交这些记录后停止。
- 若所有任务都已 `[DONE]`：按项目要求执行最终审查、必要调整和完整验证，提交后创建 `endtag`。

## 当前状态

- 已读取 `TODO.md` 并按标题状态确定首个未完成任务为 **M2-2 `PendingMessage` 与 stream/non-stream 冻结边界**；本轮不得进入 M2-3。
- 当前任务必须复用 Client `Accumulator`，让 stream/non-stream 共用受检冻结语义；成功冻结时才绑定外部 `MessageId`，所有 partial、terminal error、重复 finish 与 cancel/drop 路径都不得产生 closed message。
- 最新提交为已完成的 M2-1，未声明 M2-2 遗留问题；本轮开始前工作区干净，不是意外中断续作。
- 实现设计：`PendingMessage` 不实现 serde/Clone，内部状态分别持有唯一 `Accumulator` 或完整 `Response`；成功冻结后进入 frozen，失败后进入 terminal。`FrozenMessage` 只读承载 `ConversationMessage`、usage、stop reason 与 response extra，并提供消费式拆分。
- stream 与 non-stream 最终都经过同一个 response→frozen 转换，统一拒绝非 assistant response；工具输入保持完整 `serde_json::Value`，stream partial JSON 只由现有 `Accumulator` 在完整边界解析。
- `PendingMessageError` 将嵌入统一 `ConversationError`，并以原始 `AccumulatorError` 为 source；为保持既有 `ConversationError: Clone + Eq` API，原始错误放入共享只读 `Arc`，相等性以稳定的分类化错误表示比较。
- `cancel(self)` 仅消费/丢弃 pending，不调用隐式 finish；第二次 finish、成功后 push、terminal 后 push/finish 都返回分类生命周期错误。
- 已完成 pending 模块、错误链、实现、拆分聚焦测试及 README/crate docs 更新；`PLAN.md` 的阶段级计划没有变化。
- 首次 `cargo fmt --all` 通过；首次严格 clippy 仅报告两个构造函数与类型级 `#[must_use]` 重复，已删除方法级冗余标注。下一步重新执行 format → clippy，随后运行聚焦测试、全量测试、doc test/rustdoc 与 diff check。
- 第二轮 format 与严格 clippy 通过。首次 pending 聚焦测试为 8 passed / 1 failed：实现错误变体使用 `transparent` 导致标准 `Error::source()` 跳过 `PendingMessageError`，直接透传更深层 source。已改为显式 source 包装，以稳定保留 `ConversationError → PendingMessageError → AccumulatorError → ClientError` 四层链；将重新执行 format → clippy → 聚焦测试。
- 第二次聚焦测试仍为 8 passed / 1 failed，并进一步定位到 `thiserror` 对 `Arc<AccumulatorError>` source 的自动暴露类型是 `Arc` 包装，而非可直接 downcast 的内部分类。保持严格测试不变，现已为 `PendingMessageError` 手写 `Display`/`Error::source`，明确返回共享错误的内部引用，同时保留既有统一错误的 Clone/Eq 能力。下一步再次执行完整的 format → clippy → 聚焦测试链。
- 第三轮 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings` 与 pending 聚焦测试全部通过（9 passed）。
- 30 分钟硬上限下的 `cargo test --all --all-targets` 通过：183 个库测试、3 个离线集成测试及全部示例 test target 通过，7 个真实 endpoint 测试按既有 `#[ignore]` 跳过，0 failed。
- 下一步运行 doc tests（包含新增 non-serde/no-partial compile-fail）、严格 rustdoc 和 diff/worktree 审查；全部通过后更新 `TODO.md` 完成记录，再进行最终复核与提交。
- `cargo test --doc` 通过（1 个正向、8 个 compile-fail，其中 2 个为 M2-2 新增）；`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` 通过；首次提交前 `git diff --check` 通过。
- 已逐项复核实现/测试与 M2-2 要求，没有发现阻塞项、spec deviation 或需要修改 `PLAN.md` 的阶段变化；已将 `TODO.md` 的 M2-2 标为 `[DONE]` 并写入完成记录。
- 最终 `cargo fmt --all -- --check` 与 `git diff --check` 均通过；暂存清单只有 M2-2 的 11 个实现、测试、任务记录、进度与公共文档文件，cached diff check 通过。
- 最后步骤：创建 `[M2-2]` 原子提交，确认提交与工作区状态后停止，不进入 M2-3。
