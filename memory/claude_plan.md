# 执行计划

## 当前约束

- 输出使用中文。
- 以 `TODO.md` 为任务顺序和完成状态的唯一权威来源。
- 本轮只完成第一个标题未带 `[DONE]` 的任务，然后停止。
- 不做开放式历史问题清扫；只有阻塞当前任务或测试失败策略要求时才处理或登记问题。
- 完成后需要更新 `TODO.md` 的任务标题和完成记录，必要时才更新 `PLAN.md`。
- 修改代码后按顺序运行 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets`，完整测试超时不超过 30 分钟。
- 最终需要提交 Git commit。

## 步骤计划

1. 读取 `TODO.md`，找到第一个标题未带 `[DONE]` 的任务。
2. 查看最新提交信息，判断是否明确提到与该任务直接相关的未完成问题。
3. 读取当前任务涉及的设计文件和源码上下文，只收集实现该任务所需的信息。
4. 如发现当前任务被具体前置问题阻塞，则在 `TODO.md` 插入最小必要前置任务并停止；否则直接实现当前任务。
5. 增加或调整聚焦测试，覆盖任务要求和风险点。
6. 运行格式化、严格 lint、完整测试；如出现未安排的失败，修复或按策略登记前置任务。
7. 更新 `TODO.md`：给完成的任务标题加 `[DONE]`，补充完成记录、验证命令和结果。
8. 检查工作区 diff，确认没有误改无关内容。
9. 用清晰任务消息提交所有本轮相关改动。
10. 停止，不继续处理下一个任务。

## 进度记录

- 已写入初始计划。
- 已读取 `TODO.md` 任务标题列表；首个未完成任务是 `M4-3 [TODO] 原子 apply_compaction 与 tiered/consolidated 更新`。
- 已查看最新提交：`8ce12f3 [M4-2] Implement effective conversation view`；该提交直接完成前置任务，未从提交标题/stat 中发现需要先处理的相关未完成事项。
- 已读取 `M4-3` 任务正文、`PLAN.md` 和 `docs/conversation-core.md` 中 projection/compaction 相关章节。
- 已阅读 `src/conversation/projection`、`Conversation`、`Boundary`、`History` 和 `ProjectionError` 的现有实现。
- 实施计划细化：
  1. 新增纯数据 `CompactionPlan`、`CompactionStep`、`CompactionTarget`，plan 记录 conversation id、structural version、head 和外部生成 artifact 列表。
  2. 在 `Conversation::apply_compaction(&CompactionPlan)` 中实现两阶段验证与原子 projection 替换；成功后推进 structural version，使旧 plan/boundary token 失效。
  3. 支持 raw target（可切 raw span）和 spans target（必须对齐现有 span 边界），从而覆盖首次压缩、tiered raw tail 和 consolidate summary-of-summaries。
  4. 保留当前 projection 中仍属于当前 head 的旧 artifacts；consolidate 替换掉的 artifact 作为未引用 provenance/audit 数据继续留在 projection artifacts 中。
  5. 调整 commit 后 projection 维护：提交新 turn 时保留当前有效 overlay 并追加 raw tail，避免已应用 compaction 在下一次 commit 后被重置为 all-raw。
  6. 增加 M4-3 聚焦测试，包括正向 compaction 场景、pending/stale/mismatch/overlap/partial compacted span 等负向原子性，以及 raw turns/message payload 不变。
  7. 更新 `TODO.md` 完成记录、README/rustdoc 当前能力描述，随后执行格式化、clippy、聚焦测试、全量测试、rustdoc、diff check 并提交。
- 已实现初版 compaction 数据模型、`Conversation::apply_compaction`、commit 后 overlay 保留逻辑和 M4-3 聚焦测试。
- 已运行 `cargo fmt --all` 成功。
- 已运行 `cargo test conversation::projection -- --nocapture` 成功：19 passed。
- `cargo clippy --all-targets -- -D warnings` 首次发现测试 helper 类型复杂度问题，已用测试内 type alias 修正。
- 已重新运行 `cargo fmt --all` 成功。
- 已重新运行 `cargo clippy --all-targets -- -D warnings` 成功。
- 已按验证顺序重新运行聚焦测试：`cargo test conversation::projection -- --nocapture`，19 passed。
- 已运行 1800 秒硬上限内完整测试：`perl -e 'alarm shift; exec @ARGV' 1800 cargo test --all --all-targets`，263 个库测试与 3 个离线集成测试 passed，7 ignored，examples 0-test targets passed。
- 已运行 `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` 成功。
- 已运行 `git diff --check` 成功。
- 已额外运行 `cargo test --doc` 成功：1 个正向 doctest 与 10 个 compile-fail doctest passed。
- 已更新 `TODO.md`：M4-3 标题改为 `[DONE]`，并补充完成记录；未修改 `PLAN.md`。
- 已在 TODO 更新后重新运行 `git diff --check` 成功；最后只改 Markdown，因此不重跑 Rust 套件。
- 审查时补充了 `CompactionPlan` serde round-trip 断言。
- 补充测试后重新完成验证链：`cargo fmt --all`；`cargo clippy --all-targets -- -D warnings`；
  `cargo test conversation::projection -- --nocapture`（19 passed）；
  `perl -e 'alarm shift; exec @ARGV' 1800 cargo test --all --all-targets`
  （263 个库测试与 3 个离线集成测试 passed、7 ignored、examples 通过）；
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`；`git diff --check`；
  额外 `cargo test --doc`（1 个正向 doctest 与 10 个 compile-fail doctest passed）。
- 下一步查看工作区并提交本轮改动。
