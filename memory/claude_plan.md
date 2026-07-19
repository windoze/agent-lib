# 执行计划

当前调用目标：完成 `TODO.md` 中第一个未完成任务 `M9-4 文档同步与审查报告勾销`，然后停止。

步骤：
1. 检查 `TODO.md`，确定第一个未完成任务及其验证要求。
2. 检查相关代码、文档和测试，只围绕当前任务建立必要上下文。
3. 如发现当前任务被具体前置问题阻塞，按要求在 `TODO.md` 中加入最小前置任务并停止；否则实现当前任务。
4. 运行格式化、lint 和相关测试；如观察到未被排期的失败测试，修复或将其加入正确的 TODO 顺序。
5. 更新 `TODO.md`：给完成任务标题添加 `[DONE]` 并填写完成记录。
6. 仅在阶段计划实际变化时更新 `PLAN.md`。
7. 检查 git 状态与 diff，提交本次任务涉及的全部变更。
8. 提交后停止，不处理下一个任务。

进度：已确认 `M9-4` 是第一个未完成任务；最新提交 `[M9-3] Optimize performance hot spots` 未留下直接相关的未完成事项。审查报告仍有未标最终状态的条目（如 `M-PROM-3`、`M-EXT-2`、`M-ADP-4` 与若干低严重度尾项），下一步逐条核对代码/文档并补齐状态与文档说明。

已完成关键步骤：
- `docs/review-2026-07.md` 已补齐所有未标最终状态条目，并选择保留在 `docs/`、不移动归档。
- `src/lib.rs` crate 文档已去除过时的“预算/多 agent 编排仍是 future layer”措辞，保持与 facade/agent 现状一致。
- `docs/facade-api.md` 与 `AgentRunStream` rustdoc 已声明 stream 的 `!Send` / 运行中 `&mut Agent` 借用限制。
- `docs/managed-external-agent.md` 与 `AGENTS.md` 已补 CLI 环境继承边界，并同步 `external-acp` feature 说明。

下一步：运行 `cargo fmt --all` 与 rustdoc 验证；随后记录 10 条人工抽查结论并更新 `TODO.md` 完成记录。

验证与记录：`cargo fmt --all` 和 `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` 已通过；`TODO.md` 已将 `M9-4` 标为 `[DONE]` 并写入 10 条人工一致性抽查、验证结果与跳过全量测试/Clippy 的理由。

下一步：检查 git diff/status，确认变更范围后提交 `[M9-4] Synchronize documentation review closeout`。
