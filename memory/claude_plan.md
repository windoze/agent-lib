# 当前执行计划

## 目标与边界

- 以 `TODO.md` 为唯一任务顺序与验收依据，识别并完成首个标题未带 `[DONE]` 的任务。
- 本次只完成一个任务；完成实现、验证、任务记录与 Git 提交后立即停止。
- 不做开放式历史问题扫描；只处理当前任务的直接依赖、阻塞问题、当前改动引入的回归，以及验证过程中发现且尚未排期的测试失败。
- 本文件记录可审查的计划、关键判断、命令结果摘要和进度，不记录模型私有的逐字思维链。

## 初始执行步骤

1. 首先读取 `TODO.md`，按标题是否有 `[DONE]` 判定首个未完成任务，并完整阅读该任务的要求、依赖、验收与完成记录。
2. 检查最新提交说明以及工作区状态，只判断它们是否与当前任务直接相关；保留用户已有改动，不擅自回退。
3. 按需读取 `PLAN.md`、相关源码、测试和项目说明，确定当前任务的实现边界与验证命令。
4. 若任务可以按原定义完成，直接实现；若存在无法绕过的具体前置缺陷，则只新增最少的前置任务到 `TODO.md`、更新依赖并提交后停止。
5. 使用小而集中的补丁实现代码与测试；每完成关键步骤就在本文件更新进度和必要的计划调整。
6. 按规定顺序验证：先 `cargo fmt`，再 `cargo clippy --all-targets -- -D warnings`，最后运行任务要求的测试和完整测试套件（完整 Rust 测试最长 30 分钟，单个测试不得超过 1 分钟）。如项目并非 Rust 或任务另有明确命令，则采用 `TODO.md` 指定的等价流程。
7. 对任何失败测试按测试失败政策处理：修复，或在当前任务之前/适当位置加入最少且明确的修复任务；未处理的失败存在时不把当前任务标记为完成。
8. 验收通过后，在 `TODO.md` 的任务标题前加 `[DONE]` 并补全完成记录；仅在阶段级计划确实变化时更新 `PLAN.md`。
9. 复查差异与 Git 状态，以清晰的任务编号提交相关改动；若确认是在续做意外中断的同一任务，则按要求把当前所有未提交文件一并纳入提交。
10. 确认提交成功并汇报任务、主要改动、验证结果和提交哈希，然后停止，不开始下一任务。

## 当前进度

- [x] 在运行其他命令前建立本执行计划。
- [x] 识别首个未完成任务：`M1-6 ProviderExtras（逃生舱 A）与 ProviderId`。
- [x] 确认相关工作区/最新提交状态与实现范围。
- [x] 完成实现与测试代码。
- [x] 完成格式化、静态检查和测试验证。
- [x] 更新 `TODO.md`；阶段计划未变化，无需更新 `PLAN.md`。
- [x] 提交本次任务并停止。

## 已确认的任务要求

- 在 `model/extras.rs` 定义可扩展的 `ProviderId`（当前至少包含 `Anthropic`、`OpenAiResp`）。
- 定义 `ProviderExtras { provider, fields }`，承载仅属于指定 provider 的请求侧方言字段。
- 实现 `merge_into(&self, body: &mut Value, target: ProviderId)`：仅 provider 匹配时在最终序列化阶段合并；不匹配必须可观测，且不得修改 body。
- 单元测试至少覆盖匹配合并与不匹配不合并，并按项目既有质量门禁完成验证。

## 范围检查与实现决定

- 最新提交 `8ee59a5` 已完成 `M1-5`，未提及会阻塞 `M1-6` 的未完问题；检查时工作区除本计划文件外干净。
- `DESIGN.md` 只规定 provider 不匹配时“可丢弃/报错”，项目当前没有日志抽象。为保证忽略行为可观测，`merge_into` 将返回具名结果枚举，其中包含源 provider 与目标 provider。
- provider 匹配但 `body` 不是 JSON object 时无法正确 merge；该情况将返回明确错误并保持 body 不变，不使用 panic 或静默忽略。
- `ProviderId` 和 `ProviderExtras` 属于可持久化的请求数据/config 边界，将派生 serde，并增加 round-trip 测试，为 `M1-R` 的“所有 M1 类型 serde round-trip”验收提供覆盖。
- `PLAN.md` 的阶段顺序、依赖和完成标准未变化，本任务不更新 `PLAN.md`。

## 实现进度

- 已在 `src/model/extras.rs` 实现 `ProviderId`、`ProviderExtras`、可观测的 `ProviderExtrasMergeOutcome` 与非对象请求体错误。
- merge 语义明确为最终阶段插入：匹配时 extras 的同名字段覆盖已有字段；不匹配时返回含双方 provider 的 outcome 且 body 保持不变。
- 已添加四类单元测试：匹配 merge、不匹配可观测 no-op、非对象错误且不修改 body、serde round-trip 与 wire name。
- 实现后的格式化、clippy、focused test 与完整测试套件均已通过。

## 验证结果

- `cargo fmt --all`：通过；源码已按 rustfmt 格式化。
- `cargo clippy --all-targets -- -D warnings`：通过，无 warning。
- `cargo test model::extras::tests`：通过，4 passed / 0 failed，每个测试耗时远低于 1 分钟。
- `cargo test --all --all-targets`：通过，30 passed / 0 failed / 0 ignored，完整运行远低于 30 分钟。
- 未观察到失败测试、spec mismatch 或需要排期的阻塞问题，可以完成 `M1-6`。

## 完成记录与提交准备

- `TODO.md` 中 `M1-6` 标题已显式改为 `[DONE]`，并记录实现内容和全部验证结果。
- `PLAN.md` 未修改，因为本次没有改变阶段级顺序、依赖、假设或完成标准。
- 最终差异仅包含 `src/model/extras.rs`、`TODO.md` 与本进度文件；`git diff --check` 和最终 `cargo fmt --all -- --check` 均通过。
- 完整测试后仅修改了 Markdown 完成记录与进度，不影响编译输出，按任务规则无需重跑完整套件。
- 已创建并补全任务提交（`[M1-6] Implement provider extras`）；最终只核验提交哈希与工作区清洁状态，然后停止，不开始 `M1-R`。
# 当前调用执行计划（2026-07-13）

## 目标与边界

- 唯一目标：从 `TODO.md` 中识别并完成标题未以 `[DONE]` 开头的第一个任务，然后停止；不提前处理后续任务。
- `TODO.md` 是任务顺序、依赖、验收要求和完成记录的唯一事实来源；仅当阶段级规划确实变化时才更新 `PLAN.md`。
- 先检查最新提交是否明确提到与当前任务直接相关的未完成问题；不做开放式历史缺陷扫描。
- 保留工作区中已有的用户改动；若这是一次中断后的续做，最终提交需包含当前任务遗留的全部未提交文件。

## 分步执行计划

1. 读取 `TODO.md`，按标题是否带 `[DONE]` 精确定位第一个未完成任务，并提取其依赖、实现范围、验收命令和完成记录要求。
2. 检查 Git 状态、最新提交及与该任务直接相关的现有改动，确认是否属于续做，以及最新提交是否声明了必须纳入当前任务的未完成问题。
3. 只读取完成当前任务所需的 `PLAN.md` 片段、源码、测试和项目说明；建立当前行为与任务规格之间的差距清单。
4. 若发现会阻止正确实现的具体前置缺陷：先判断能否作为当前任务的一部分完整修复；若必须新增独立前置任务，则以最小数量写入 `TODO.md` 正确位置、补明依赖，必要时更新阶段计划，提交后立即停止。
5. 若无上述阻塞，按任务规格完整实现；使用小而聚焦的补丁，修复同一根因影响的整类问题，并同步补充或调整测试与必要文档。
6. 每完成一个关键阶段或计划发生变化，更新本文件的“进度与决策记录”，使实际执行状态可核查。
7. 按规定顺序验证：先 `cargo fmt`，再 `cargo clippy --all-targets -- -D warnings`，最后运行任务指定测试及不超过 30 分钟的完整测试套件；任何未明确排期的失败都必须在当前任务修复或作为最小前置/跟进任务写入 `TODO.md`，不能忽略。
8. 验证通过后，在 `TODO.md` 的任务标题前添加 `[DONE]` 并填写准确完成记录；只有阶段级顺序、依赖、假设或完成条件变化时才更新 `PLAN.md`。
9. 复查差异、状态和任务边界，创建清晰的单次任务 Git 提交；确认提交成功后停止，不开始下一个任务。

## 当前假设与验证原则

- 尚未读取 `TODO.md`，因此当前任务编号、具体实现内容和专用验收命令待步骤 1 确认。
- 若项目并非 Rust 项目，将以 `TODO.md` 和仓库实际工具链为准，但仍遵守“格式化/静态检查先于完整测试”的顺序。
- 不使用缩窄测试形状、改变预期表示、任务私有特判、兼容垫片等方式规避规格缺口。
- 不会把仅填写了完成记录、但标题没有 `[DONE]` 的任务视为完成。

## 进度与决策记录

- 2026-07-13：已建立本次调用的初始执行计划；下一步是读取 `TODO.md`，确定唯一当前任务。
- 2026-07-13：已读取 `TODO.md` 并确定第一个未完成任务为 `M1-R [TODO] Milestone 1 Review`。本次仅核对并修复 Milestone 1 的完整性：serde round-trip、Normalized/Usage/ContentBlock 规格一致性、Message 无 id、A/B/C 逃生舱、公共文档，以及 build/test/doc/lint 全绿；不会进入 `M2-1`。
- 下一步：检查工作区与最新提交，识别是否为中断续做及是否存在最新提交明确留下、且直接关联 M1-R 的问题；随后逐项审阅 M1 源码和测试。
- 2026-07-13：工作区除本计划外原本清洁；最新提交 `39eee6a [M1-6] Implement provider extras` 未声明相关未完成问题，因此无需新增前置任务，也不是代码续做。
- 2026-07-13：已逐项对照 `DESIGN.md` §4/§5、`PLAN.md` 已定决策和全部 M1 源码。核心实现满足要求：cache/reasoning 单列、thinking signature 保留、Message 明确无 id、Usage/ContentBlock flatten extra 生效、ProviderExtras 与 Normalized 就位。
- 评审发现并纳入 M1-R 的可直接修复项：根 `README.md` 缺失；`Role`、`StopReason`、`ProviderId` 虽已有 round-trip，但未覆盖各枚举变体。计划补充 README（概览/安装/当前 API 示例/验证命令）、扩充枚举 serde 测试、启用 `missing_docs` lint，并为生产私有辅助函数补足用途注释。完成后按 fmt → clippy → build/test/doc 顺序验证。
- 2026-07-13：上述评审修复已实现；`cargo fmt --all` 与 `cargo clippy --all-targets -- -D warnings` 均通过。严格 lint 同时验证了所有当前公共 API 均具备文档注释。下一步运行 build、完整测试套件与 warnings-as-errors 的 rustdoc。
- 2026-07-13：验证全部通过：`cargo build --all-targets`、带 1800 秒上限的 `cargo test --all --all-targets`（30 passed）、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`、`git diff --check`；源码无遗留 TODO/FIXME/XXX/HACK（README 对 `TODO.md` 的正常链接除外）。
- 2026-07-13：已将 `M1-R` 标记为 `[DONE]` 并填写完成记录；`PLAN.md` 的阶段顺序、依赖、假设和完成条件均未变化，因此不更新。下一步只做最终格式/差异/状态复核并提交 `[M1-R]`，提交后停止。
- 2026-07-13：最终复核通过，已创建 `[M1-R] Complete milestone 1 review` 任务提交；本条进度记录将 amend 进同一提交。此后只确认最终提交与工作区清洁，不开始下一个任务。
