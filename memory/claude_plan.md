# 当前执行计划

## 目标与约束

- 本次调用只完成 `TODO.md` 中按顺序出现的第一个标题未带 `[DONE]` 的任务，然后停止。
- `TODO.md` 是任务内容、顺序、依赖、验证要求和完成记录的唯一权威来源；仅在阶段级计划确实变化时修改 `PLAN.md`。
- 在选择当前任务前不做开放式历史问题排查；只检查最新提交是否明确提到与当前任务直接相关的未完成问题。
- 不通过缩小范围、特殊分支或临时兼容层绕开规格缺口。若出现无法在当前任务内正确解决的具体前置阻塞，则以最小数量的新前置任务更新 `TODO.md`，提交该结构性变更并停止。
- 任何观察到且未被后续明确任务覆盖的测试失败，都必须在本次修复或被安排为当前任务之前的明确前置任务。
- 保留工作区中用户已有的改动；若本次是在恢复意外中断的同一任务，则最终提交需包含所有当前未提交文件。

## 分步执行方案

1. 读取 `TODO.md`，严格按标题是否带 `[DONE]` 判断完成状态，锁定首个未完成任务，并记录其需求、依赖、验收标准和指定验证命令。
2. 查看简洁的 Git 状态以及最新提交说明，识别用户已有改动，并判断最新提交是否明确指出了与当前任务直接相关的未完成问题；不进行无边界历史缺陷扫描。
3. 将已锁定的任务、相关文件、风险和具体验证清单补充到本文件；若发现执行方案变化，也同步更新。
4. 阅读完成该任务所必需的设计、实现和测试文件，确认当前行为与任务规格之间的差距。
5. 用小而聚焦的补丁完整实现任务；每完成关键实现或改变方案，更新本文件。若遇到具体前置阻塞，按规则更新 `TODO.md`（必要时才更新 `PLAN.md`）、提交后停止。
6. 添加或调整覆盖正常路径、边界条件和错误路径的测试；不以任务私有特例替代通用修复。
7. 按规定顺序验证：先 `cargo fmt --all`，再 `cargo clippy --all-targets -- -D warnings`，最后运行任务指定测试和不超过 30 分钟的 `cargo test --all --all-targets`；如任务要求，再运行 `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`。确保单个测试不超过 1 分钟。
8. 复核改动和测试结果，在 `TODO.md` 中给任务标题加 `[DONE]`，并填写实际实现与验证记录；只有阶段级依赖或完成标准变化时才更新 `PLAN.md`。
9. 再次检查格式、差异和 Git 状态，确认没有遗漏相关文件、凭据或无关破坏。
10. 以包含任务编号的清晰消息提交本次所有应纳入的改动，确认工作区/提交结果，然后停止，不开始下一个任务。

## 当前状态

- 已在运行任何仓库检查命令前建立本计划。
- 已读取 `TODO.md` 并按标题标记确认：M1--M6 及其 Review 均为 `[DONE]`，首个未完成任务是 `NEXT-1 [TODO] 归档 Client 层计划并建立 Conversation Core 计划`。
- 本次只执行 `NEXT-1`，完成归档、根计划重建、引用核对、文档与 Rust 全量验证、完成记录和 Git 提交后停止；不会开始新 `TODO.md` 中的 Conversation Core 实现任务。
- 已检查 Git：本次开始前工作区无未提交改动；最新提交 `1429698` 仅完成 M6-R，没有声明与 NEXT-1 直接相关的遗留问题。
- 已完整阅读 `docs/conversation-core.md`、旧 `PLAN.md`、`DESIGN.md` Conversation/序列化章节及现有 Client 消息、工具、stream accumulator 和两家请求 mapper 的公共边界。
- 已在 `docs/archive/2026-07-13-client-layer/` 建立旧 `PLAN.md`/`TODO.md` 副本，并通过 `cmp` 确认它们与覆盖根文件前的源文件逐字节一致。
- 发现并纳入新计划的具体前置缺口：`ToolResponse.status` 支持 `Cancelled`，但 `ContentBlock::ToolResult` 只存 `is_error`，不能无损持久化 cancel 结果；必须在 pending/cancel 实现前做共享模型的类级修复。
- 已重建根 `PLAN.md`：范围限定为 Conversation Core，明确规范优先级、13 项硬决策、M1--M6 依赖、目录/API、测试策略、serde/持久化边界和每阶段 Review。
- 已重建根 `TODO.md`：共 28 个 `[TODO]` 任务，M1 为 1--3、M2--M5 为 1--4、M6 为 1--3，六个里程碑均以独立 `Mx-R` 收尾；结构化检查确认每项均有前置依赖、上下文、做什么和验证。
- 新任务显式排期了 tool-result 状态类级修复、受检 Turn 反序列化、cancel 三种一致 disposition、stale/ABA Boundary、逻辑 revert/redo、可判定 O(1) fork、head 内摘要防泄漏、restore corruption 检查及存盘→恢复→effective_view 一致性。
- 已更新 README 与 capability matrix：当前计划入口继续指向根 `PLAN.md`/`TODO.md`，明确属于历史 Client endpoint/完成记录的引用改到归档路径。
- 规定验证已全部通过：`cargo fmt --all -- --check`；严格 Clippy；完整测试 133 passed、7 ignored、0 failed（8.4 秒）；`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`；完成状态写入后的 `git diff --check`。
- 已在归档 Client `TODO.md` 将 `NEXT-1` 标为 `[DONE]` 并填写实际归档、计划、任务结构、引用与验证记录；根 Conversation `TODO.md` 仍全部为 `[TODO]`。
- 已完成最终 diff/status 审查并创建本次 `NEXT-1` 提交；本次调用到此停止，不执行 M1-1。

## 当前任务的具体交付

1. 在 `docs/archive/2026-07-13-client-layer/` 原样保存执行任务前的根 `PLAN.md` 与完整 `TODO.md`，归档 `TODO.md` 必须保留 `NEXT-1` 本身及全部历史完成记录。
2. 完整阅读 `docs/conversation-core.md`、旧 `PLAN.md` 及与当前/历史计划入口有关的仓库文档，提取已经确定的范围、不变量、公共类型边界、实现顺序、测试和持久化约束。
3. 重建根 `PLAN.md`：只规划 Conversation Core，明确范围/非目标、关键不变量、真实依赖顺序、目录与 API、测试策略、serde/持久化边界和阶段 Review。
4. 重建根 `TODO.md`：与新计划逐里程碑一一对应；实现任务编号连续唯一且全部为 `[TODO]`，每项包含充分上下文、具体实现、依赖/边界/错误行为和可判定验证；每个里程碑以独立 `Mx-R [TODO]` 收尾。
5. 只把明确指向历史 Client 计划/任务的链接改到归档路径；仍表示当前实施入口的引用继续指向根 `PLAN.md`/`TODO.md`。
6. 人工和脚本化核对归档完整性、新计划/TODO 对齐、标题状态、编号唯一连续、Review 完整、范围无 Agent loop/Tool registry/多 agent 编排混入。
7. 由于任务要求明确指定完整验证，即使主要为文档改动也依次运行 `cargo fmt --all -- --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` 与 `git diff --check`。
8. 在归档的 Client `TODO.md` 中将 `NEXT-1` 标为 `[DONE]` 并填写真实完成记录；根 `TODO.md` 只保留尚未执行的 Conversation Core `[TODO]` 项。随后提交本次全部应纳入改动并停止。
