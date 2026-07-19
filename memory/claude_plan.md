# 执行计划

## 范围

- 以 `TODO.md` 为唯一任务排序来源，当前第一个未完成任务是 `M6-1 [TODO] drain/drive_turn 接入预算记账（M-PROM-1 核心）`。
- 本轮只完成 M6-1；完成实现、验证、记录并提交后停止，不推进 M6-2。
- 若发现 M6-1 被具体前置问题阻塞，则在 `TODO.md` 插入最小必要前置任务，提交后停止。

## 步骤

1. 检查最近提交是否明确提到与 M6-1 直接相关的未完成问题。
2. 阅读预算上下文、drain/streamed drive、默认机器终止语义及相关文档，确定生产接线点。
3. 实现预算记账：在 step boundary 计步，在 LLM response usage 可得处计 usage，并把超限映射到既有预算终止语义。
4. 补充测试：小预算超限终止且会话一致；预算充足时累计记账等于各步 usage 之和。
5. 同步预算非原子窗口选型与 effect-model/agent-layer 文档；标注审查条目中 M-PROM-1 核心部分。
6. 运行 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、相关定向测试、全量测试和 rustdoc；如出现未排期失败，修复或排入必要前置任务。
7. 将 `TODO.md` 的 M6-1 标记为 `[DONE]` 并写完成记录，检查 diff 后提交全部相关改动。

## 进度

- 已读取 `TODO.md`，确认 M6-1 是当前第一个未完成任务。
- 已写入本轮执行计划。
- 最近提交为 `[M5-7] Review facade commitments`，未发现提交标题中明确声明需在 M6-1 前处理的额外未完成问题。
- 已阅读预算、drive/streamed drive、默认机器 cursor 语义：`charge_*` 只在 external/test 使用，默认 drive 尚未接线；`BudgetExceeded`/`BudgetExhausted` 仍无生产构造点。
- 实现选型：在 `AgentMachine` trait 增加预算中断 hook；drive 在新的 LLM spend 前做 exhausted 预检，LLM 成功返回后依次 `charge_step` 与 `charge_usage`，超限时将未恢复 requirement 记为 `NeverResumed` 并让机器停在 `Done(BudgetExhausted)`。该方案保留现有 BudgetHandle 非预扣模型，并在文档中说明预检与实际 charge 之间的非原子窗口。
- 已实现预算接线与预算中断 hook：`drain` 与流式 `drive_streamed` 都会在 LLM batch 前预检、在 LLM resolution resume 前记账；默认/external/nested machine 覆盖预算中断，默认机器会丢弃未提交 pending 并停在 `Done(BudgetExhausted)`。
- 已补测试：预算充足时 step/usage 累计；usage 超限时 LLM response 不恢复且 requirement 记 `NeverResumed`；默认机器小 token 预算下当前未提交 turn 被丢弃；subagent shared budget 测试更新为包含 driver 自动 usage charge。
- 定向验证已通过：`cargo fmt --all`、`cargo test -p agent-lib --lib agent::drive`。
- 已同步文档：`docs/agent-effect-model.md`、`docs/agent-layer.md`、`docs/TESTABILITY.md`、`docs/review-2026-07.md`。
- 完整验证已通过：`cargo clippy --all-targets -- -D warnings`、external feature clippy、`cargo test --all --all-targets`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`；期间修正了集成测试/agent-testkit 中旧的手动 LLM usage charging 期望，避免与 driver-level charge 双重计费。
- 已将 `TODO.md` 的 M6-1 标记为 `[DONE]` 并写入完成记录。下一步检查 diff/status 并提交本任务改动。
