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
