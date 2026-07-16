# M3-5 — Milestone 3 Review(external agent machine sign-off)

**状态:完成(已全绿,待提交)。**

## 目标(TODO.md M3-5)
- 审阅 machine 状态迁移是否穷尽(Idle/AwaitingSession/AwaitingInteraction/Done/Error 无悬空态)。
- 确认 `step` 无 `await`/无 IO(sans-io)。
- 确认清理归属与设计 §6.4 一致(abandon never-resume,清理在 handle 层,不 emit Shutdown)。
- 用 harness 跑完整场景(Start→Paused→Respond→Completed)与取消场景,确认无回归。
- 完整验证序列全绿,`cargo test --all --all-targets` 无回归。
- Review 结论与遗留项写入完成记录。

## 审阅结论(已核对)
- 状态迁移穷尽:`ExternalAgentCursor` 5 态,`step` match 覆盖 External(UserMessage/Pivot)/Resume/Abandon;
  所有 `match` 通配臂(cursor `other`、result `other`、ContentBlock `_`)都是**有意的错误/过滤处理**,
  非悬空态。`requirement()`/`is_terminal()`/`is_idle()` 显式列出所有 variant,无 wildcard。
- sans-io:machine.rs 无 `.await`/`async fn`/`std::{fs,io,net,process}`/`tokio::`/`spawn`/`block_on`;
  `block_on_session` 只是「reify 一个 NeedExternalSession requirement 并 park」的纯函数命名,非真实阻塞。
- 清理归属:abandon 只 `mark_cleanup_required()` + 丢弃悬空 turn + 收敛 Idle,不 emit requirement/Shutdown;
  disposition(`ExternalSessionShutdown`)由 handle 层记 trace,与 §6.4/§10 一致。
- 场景覆盖(已有测试,无需新增):
  - Start→Completed / Start→Failed / Continue:`tests/agent_external_basic.rs`
  - Start→Paused→Respond→Completed:`tests/agent_external_interaction.rs` + 单元
    `external_pause_then_respond_then_complete_commits_the_turn`
  - 取消(never-resume abandon):`tests/agent_external_lifecycle.rs::external_agent_abandon_...` + 3 单元
  - 挂载:`external_agent_mounts_under_nested_machine`

## 发现的遗留项(review finding)
- machine.rs `initial_loop_cursor` rustdoc 旧引用「out of scope until the mount/cleanup work in M3-4」已过时
  (M3-4 已完成且未加 mid-flight restore)。已改写为「mid-flight restore 是超出 milestone 3 的持久化关注点」。
  这是**仅注释**改动,但为稳妥仍跑 clippy + doc + 全量测试。

## 步骤
1. [x] 审阅 machine/state/shutdown/runtime,确认三项不变量。
2. [x] 修正 `initial_loop_cursor` 过时注释。
3. [x] fmt 无差异 / clippy 0 告警 / `cargo test external_agent` 13 过 / 全量 672 passed 0 failed / doc 0 告警。
4. [x] TODO.md 标 [DONE] + 完成记录(review 结论 + 遗留项)。
5. [ ] 提交 `[M3-5] ...`,停止。
