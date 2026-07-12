# 当前任务执行计划

## 原则与范围

- 以 `TODO.md` 为唯一任务顺序与完成状态来源，只执行首个标题未带 `[DONE]` 的任务。
- 在选择当前任务前不进行开放式历史问题排查；仅检查最新提交是否明确提到与当前任务直接相关的未完成事项。
- 不记录模型私有的逐字思维过程；本文件记录可审计的判断依据、执行步骤、关键发现、计划变更与验证结果。
- 若发现阻塞当前任务的真实规格缺口或未排期测试失败，按要求修复，或在 `TODO.md` 中插入最少的前置任务并提交后停止。

## 初始执行步骤

1. 阅读 `TODO.md`，定位首个未完成任务，提取其需求、依赖、验证命令和完成记录要求。
2. 检查工作树状态与最新一次提交，仅判断是否存在与该任务直接相关的遗留事项；不触碰无关用户改动。
3. 阅读当前任务直接涉及的设计文档与代码/测试，确认实现边界；随后把具体任务、验收标准和文件范围补充到本文件。
4. 完整实现当前任务，并以小而聚焦的补丁逐步修改；每完成关键步骤或改变计划即更新本文件。
5. 增补并运行相关测试。最终按顺序运行 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets`（最长 30 分钟）和任务要求的其他验证（如文档构建）。
6. 在所有要求通过后，将任务标题加上 `[DONE]` 并填写 `TODO.md` 完成记录；仅当阶段级计划确实改变时更新 `PLAN.md`。
7. 复查差异与工作树，提交当前任务要求的全部变更，使用清晰且包含任务编号的提交消息，然后停止，不处理下一任务。

## 当前状态

- 已完整读取到首个未完成任务并选定 **M3-1 `ClientError` 分类**；本次不会处理 M3-2 或后续任务。

## M3-1 具体范围与验收标准

- 在 `client/error.rs` 以 `thiserror` 定义 `ClientError`：`RateLimited { retry_after: Option<Duration> }`、`Timeout`、`ContextLengthExceeded`、`ContentFiltered`、`Network(..)`、`Protocol(..)`、`Auth`、`Api { status: u16, body: String }`、`Other(..)`。
- 提供从 HTTP status、body 与可用响应头信息进行分类的辅助 API；429 必须解析 `Retry-After`，并覆盖 Foundry 401/404/content-filter 一类响应形态。
- 将 M2-2 `StreamEvent::Error(String)` 占位回填为 `StreamEvent::Error(ClientError)`，同时保持事件 serde round-trip 能力与 Accumulator 错误传播行为。
- 为状态/响应体分类、`Retry-After`、serde round-trip 和流事件回填添加或调整测试；不能以窄化输入形态绕过同类错误。
- 最终验证顺序：聚焦测试 → `cargo fmt --all` → `cargo clippy --all-targets -- -D warnings` → `cargo test --all --all-targets`（不超过 30 分钟）→ `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` → `git diff --check`。

## 下一步

1. 检查 `git status` 与最新提交说明，识别是否存在 M3-1 直接相关的未提交/遗留事项。
2. 阅读 `DESIGN.md` 错误模型段落、现有 `client`/`stream` 模块与测试，确定 serde、错误所有权与公开 API 的现有约定。
3. 分小补丁实现错误类型、分类器和流事件回填，再逐层验证。

## 已确认的实现决策

- 2026-07-13：工作树除本文件外无修改；最新提交 `b52b468` 仅完成 M2 Review，没有提及 M3-1 的未完成阻塞项。
- `ClientError` 需要派生 `Serialize`/`Deserialize`、`Clone`、`PartialEq`/`Eq`，因为 `StreamEvent` 当前具备这些数据模型能力，回填后不能造成能力回退。
- HTTP 分类辅助不依赖尚未引入的 `reqwest`，输入采用状态码、响应正文和可选的 `Retry-After` 原始值；M4 可直接从响应头提取字符串后调用。
- `Retry-After` 同时支持标准的 delay-seconds 与 HTTP-date。为正确处理后者，增加轻量 `httpdate` 依赖，并以可注入当前时间的内部路径编写确定性测试；已过期日期归一为零等待，非法值为 `None`。
- 分类优先级：429 限流；408/504 超时；正文中的 context-length/content-policy 语义；401/403 认证；其余状态保留为 `Api { status, body }`。正文语义先于 403 认证，以正确承载 provider 用 403 表达的内容策略拒绝。
- `Network`、`Protocol`、`Other` 使用可序列化字符串上下文；`AccumulatorError::Stream` 改为承载完整 `ClientError`，不把已分类信息降级回字符串。

## 进展与验证记录

- 2026-07-13：已新增 `ClientError` 全部九类变体、HTTP 错误分类辅助、标准两种 `Retry-After` 形式解析，并在 `client` 模块公开重导出。
- 2026-07-13：已将 `StreamEvent::Error(String)` 回填为 `StreamEvent::Error(ClientError)`；`AccumulatorError::Stream` 同步保留分类类型及 error source 链。
- 2026-07-13：新增独立错误测试模块，覆盖全变体 serde、429 秒数/HTTP-date/无效值/过期值、408/504 timeout、413 与 provider context body、Foundry/Azure content filter、401/403 auth、404/500 原始 API 错误。
- 聚焦测试结果：`cargo test client::error::tests` 10 passed；首次运行只发现测试反序列化目标类型缺少显式标注，已修复后通过。
- 回归测试结果：`cargo test stream::` 19 passed，确认流事件 serde 与 Accumulator 分类错误传播无回归。
- 最终验证结果：`cargo fmt --all` 通过；`cargo clippy --all-targets -- -D warnings` 通过；`cargo test --all --all-targets` 60 passed；`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` 通过；此前及更新完成记录前的 `git diff --check` 通过。
- 已将 `TODO.md` 的 M3-1 标题更新为 `[DONE]` 并填写完成记录。阶段级顺序、依赖和验收标准未改变，因此不修改 `PLAN.md`。
- 2026-07-13：最终格式、差异与任务边界检查均通过；本次全部文件已纳入 `[M3-1] Implement classified client errors` 提交。完成最终工作树确认后停止，不开始 M3-2。
