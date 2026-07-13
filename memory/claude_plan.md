# 执行计划

## 可公开推理摘要

- 本次调用必须只完成 `TODO.md` 中第一个标题未以 `[DONE]` 标记的任务，然后停止。
- `TODO.md` 是任务顺序、依赖、完成标准和验收要求的唯一权威来源；`PLAN.md` 只在阶段级计划确实变化时更新。
- 若当前任务被具体前置问题阻塞，应在 `TODO.md` 中插入最小必要前置任务并提交后停止，不能用缩小范围或临时 workaround 继续。
- 任意观察到且未被明确排期的测试失败都必须修复，或在 `TODO.md` 中排入当前任务完成前的必要修复任务。
- 需要先格式化，再严格 lint，最后运行相关或完整测试；完整 Rust 测试最长不超过 30 分钟。
- 完成后必须把当前任务标题前缀改为 `[DONE]`，更新完成记录，并创建包含全部相关未提交变更的 Git commit。

## 初始执行步骤

1. 读取 `TODO.md`，只定位第一个未完成任务，不做开放式历史问题扫描。
2. 查看最新提交信息，确认是否明确提到与该任务直接相关的未完成问题。
3. 按任务正文读取必要代码、测试和文档，确定实现边界与验收命令。
4. 若任务可直接实现，按现有代码风格做最小但完整的设计与代码修改。
5. 若发现必须先修复的具体前置问题，更新 `TODO.md` 记录前置任务和依赖，提交后停止。
6. 对代码变更运行 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`，再运行任务要求的测试；如需完整套件，使用不超过 30 分钟 timeout。
7. 更新 `TODO.md`：将任务标题标记为 `[DONE]`，补充完成记录、验证命令和结果。
8. 仅当阶段级计划变化时更新 `PLAN.md`。
9. 检查 git diff，确认未误改无关内容；提交本次任务所有相关改动。
10. 停止，不继续下一个任务。

## 进度记录

- 已创建本计划文件。
- 已读取 `TODO.md` 并确认第一个未完成任务是 `M2-1 AgentInput、AgentEvent、AgentOutcome 与 stream 契约`。
- 已查看最新提交：`[M1-R] Review milestone 1 agent boundaries`，未发现直接声明 M2-1 阻塞项。
- 当前任务目标：
  - 新增 Agent loop 的输入、事件、结果、step boundary 和错误数据边界。
  - 定义对象安全可用的 `AgentLoop` 抽象及可装箱推进 stream。
  - `AgentEvent::Llm` 透明承载 Client `StreamEvent`。
  - `StepBoundary` 携带 `conversation::Boundary`、`StepId` 和 trace metadata。
  - `Done` 区分完成、预算耗尽、取消、错误和等待外部恢复。
  - 实现 feed 重入/backpressure guard，活跃 stream 未结束前再次 feed 必须被拒绝，stream drop 后清理状态。
- 下一步读取 `docs/agent-layer.md` §1.3/§2、现有 `src/agent`、`src/client`/`src/stream` 事件类型和测试结构，再制定具体编辑点。
- 已读取设计与现有源码：
  - `StreamEvent`、`ClientError`、`Boundary`、`TraceNodeId` 均已有 serde/data shape，可直接复用。
  - `QueuedPivot` 已表达 user-only pivot，可作为 `PivotMessage` 契约复用，避免重复模型。
  - 当前没有 Agent loop trait 或 feed guard，因此 M2-1 需要新增模块。
- 具体编辑计划：
  1. 新增 `src/agent/event.rs`：定义 `AgentInput`、`AgentUserInput`、`ResumeInput`、`PivotMessage`、`AgentEvent`、`StepBoundary`、tool/approval payload、`AgentOutcome`、`AgentErrorKind`、`AgentFailure` 与 `AgentError`。
  2. 新增 `src/agent/loop_driver.rs`：定义对象安全 `AgentLoop`、boxed event stream 类型、`AgentFeedGuard`、`AgentFeedPermit` 与 `AgentEventStream` wrapper；stream EOF 或 drop 都释放 active-feed 标记。
  3. 更新 `src/agent/mod.rs` 和 crate/README 文档，把 M2-1 的 stream 契约标为已暴露的 Agent 层边界，但不声明默认 loop driver 已实现。
  4. 增加聚焦测试：事件 serde/data shape、`StreamEvent` 透传、done/outcome 分类、feed 重入拒绝、stream drop 与 EOF 后清理。
  5. 按要求运行格式化、clippy、聚焦测试、全量测试、rustdoc 和 diff check。
- 已完成代码实现：
  - `src/agent/event.rs` 新增 Agent input/event/outcome/error 数据契约。
  - `src/agent/loop_driver.rs` 新增对象安全 loop trait、boxed stream 类型和 feed guard。
  - `src/agent/mod.rs`、`src/lib.rs`、`README.md` 已更新公开导出和能力说明。
- 已完成验证：
  - `cargo fmt --all`
  - `cargo clippy --all-targets -- -D warnings`
  - `cargo test agent::event`
  - `cargo test agent::loop_driver`
  - `perl -e 'alarm 1800; exec @ARGV' cargo test --all --all-targets`
  - `cargo test --doc`
  - `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`
  - `git diff --check`
- 已更新 `TODO.md`：M2-1 标题标记为 `[DONE]`，并补充完成记录。
- 下一步检查 git diff/status 后提交本次任务变更。
