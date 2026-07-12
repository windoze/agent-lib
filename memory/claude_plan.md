# 当前执行计划

## 目标与约束

- 以 `TODO.md` 为唯一的任务排序与完成状态依据，只处理第一个标题尚未带 `[DONE]` 的任务。
- 在选定任务前不做开放式历史问题排查；仅检查最新提交中与当前任务直接相关的未完成事项。
- 完整实现、测试、记录并提交该任务；完成后立即停止，不进入下一任务。
- 若遇到会阻塞当前任务且尚未跟踪的真实前置问题，只在 `TODO.md` 中新增最少的前置任务、提交后停止。
- 保留仓库已有的用户改动；如这是一次中断后的续作，则按要求将当前未提交状态一并纳入最终原子提交。

## 分步计划

1. 读取 `TODO.md`，定位第一个标题未带 `[DONE]` 的任务，并完整提取其要求、依赖、验收项与完成记录格式。
2. 查看最新提交说明、工作树状态及与该任务直接相关的文件，判断是否存在续作状态或明确的相关未完成问题。
3. 将选定任务、范围判断和具体实施方案补充到本文件，再开始修改实现。
4. 以小而集中的补丁完成代码、测试与文档变更；每完成关键步骤就在本文件更新进度和必要的计划调整。
5. 按要求依次运行 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets`，并运行任务指定的其他验证；单次完整测试最长不超过 30 分钟。
6. 若验证失败，修复整个已识别的问题类别并补充覆盖；若是无法在当前任务内消除的具体前置阻塞，则按规则更新 `TODO.md` 后停止。
7. 验证全部通过后，在 `TODO.md` 的任务标题前添加 `[DONE]`，填写真实完成记录；仅在阶段级计划确有变化时更新 `PLAN.md`。
8. 检查最终差异和仓库状态，提交全部本次任务所需改动，提交信息清晰包含任务编号；确认提交成功后停止。

## 当前进度

- 已建立执行计划。
- 已完整读取 `TODO.md`；第一个标题未带 `[DONE]` 的任务是 `M3-4 [TODO] LlmClient trait`。
- 当前任务范围：定义 `Send + Sync`、使用 `#[async_trait]` 的 dyn-safe `LlmClient`，提供 `capability`、非流式 `chat` 与返回 `BoxStream<'static, Result<StreamEvent, ClientError>>` 的 `chat_stream`；以 mock 测试验证 `Box<dyn LlmClient>` 可调用。
- 已检查最新提交和工作区：最新提交为 `f5711a4 [M3-3] Implement endpoint and request configuration`，未提及相关未完成事项；除本计划文件外无未提交改动，本次不是遗留代码续作。
- 已核对 `PLAN.md`/`DESIGN.md` 及现有 `client`、`stream`、`Accumulator` API；没有发现 M3-4 的直接阻塞，也不需要拆分或改动阶段计划。
- 实现决策：不采用任务注明“可选”的默认 `chat`。后续 `M4-2` 明确要求实现真实非流式响应路径；默认强制走 `chat_stream` 会提前规定 `ChatRequest.stream` 是否改写及 `AccumulatorError -> ClientError` 的策略，并可能让非流式路径被旁路。两个方法均保持为适配器必须实现的独立入口。
- 具体修改：在 `src/client/mod.rs` 定义并完整文档化 `#[async_trait] pub trait LlmClient: Send + Sync`，直接使用任务指定的 `BoxStream<'static, Result<StreamEvent, ClientError>>` 返回类型；测试放入独立的 `src/client/tests.rs`，避免模块入口膨胀。
- 计划测试：构造同时实现非流式与流式入口的 mock，装箱为 `Box<dyn LlmClient>`，验证 capability、`chat` 结果和 `chat_stream` 调用；将 boxed stream 交给现有 `collect`，断言折叠结果与非流式响应完全一致，从编译与运行两方面证明 dyn-safe 和两种消费姿势可用。
- 已以两个小补丁完成 trait 与独立 mock 测试，实现未越界到 M3-R 或 provider 适配器。
- 聚焦验证通过：`cargo test client::tests::boxed_dyn_client_supports_complete_and_streaming_calls` 为 1 passed、0 failed，耗时远低于 1 分钟；dyn trait object、完整响应入口、boxed stream 与 Accumulator 折叠均已实际执行。
- 补丁复读和初次 `git diff --check` 均无问题，未发现需要新增前置任务的规格缺口。
- `cargo fmt --all` 已通过并完成格式化。
- `cargo clippy --all-targets -- -D warnings` 已通过，无 warning。
- `/opt/homebrew/bin/timeout 1800 cargo test --all --all-targets` 已通过：71 passed、0 failed、0 ignored；整套测试约 1 秒完成，单个测试均远低于 1 分钟。
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` 已通过，无文档 warning。
- 已审查 trait 与测试的完整代码差异、工作区状态和 `git diff --check`；实现只涉及 M3-4，未修改 provider 适配器或 M3-R，文件组织与公共文档符合现有约定。
- 已将 `TODO.md` 的 M3-4 标题显式改为 `[DONE]` 并填写实现、mock dyn-safe 验证及完整命令结果；M3-R 保持 `[TODO]`，它将是下一次调用的任务。
- 阶段级顺序、依赖、假设与验收标准均未变化，因此不修改 `PLAN.md`。测试通过后仅更新了 Markdown 进度记录，无需重跑编译测试，复用上述绿色结果。
- 下一步：执行最终任务顺序/差异/状态检查，暂存本次全部文件，审查 staged diff 后以 `[M3-4]` 提交；确认提交成功后立即停止。
