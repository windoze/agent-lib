# 本轮执行计划

## 约束
- 以 `TODO.md` 为唯一任务排序与完成状态来源。
- 只完成第一个标题未带 `[DONE]` 的任务，完成后提交并停止。
- 若遇到阻塞当前任务的缺陷或未排期失败测试，先修复或在 `TODO.md` 中插入最小必要前置任务并提交停止。
- 不因任务较大而拆分；仅在确有无法一起落地的前置依赖时才最小化拆分。
- 不改动无关用户变更，不回滚未由我产生的工作区内容。

## 步骤
1. 读取 `TODO.md`，定位第一个标题未带 `[DONE]` 的任务，并确认其要求、依赖和验证标准。
2. 检查最近提交信息；若明确指出与当前任务直接相关的未完成问题，则纳入当前任务或作为前置任务记录。
3. 读取与当前任务相关的源码、测试和文档，限定范围内建立实现上下文。
4. 按任务要求做最小正确实现；如果计划发生实质变化，更新本文件。
5. 运行必要验证，顺序优先为 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、相关测试，再根据任务影响决定是否运行完整测试；完整 Rust 测试不超过 30 分钟超时。
6. 若测试失败且未被明确排期，修复失败或在 `TODO.md` 插入最小必要前置任务并停止。
7. 更新 `TODO.md`：给完成任务标题加 `[DONE]`，补充完成记录和验证结果；仅当阶段计划真实变化时更新 `PLAN.md`。
8. 检查 `git status`、`git diff`、最近提交，确认只提交本轮相关改动；如本轮是恢复未提交任务，则按要求包含当前未提交文件。
9. 使用清晰的任务编号提交信息提交变更，然后停止，不继续下一个任务。

## 当前状态
- 已定位第一个未完成任务：`M7-6 [TODO] M7 review：adapter 收口`。
- 最近提交为 `[M7-5] Add ContentBlock unknown fallback`，与当前 M7 review 直接相关；本轮将把 M7-1 ~ M7-5 的收口核对纳入审查。
- 已核对 `docs/review-2026-07.md`：M-ERR-4、M-ADP-1、M-ADP-2 均为 `✅ 已修复`；协议解析边角中空 arguments、Anthropic 可选字段、CLI 非 JSON 噪声、未知 `ContentBlock`、非对象 usage details 均分别标注 M7-4/M7-5 已修复。
- 已抽查代码/测试覆盖点：HTTP 误分类测试、`StreamEvent::Usage` 增量语义文档、OpenAI 缺失 sequence number 测试、M7-4 容错测试、`ContentBlock::Unknown` 兜底测试均在场。
- 已完成 M7 review 全量门禁：`cargo fmt --all`、默认 clippy、external feature clippy、`cargo test --all --all-targets`、rustdoc 全部通过。
- 已将 `TODO.md` 中 `M7-6` 标记为 `[DONE]` 并写入完成记录。
- 提交前检查结果：工作区仅有 `TODO.md` 与 `memory/claude_plan.md` 两个本轮相关改动；准备提交 `[M7-6] Review adapter fixes`。
