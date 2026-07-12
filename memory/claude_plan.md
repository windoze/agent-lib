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
