# 当前执行计划

说明：本文件记录可审查的执行计划、关键决策和进度，不包含隐藏推理细节。

1. 读取 `TODO.md`，按标题是否带 `[DONE]` 判断首个未完成任务。
2. 查看最新提交摘要，只在其明确提到且直接影响当前任务的未完成事项时纳入当前任务或作为前置任务记录。
3. 阅读当前任务相关的源码、测试和文档，确认任务边界、依赖、验收要求。
4. 以最小正确变更实现当前任务；如遇阻塞当前任务的规范不匹配或测试失败，优先修复，或在 `TODO.md` 中添加最小前置任务并停止。
5. 按要求运行格式化、lint 和相关/完整测试；所有新观察到且未排期的失败必须修复或排期。
6. 更新 `TODO.md`：完成时给任务标题加 `[DONE]` 并填写完成记录；如仅排期阻塞项，则保持当前任务未完成。
7. 仅在阶段级计划改变时更新 `PLAN.md`。
8. 提交所有本次任务相关变更，然后停止，不继续下一个任务。

进度：已识别首个未完成任务为 `M4-7 [TODO] M4 review：Agent 语义收口`。最新提交 `M4-6` 直接关联当前 review 范围，需纳入复核。当前计划调整为：核对 M4-1~M4-6 完成记录与实现/文档状态，补齐 `docs/review-2026-07.md` 中未标注的 M4 条目，运行全量门禁，最后更新 `TODO.md` 的 M4-7 完成记录并提交。

进度：已核对 M4 主要实现锚点（协作读工具、AwaitingReconfig 拒绝、pivot trace 派生 id、软拒绝、取消双观测点与 TurnDone.cancelled、resolver 单一来源与 fail-closed 默认）均在源码与测试中存在。已补齐 `docs/review-2026-07.md` 的 M4 状态标注，下一步运行格式化、lint、测试和文档门禁。

进度：验证全部通过：`cargo fmt --all`、默认 clippy、external/acp feature clippy、`cargo test -p agent-lib --lib agent::`、`cargo test --all --all-targets`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`。已将 `TODO.md` 的 `M4-7` 标记为 `[DONE]` 并写入完成记录。下一步检查 diff/status 后提交本任务变更并停止。
