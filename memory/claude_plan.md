本轮可审查推理摘要与执行计划

约束摘要
- `TODO.md` 是任务顺序、需求、依赖、验证和完成记录的唯一权威来源。
- 本轮只完成第一个标题未带 `[DONE]` 的任务，完成后提交 Git commit 并停止。
- 若遇到会阻塞当前任务的规格不匹配、已有缺陷或失败测试，必须修复，或在 `TODO.md` 中插入最小必要的前置任务并提交后停止。
- 不做开放式历史问题扫描；只处理当前任务和直接阻塞当前任务的问题。
- 修改前后需持续更新本文件，记录计划变化和关键步骤完成状态。

初始步骤计划
1. 读取 `TODO.md`，按标题 `[DONE]` 前缀识别第一个未完成任务。
2. 查看该任务的具体要求、依赖、验证要求和完成记录；必要时查看 `PLAN.md` 获取阶段背景，但不把它当作执行日志。
3. 检查最新提交信息是否明确提到与该任务直接相关的未完成事项；若相关，将其纳入当前任务或作为前置任务写入 `TODO.md`。
4. 针对选定任务读取相关源码、测试和文档，限定在完成该任务所需范围内。
5. 实现任务，优先沿用现有模块结构、类型和测试风格；若发现必须新增前置任务，更新 `TODO.md` 后停止。
6. 运行要求的验证，顺序为 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`，然后在需要时运行 `cargo test --all --all-targets`，完整测试超时不超过 30 分钟。
7. 若验证失败，按测试失败策略修复或在 `TODO.md` 中安排明确任务；不能留下未排期失败。
8. 完成后在 `TODO.md` 的任务标题前加 `[DONE]`，更新 completion record；仅在阶段计划变化时更新 `PLAN.md`。
9. 查看 Git 状态，提交本轮所有相关未提交变更，提交信息包含任务编号或明确任务描述。
10. 提交后停止，不继续下一个任务。

当前状态
- 已读取 `TODO.md` 并按标题 `[DONE]` 前缀确认当前第一个未完成任务为：
  `M6-2 [TODO] 两家 Client request mapper 兼容性验收`。
- 已确认更早的 M1--M5、M6-1 均标为 `[DONE]`；M6-3、M6-R、M7-1 仍在后续，不属于本轮执行范围。

当前任务执行计划
1. [完成] 检查最新 Git 提交信息：最新提交为 `[M6-1] Add conversation state machine acceptance`，
   未明确提出与 M6-2 直接相关的未完成事项；当前未提交变更仅有本计划文件。
2. [完成] 阅读 M6-2 相关设计与代码：`docs/conversation-core.md`/`docs/agent-layer.md` 仅在必要时参考，
   重点读取 adapter request mapper、`EffectiveView`、Conversation state machine fixtures 和
   现有跨 adapter 测试。
   已确认可复用 `tests/conversation_state_machine/support.rs` 的 deterministic public-API fixture；
   两家 adapter 均提供无网络 `build_request(&ChatRequest)`。
3. [完成] 构造只通过 public Conversation API 得到的 canonical `EffectiveView`，覆盖 system、多轮、
   parallel tool、cancelled tool result、reasoning 和 compaction artifact。
4. [完成] 用本地 dummy endpoint 组装 Anthropic Messages 与 OpenAI Responses 的 `ChatRequest`，
   调用两家现有 request builder，确保无网络访问。
5. [完成] 在统一断言中检查相同 view 的 role/content/tool pairing 语义；在 adapter 专属 helper 中仅检查
   Anthropic/OpenAI wire 字段差异，特别是 `Denied/Cancelled` 的 error/incomplete 映射。
6. [完成] 增加非法 Conversation 数据无法通过 public API 构造或无法绕过 Core 不变量的回归；避免依赖
   adapter 作为最后防线。
7. [进行中] 运行 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、跨 adapter 聚焦测试、
   `cargo test --all --all-targets`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` 和
   `git diff --check`。
   - `cargo fmt --all` 已通过。
   - `cargo test --test conversation_adapter_compat -- --nocapture` 已通过（2 passed，无 warning）。
   - `cargo clippy --all-targets -- -D warnings` 已通过。
   - `perl -e 'alarm 1800; exec @ARGV' cargo test --all --all-targets` 已通过：
     287 个库测试、3 个 capability 集成测试、2 个新增 adapter compat 集成测试、3 个
     state machine 集成测试通过；7 个真实 endpoint 测试按配置 ignored；examples test target 编译通过。
   - `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` 已通过。
   - `git diff --check` 已通过。
8. [完成] 成功后将 `TODO.md` 中 M6-2 标题改为 `[DONE]` 并补 completion record。
   完整测试后仅修改了 Markdown 记录与本计划文件，按任务规则无需重跑全量测试；需要重新执行
   `git diff --check` 确认最终文档 diff 无 whitespace 问题。
   - 最终 `git diff --check` 已通过。
9. [完成] 提交本轮相关变更后停止。
   - 提交信息：`[M6-2] Add cross-adapter conversation request acceptance`。
