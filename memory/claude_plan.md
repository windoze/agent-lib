# 当前调用执行计划

## 目标与约束

- 以 `TODO.md` 为唯一的任务排序与完成状态依据，找到标题中第一个没有 `[DONE]` 前缀的任务。
- 在选择任务前不做开放式历史问题排查；仅检查最新提交是否明确提到与当前任务直接相关的未完成问题。
- 本次调用只完成一个任务。若遇到会阻止正确实现的具体前置问题，则按规则把最少的前置任务写入 `TODO.md`、保持当前任务未完成、提交任务编排变更后停止。
- 不采用缩窄范围、特殊用例或临时兼容层来绕过规格问题。
- 保留工作区中用户已有的改动；若这是上次中断后恢复的同一任务，完成时按要求把当前未提交文件原子地纳入本次提交。

## 执行步骤

1. 读取 `TODO.md`，严格按标题的 `[DONE]` 状态确定第一个未完成任务，并阅读该任务的全部要求、依赖、验收标准和完成记录。
2. 查看最新提交说明及工作区状态，只判断是否存在与所选任务直接相关的未完成事项或恢复现场；不开展宽泛历史缺陷扫描。
3. 阅读实现所必需的局部源码、测试、`PLAN.md` 相关阶段说明和仓库级指令，确认任务边界与现有设计。
4. 按任务规格完整实现；采用小而聚焦的补丁，并在关键实现步骤后重新检查受影响代码。
5. 增补或更新覆盖正常、边界与错误路径的测试。若发现未被后续明确任务覆盖的失败，立即修复，或在无法继续时添加最少前置任务并停止。
6. 按指定顺序验证：`cargo fmt --all`，然后 `cargo clippy --all-targets -- -D warnings`，再运行任务要求的测试与 `cargo test --all --all-targets`（完整测试最长不超过 30 分钟），最后按任务要求运行文档构建等附加检查。
7. 仅在全部验收条件满足且没有未安排失败时，将任务标题加上 `[DONE]` 并填写 `TODO.md` 完成记录。只有阶段级计划发生变化时才更新 `PLAN.md`。
8. 检查最终差异与状态，确认没有误改或遗漏；以清晰、包含任务编号的提交信息提交本次全部应纳入的改动。
9. 在本文件持续记录关键进度、计划变化和验证结果。提交完成后立即停止，不开始下一任务。

## 当前状态

- 已建立初始计划。
- 已完整读取 `TODO.md`；首个未完成任务为 `M4-1 [TODO] 接入 HTTP 客户端与 Anthropic 请求构造`。
- 本次任务边界：添加 `reqwest`（rustls/json/stream），实现 `ChatRequest` 到 Anthropic Messages 请求体的完整映射，应用 endpoint 的 base URL、认证、额外 header 与 query，并以不联网的单元测试验证请求 JSON 和 HTTP request 构造。
- 不进入后续 `M4-2` 响应解析或 `M4-3` SSE 流解析。
- 已检查最新提交 `86135e3`（仅完成 M3-R）与工作区：没有直接相关的未完成问题，也不是遗留实现恢复现场；初始工作区无其他未提交改动。
- 已阅读 M4-1 所需的 `PLAN.md`/`DESIGN.md` 约束、probe 记录以及现有 request/config/content/tool/extras/error API。阶段计划与依赖不变，无需修改 `PLAN.md`。
- 实现设计：
  - `AnthropicAdapter` 持有可复用的 `reqwest::Client` 和 `EndpointConfig`，公开构造器、endpoint 访问器与不发网的 request builder，便于 M4-2/M4-3 复用。
  - 私有 wire 请求模块负责显式转换全部现有 `ContentBlock`；尤其把中立 `Thinking { text }` 转为 Anthropic 的 `thinking` 字段，并保留 block/source `extra`，且标准字段优先于响应侧 extra。
  - `Role::Tool` 映射为 Anthropic `user`（tool_result 的协议归属）；`Role::System` 消息会被明确拒绝，要求使用 `ChatRequest.system`，避免产生无效 wire。
  - 匹配 Anthropic 的 provider extras 在 JSON 序列化最后一步合并；错误 provider 的 extras 返回可观测错误，不静默丢弃。
  - URL 以结构化方式追加 `/v1/messages`，应用重复 query、Bearer/任意 Header/None auth 与额外 headers；JSON content type 在 endpoint 未显式提供时自动补齐。
- 下一步：分小补丁添加依赖、adapter 骨架、请求转换与聚焦测试，然后依次执行格式化、严格 lint、完整测试和文档验证。

## 实施与验证进度

- 已在 `Cargo.toml` 添加 `reqwest 0.12`，关闭默认特性并启用 `json`、`rustls-tls`、`stream`；`Cargo.lock` 已同步。
- 已新增 `AnthropicAdapter`，支持默认/注入 `reqwest::Client`，并暴露 endpoint 配置与不发送网络请求的 `build_request`。
- 已新增独立请求模块：完成 Anthropic Messages body、`/v1/messages` URL、query、三种认证形式、额外 header、JSON content type 与 provider extras 的构造；完整覆盖 text/image/tool_use/tool_result/thinking wire 映射和 block/source extra 保留。
- 已新增 6 个不联网单元测试，覆盖完整请求 JSON、工具 schema、thinking signature、两类图片、Tool 角色、可选字段、Bearer/Header/None、路径/query/header、跨 provider extras 拒绝、System 消息拒绝及畸形 endpoint 错误。
- 已通过：
  - `cargo fmt --all`
  - `cargo clippy --all-targets -- -D warnings`
  - `cargo test adapter::anthropic::request::tests`（6 passed）
  - `cargo test --all --all-targets`（77 passed，0 failed，使用 1800 秒硬超时）
  - `RUSTDOCFLAGS='-D warnings' cargo doc --no-deps`
  - `git diff --check`
- 最终审阅未发现阻塞 M4-1 的规格偏差或未安排测试失败；`PLAN.md` 的阶段顺序与验收标准未改变。
- 已把 `TODO.md` 中 M4-1 标题显式改为 `[DONE]` 并填写完成记录；确认下一项仍是 `M4-2 [TODO]`，本次不执行它。
- `TODO.md` 与本文件是在绿色全量验证后发生的纯 Markdown 记录更新，不影响已验证的编译输出，故按规则不重复运行完整测试。
- 最终任务顺序、工作区文件与完整差异已复核；本次 7 个文件均已暂存，`git diff --cached --check` 通过，staged 文件清单和请求实现/测试均已复读。
- 已以 `[M4-1] Implement Anthropic request construction` 创建本任务提交；本次任务全部完成，不执行 M4-2。最后仅确认提交记录与工作树状态。

## 2026-07-13 当前调用执行计划

### 目标与约束

- 以 `TODO.md` 为唯一任务顺序与验收来源，先定位标题中第一个没有 `[DONE]` 的任务。
- 本次调用只完成该任务；若遇到阻断该任务的具体前置缺陷，则按规则在 `TODO.md` 中加入最少的前置任务、提交记录并停止。
- 不做开放式历史问题扫描，不绕过规范差异，不以缩小模型或测试范围代替修复。
- 保护工作区已有改动；若这是一次中断后续作，则完成时把当前任务遗留的全部未提交文件纳入同一提交。

### 分步执行计划

1. 读取 `TODO.md`，确认第一个未完成任务的完整正文、依赖、验收项和完成记录格式；同时确认是否全部任务均已完成。
2. 在任务已选定后检查 `git status` 与最新提交，仅判断已有未提交内容或最新提交是否直接属于/影响当前任务，不开展无边界缺陷排查。
3. 阅读当前任务直接涉及的设计、源码和测试；建立需求到实现与测试的逐项映射。如发现会使指定行为无效的既有缺陷，先修复该缺陷；若无法在本任务中一起落地，则按规则新增最少前置任务并停止。
4. 用小而聚焦的补丁完成实现和测试；每完成关键实现、发现计划偏差或验证结果变化时，更新本文件的“执行进展与决策”部分。
5. 按规定顺序验证：`cargo fmt --all`，然后 `cargo clippy --all-targets -- -D warnings`，再运行任务要求的定向测试和 `cargo test --all --all-targets`（完整测试最长 30 分钟），最后按任务/仓库要求执行文档构建等其他检查。任何未被明确排期的失败都必须修复或转化为前置任务，不能忽略。
6. 验收全部通过后，在 `TODO.md` 的任务标题前添加 `[DONE]` 并填写精确完成记录（实现内容、测试命令与结果）。仅在阶段级顺序、依赖、假设或完成标准确有变化时修改 `PLAN.md`。
7. 复查差异、任务边界、格式和无警告要求；更新本文件记录最终状态。
8. 使用清晰、包含任务编号的提交信息提交本任务全部相关改动，然后确认工作区状态并停止，不开始下一个任务。若所有任务此前已完成，则改为执行最终审查、必要调整、提交并创建 `endtag`。

### 执行进展与决策

- 计划已建立，并已读取 `TODO.md`。
- 首个未完成任务确认为 **M4-2 Anthropic 非流式响应 → `Response`**；本次调用不会进入 M4-3。
- 当前验收范围：解析 text/tool_use 等 Anthropic content block；保留 stop reason 的归一化值与 raw；把 input/output/cache creation/cache read 以及 cache creation 的细分字段正确落位/保留；未知顶层与块级字段进入 `extra`；添加基于真实探测样本的单元测试和默认 `#[ignore]` 的真实 `databricks-claude-haiku-4-5` 集成测试。
- 下一步只检查工作区状态、最新提交及 M4-2 直接依赖的现有 Anthropic adapter/API，判断是否存在中断遗留或直接阻断项。
- 工作区检查结果：开始时除本文件外无未提交改动；最新提交 `ceb8e7c [M4-1] Implement Anthropic request construction` 没有声明未完成问题，也未发现 M4-2 的直接前置阻断。
- 实现设计已明确：新增聚焦的 `src/adapter/anthropic/response.rs`，用 Anthropic wire 中间类型解析 assistant role、text/tool_use/thinking 块，块内未知字段进入各自 `extra`；顶层未建模字段进入 `Response.extra`；thinking 的 wire 字段 `thinking` 映射到统一模型的 `text` 并保留 `signature`。
- Usage 沿用统一 `Usage` 的防御性反序列化：`input_tokens`/`output_tokens`、`cache_creation_input_tokens`/`cache_read_input_tokens` 分别归一；`cache_creation` 的 5m/1h 细分以及其他未知 usage 字段保留在 `Usage.extra`，不新增与既有模型冲突的临时字段。
- 新增 `AnthropicAdapter::chat` 非流式公共入口：复用 M4-1 request builder；拒绝错误的 streaming 请求形态；发送失败映射为 Timeout/Network；非 2xx 响应经统一 `ClientError::from_http_response` 分类并保留响应体；成功体交给同一解析函数。
- 测试布局：response 模块内用真实 Anthropic/Foundry 形态 JSON 覆盖 text + tool_use、thinking signature、stop reason 已知/未知 raw、顶层/块级/usage 逃生舱和错误边界；`tests/integration_anthropic.rs` 通过公共 API 做默认忽略的真实 `hi` 调用，缺环境变量时按 `PLAN.md` 约定跳过。
- 已完成实现阶段：新增 response parser 与非流式 `chat`；真实探测获得 text 和 tool_use 两份 Foundry JSON，确认 `cache_creation` 的实际键为 `ephemeral_5m_input_tokens`/`ephemeral_1h_input_tokens`，样本已作为测试 fixture 固定。
- 已完成测试编写：解析测试覆盖真实 text/tool_use、thinking signature、未知 stop reason raw、顶层/块级/usage extra 及畸形输入；本地一次性 HTTP 测试覆盖成功路径、429 `Retry-After`、非法 2xx body 和 streaming 请求拒绝；真实集成测试默认 ignore、缺环境变量跳过且有 55 秒 timeout。
- 下一步严格按验证顺序执行 `cargo fmt --all` → `cargo clippy --all-targets -- -D warnings`；修复其发现的问题后再跑聚焦测试与完整测试。
- 验证结果：`cargo fmt --all` 与 `cargo clippy --all-targets -- -D warnings` 通过；M4-2 聚焦测试 8/8 通过；带 1800 秒上限的 `cargo test --all --all-targets` 通过(85 passed,真实集成测试 1 ignored)；`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` 通过。
- 真实 endpoint 验证：加载 `.envrc` 后显式运行 ignored test，`databricks-claude-haiku-4-5` 的非流式 `hi` 调用成功并取得非空文本及 input/output usage，1/1 通过、耗时 1.85 秒。之后仅对 fixture 中 provider 分配的 id 做等形脱敏并同步断言，生产代码未改变；脱敏后重新执行 fmt、严格 clippy 与完整测试，仍全部通过。
- `TODO.md` 已将标题更新为 `M4-2 [DONE]` 并写入实现、逃生舱、测试与真实调用记录。阶段级顺序、依赖、假设和完成标准没有变化，因此未修改 `PLAN.md`。
- 最终文件清单已复核：共 10 个任务相关文件(3 个修改、7 个新增)已暂存；不包含 `.envrc`、`PLAN.md` 或无关文件。`git diff --cached --check` 通过，fixture 中真实 provider id 已脱敏，源码/测试只引用环境变量名而未写入认证值。
- 待完成动作：提交为单一 M4-2 提交并确认提交后工作树干净；随后停止，不执行 M4-3。
