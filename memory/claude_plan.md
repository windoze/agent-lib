# 执行计划：M3-9 M3 review — Conversation 正确性收口

## 任务识别

TODO.md 中第一个未完成任务为 **M3-9 [TODO] M3 review：Conversation 正确性收口**（M1、M2 全部 [DONE]；M3-1 ~ M3-8 全部 [DONE]）。这是一个 review 任务（纯审查，预期无代码改动）。

## 检查项（来自 TODO.md）

1. 逐条核对 H-STATE-1/2、M-CONV-1/2/3/5/6/7 状态，`docs/review-2026-07.md` 已标注。
   - 注意：M-CONV-3 按既定口径留待 M3-9 标注（M3-5-1~4 已全部落地，本次应标注 `✅ 已修复`）。
   - 重点复验演进场景二次导出不冲突（M3-5-3 的 diff 代次键测试）。
2. 重点复验（跑定向测试）：
   - M3-1 回归测试：`apply_compaction_rejects_a_reverted_head_and_redo_keeps_every_turn`
   - rows round-trip 含 meta：`rows_round_trip_preserves_injected_user_message_meta`
   - 10 万级链不栈溢出：`parent_graph_validation_handles_a_long_chain_iteratively` 等 4 条
   - M3-5 演进场景：`insert_set_against_*` 系列
3. `docs/conversation-core.md` 与实现一致（抽查 M3 各任务新增段落）。
4. 全量门禁命令通过：
   - `cargo fmt --all`
   - `cargo clippy --all-targets -- -D warnings`
   - `cargo clippy --all-targets --features "external-claude-code external-codex external-opencode external-acp" -- -D warnings`
   - `cargo test --all --all-targets`
   - `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`

## 执行步骤

1. 读 `docs/review-2026-07.md` 中 H-STATE-1/2、M-CONV-1/2/3/5/6/7 八条的标注状态；标注 M-CONV-3。
2. 代码点位抽查确认修复在场（config 校验、generation 列、预检、resolver、fork 文档等）。
3. 跑定向测试复验重点项。
4. 抽查 `docs/conversation-core.md` 与实现一致性。
5. 全量门禁（fmt → clippy → clippy+features → test → doc）。
6. TODO.md 标记 M3-9 [DONE] + 完成记录。
7. 提交 git commit（[M3-9] ...）。

## 执行结果（已完成）

- `docs/review-2026-07.md` M-CONV-3 已标注 `✅ 已修复（M3-5）`；其余七条此前已标注，核对无误。
- 代码点位抽查：八条修复全部在场（CompactionOnRevertedHead、MessageRecord.meta、generation 列/schema v3、PairingProviderIdResolver、validate_assistant_blocks、check_parent_chain + 手工 Drop）。
- 定向复验：`cargo test -p agent-lib --lib conversation::` 177 条全过（0.63s），含 M3-1 回归、meta round-trip、10 万链 4 条、M3-5 演进 5 条、M3-6/M3-7 测试。
- `docs/conversation-core.md` 抽查与实现一致。
- 全量门禁：fmt、clippy（默认 + external features）、`cargo test --all --all-targets`（exit 0，约 32s）、`cargo doc` 全部通过。
- TODO.md M3-9 已标 [DONE] 并附完成记录。纯审查任务，无代码改动。
- 下一步：git 提交后停止。
