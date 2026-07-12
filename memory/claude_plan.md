# 当前执行计划

## 目标与边界

- 以 `TODO.md` 为唯一任务排序与完成状态依据。
- 本次只处理标题中第一个未带 `[DONE]` 的任务；完成并提交后立即停止。
- `PLAN.md` 仅在阶段级顺序、依赖、假设或验收标准发生变化时更新。
- 不进行开放式历史问题扫描；仅检查当前任务、最新提交中与其直接相关的未完成事项，以及验证过程中实际暴露的问题。

## 执行步骤

1. 读取 `TODO.md`，定位第一个未完成任务，提取其依赖、要求、测试与完成记录格式。
2. 检查最新提交说明、工作区状态及与当前任务直接相关的文件，确认是否存在续作或直接前置问题；保护用户已有改动，不回退无关内容。
3. 若任务可以按原规格完成，直接实现完整范围并补充相应测试与文档；若发现阻断规格实现的具体缺陷，则只新增最少的前置任务、调整 `TODO.md` 依赖并提交后停止。
4. 采用小而聚焦的补丁逐步修改；每完成关键实现或判断后更新本文件的进度与验证结果。
5. 按规定顺序验证：`cargo fmt --all`，然后 `cargo clippy --all-targets -- -D warnings`，最后在不超过 30 分钟的限制下运行 `cargo test --all --all-targets`；根据任务要求补充其他验证。若仅有文档改动且可复用最近的绿色全量结果，则在完成记录中说明。
6. 对任何未被明确排期的测试失败，先修复，或在 `TODO.md` 中添加位于依赖者之前的最少修复任务；存在此类失败时不将当前任务标为完成。
7. 完成后在 `TODO.md` 的任务标题前加 `[DONE]` 并填写可复核的完成记录；仅在阶段计划确有变化时修改 `PLAN.md`。
8. 复查差异与状态，提交本次任务涉及的全部未提交文件（若属于异常中断后的续作，则原子性纳入当前所有未提交文件），使用包含任务编号的清晰提交信息。
9. 确认提交成功、工作区符合预期后停止，不开始下一任务。若全部任务均已完成，则执行任务说明要求的最终复核并创建 `endtag`。

## 当前进度

- 已初始化执行计划。
- 已完整读取 `TODO.md`；本次唯一任务确定为 `M3-3 [TODO] EndpointConfig 与请求参数类型`。
- 当前任务要求：实现 `EndpointConfig`、可表达 Bearer/任意 Header/无认证的 `AuthScheme`，以及将 system 单列并承载 messages/tools/常用生成参数/provider extras 的 `ChatRequest`；补充 serde round-trip 与两个真实 endpoint 配置测试。
- 已检查最新提交、工作区状态、设计文档、真实 endpoint 参数、探测代码与现有 Client/Model API；未发现当前任务的直接阻断或未排期失败。
- 已新增 `client::config` 与 `client::request`，并从 `client` 重导出 `AuthScheme`、`EndpointConfig`、`ChatRequest`。
- 已添加两类真实 endpoint 配置、全部认证形态、完整/最小请求与 serde round-trip 测试。
- 聚焦验证通过：`cargo test client::config::tests` 为 3 passed；`cargo test client::request::tests` 为 2 passed；每个测试均远低于 1 分钟。
- `cargo fmt --all` 通过。
- `cargo clippy --all-targets -- -D warnings` 通过，无 warning。
- 带 1800 秒硬超时保护的 `cargo test --all --all-targets` 通过：70 passed、0 failed、0 ignored；所有测试均远低于 1 分钟。
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` 通过。
- 完成前代码差异审查与 `git diff --check` 通过；实现没有越界到 M3-4，也没有意外修改 `PLAN.md` 或其他项目文件。
- 已将 `TODO.md` 的 M3-3 标题改为 `[DONE]` 并填写实现与验证记录。测试后仅修改 Markdown 进度记录，复用上述绿色编译/测试结果。
- 最终复核任务顺序、完整差异与工作区均通过；全部变更已纳入 `[M3-3] Implement endpoint and request configuration` 提交。本次任务完成，停止且不开始 M3-4。

## M3-3 具体实现方案

1. 新增 `src/client/config.rs`：定义可 serde 的 `EndpointConfig` 与 `AuthScheme`；认证枚举用稳定的相邻标签 JSON 表示，并覆盖 Bearer、任意单 Header（包括 `api-key`/`x-api-key`）和无认证。
2. `EndpointConfig` 严格采用任务指定字段：`base_url: String`、`auth: AuthScheme`、`query_params: Vec<(String, String)>`、`extra_headers: Vec<(String, String)>`。两个 `Vec` 保留重复键和调用方顺序，不提前引入 HTTP 运行时类型。
3. 在配置测试中构造计划记录的两个真实 endpoint 形态：Anthropic Foundry 使用 Bearer 与 `anthropic-version`；OpenAI Responses Foundry 使用 `api-key` Header 与 `api-version=2025-04-01-preview` query。断言字段并完成 serde round-trip；另覆盖 `x-api-key` 与 `None` 认证变体。
4. 新增 `src/client/request.rs`：定义可 serde 的 `ChatRequest`，包含 `model`、`messages`、`tools`、单列 `system`、`max_tokens`、可选 `temperature`、`stream` 与可选 `provider_extras`。
5. `max_tokens` 采用必填 `u32`，以满足已排期的 Anthropic wire 必填约束；`temperature` 采用 `Option<f32>`，保留 endpoint 不接受或调用方不指定时的“不发送”语义。除任务列出的字段外不臆造未约定的请求参数，provider 方言继续通过既有 `ProviderExtras` 承载。
6. 请求测试构造包含消息、工具 schema、system、生成参数和绑定 Anthropic 的 provider extras 的完整请求，断言 serde round-trip、system 与消息序列分离，以及 provider 绑定不丢失。
7. 在 `src/client/mod.rs` 公开并重导出新模块类型；保持现有公共 API 风格和 `missing_docs` 要求。
8. 聚焦测试通过后，依次运行 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、30 分钟内的 `cargo test --all --all-targets`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` 与 `git diff --check`。

## 关键判断与变更日志

- `TODO.md` 中此前任务均已显式标记 `[DONE]`，第一个未完成标题为 `M3-3 [TODO]`；未跳过 review 任务。
- 任务当前未显示需要拆分；在完成相关检查前不扩大或缩小其建模范围。
- 最新提交为 `a58e693 [M3-2] Implement structured capabilities`，提交说明未提及与 M3-3 直接相关的遗留问题。
- 初始工作区除按用户要求更新的 `memory/claude_plan.md` 外无未提交改动；本次不是异常中断后的代码续作。
- `DESIGN.md` 要求 endpoint config 与 wire protocol 解耦，`PLAN.md` 要求 config 数据类型可 serde；实现不会引入 `reqwest` 或提前实现 M4 HTTP 行为。
- `PLAN.md` 的真实配置明确给出 Anthropic Bearer 和 OpenAI `api-key` + query 两种形态；通用 Header 变体同时覆盖文档提到的 `x-api-key`。
- 代码实现未引入任务外 HTTP 行为或阶段级计划变化；当前无需修改 `PLAN.md`。
- M3-3 的实现、测试与文档记录均已完成；M3-4 保持 `[TODO]`，本次不会继续处理。
