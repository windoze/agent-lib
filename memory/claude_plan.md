# 当前执行计划

## 决策原则

- `TODO.md` 是任务顺序、依赖、验收要求和完成状态的唯一事实来源。
- 本次调用只处理首个标题未带 `[DONE]` 的任务；完成或登记阻塞前置任务并提交后立即停止。
- 先选定当前任务，再做与该任务直接相关的检查，不进行开放式历史问题排查。
- 不用缩小范围、替代表示或临时兼容来规避规范。若出现真正阻塞当前任务的缺陷，则修复它；若必须新增前置任务，则在 `TODO.md` 中插入最少任务、提交并停止。
- 未被后续任务明确安排的测试失败必须修复或明确排期，不能作为既有噪声忽略。

## 分步计划

1. 首先读取 `TODO.md`，按标题是否带 `[DONE]` 确定第一个未完成任务，并摘录其依赖、范围、验收与完成记录要求。
2. 在选定任务后检查工作树状态与最新提交；只判断未提交改动和最新提交是否与当前任务直接相关，保留用户已有改动。
3. 阅读当前任务直接引用的设计文档、源码与测试，核对现状和预期行为；如计划因证据发生变化，立即更新本文件。
4. 用多个小而聚焦的补丁完整实现当前任务，并在每个关键步骤后复读相关代码；不提前处理下一任务。
5. 添加或调整覆盖正常路径、边界、错误路径和序列化兼容性的相关测试，并先运行针对性验证。
6. 按指定顺序执行最终验证：`cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets`（最长 30 分钟）、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`。若任务另有验收命令，也一并执行。
7. 更新 `TODO.md`：仅在所有要求和验证满足后给当前任务标题加 `[DONE]`，并写入准确的完成记录。只有阶段级计划变化时才更新 `PLAN.md`。
8. 检查最终差异与工作树，确保任务边界、文档、格式和测试结果一致；若是异常中断后的续做，提交所有当前未提交文件。
9. 创建一个清晰描述当前任务的 Git 提交，并确认提交成功、工作树状态符合预期，然后停止，不进入下一个任务。

## 当前状态

- 已读取 `TODO.md` 并按标题状态确定首个未完成任务为 `M3-R [TODO] Milestone 3 Review`。
- 当前任务是一个真实的 review 执行单元，不拆分，也不进入 `M4-1`。
- 本任务的直接验收范围：
  1. 核对 `ClientError` 分类是否保留 retry/backoff 所需信息，并检查 HTTP 分类测试覆盖。
  2. 核对 `Capability` 是否为结构化模型，默认能力表与覆盖语义是否成立。
  3. 核对 `EndpointConfig` 是否能表达 Anthropic/OpenAI 两个真实 endpoint 的认证、query 与 header 差异。
  4. 核对 `LlmClient` 是否 dyn-safe，且 `Box<dyn LlmClient>` 有编译与运行测试。
  5. 确认 `StreamEvent::Error` 已使用真实 `ClientError`，不存在 M2-2 临时字符串载荷。
  6. 检查 M3 公共 API 文档、模块组织和相关测试；发现同根问题时做类级修复。
  7. 依次完成格式化、严格 lint、全量测试与无 warning 文档构建，再更新 `TODO.md` 完成记录并提交。
- 下一步检查工作树与最新提交，只处理和本 review 或异常中断续做状态直接相关的内容。
- 已检查工作树与最新提交：除本计划文件外无未提交改动；最新提交 `8f0ef0a [M3-4]` 未声明相关未完问题，本次不是遗留代码续作，也不需要新增前置任务。
- 已对照 `DESIGN.md` §3--§5、`PLAN.md` 已定决策及 M3 源码/测试完成静态审阅：
  - `ClientError` 区分限流、超时、context 超限、内容过滤、网络、协议、认证和通用 API 错误；429 保留可选 `Retry-After`，未知 API 错误保留 status/body。
  - `Capability` 使用 context 上限、输入/输出模态集合、功能字段及 stop reason 集合；Anthropic/OpenAI 默认表可克隆覆盖且不会修改全局表。
  - `EndpointConfig` 通过 Bearer/任意 Header/None、query 列表和额外 header 列表表达两个 Foundry endpoint 的差异。
  - `LlmClient` 使用 `#[async_trait]` 且继承 `Send + Sync`，现有测试把实现装箱为 `Box<dyn LlmClient>` 并实际调用两条路径。
  - `StreamEvent::Error(ClientError)` 已回填真实分类类型，源码中未发现 placeholder/TODO/FIXME。
- 聚焦验证 `cargo test client::` 已通过：22 passed、0 failed、0 ignored，单测均远低于 1 分钟。
- 审阅发现唯一需要修正的实质问题是 README 状态陈旧：仍写“当前完成 Milestone 1”，没有反映 M2 流式聚合与 M3 Client 抽象已完成。此项属于 M3 review 的公共文档一致性修复，不改变阶段计划，也不提前实现 M6-3 的完整 Client 用法示例。
- 下一步以小补丁更新 README 的当前状态与模块概览，复读差异后按规定顺序执行最终验证。
- 已更新并复读 README：状态准确表述为完成 M1--M3，设计边界补充 `stream` 与 `client`；`git diff --check` 通过。
- 最终验证第 1 步 `cargo fmt --all` 已通过；下一步运行严格 clippy，在它通过前不启动全量测试。
- 最终验证第 2 步 `cargo clippy --all-targets -- -D warnings` 已通过且无 warning；下一步以 1800 秒硬超时运行完整测试套件。
- 最终验证第 3 步 `/opt/homebrew/bin/timeout 1800 cargo test --all --all-targets` 已通过：71 passed、0 failed、0 ignored；全部测试均远低于 1 分钟。
- 下一步运行带 `-D warnings` 的 rustdoc；通过后才会将 M3-R 标为 `[DONE]`。
- 最终验证第 4 步 `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` 已通过，无公共文档 warning。
- M3-R 的全部清单均有静态审阅和自动化测试证据，且未发现需新增前置任务的规格偏差；下一步将 `TODO.md` 中 M3-R 标题改为 `[DONE]` 并填写完成记录。
- 阶段级顺序、依赖、假设与完成标准没有变化，因此不修改 `PLAN.md`。
- 已将 `TODO.md` 的 M3-R 标题显式改为 `[DONE]`，并记录审阅证据、README 修正及全部验证结果；后续首个任务 `M4-1` 保持未动。
- `TODO.md` 与本计划文件是在成功全量测试后发生的纯 Markdown 记录更新，不影响编译输出；按任务规则复用刚完成的绿色全量结果，无需重复运行测试。
- 下一步执行最终任务顺序、差异、空白错误与工作树检查；确认仅包含本次 review 文件后，暂存全部本次改动、审查 staged diff 并以 `[M3-R]` 提交，然后停止。
- 最终检查已完成：差异仅包含 `README.md`、`TODO.md` 与本计划文件；M3-R 已完成、M4-1 仍为下一任务，`PLAN.md` 未改，完整 diff 已复读且 `git diff --check` 通过。
- 已暂存并检查三个目标文件，staged diff 无空白错误；已创建 `[M3-R] Complete Milestone 3 review` 提交。本次调用的任务工作全部完成，最终只确认提交与工作树状态，不开始 M4-1。
