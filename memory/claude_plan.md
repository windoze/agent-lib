# 执行计划

## 当前任务：M3-7 `resolved_provider_call_id` 按 claimed 排除语义重推导（M-CONV-6）

任务来源：`TODO.md` M3-7。前置状态：M3-6 已完成并提交。

### 问题（来自 TODO.md / docs/review-2026-07.md M-CONV-6）

`src/conversation/history/index.rs:413-433`：`resolved_provider_call_id` 按内容顺序取候选，
而 validation（`validation/pairing.rs:135-163`）保证的是「未被 claimed 的 provider id 中
唯一」。构造场景：同一 call_msg 含 ToolUse A、B + 同一 result_msg 含 A、B result +
一个 pairing 显式声明 B、另一个为 None → index 重推导候选 {A,B}，release 下 `expect`
通过但可能取错，debug 下 `debug_assert!` panic。

本 crate pending 路径总写 `Some`（`pending/turn/tool.rs:272-282`），但 restore 接受外部
快照的 `None` pairing。

### 待办

1. 阅读 `history/index.rs` 的 `resolved_provider_call_id` 及其调用点、`validation/pairing.rs`
   的 claimed 排除逻辑、restore 路径如何使用。
2. 按任务要求选型（推荐评估「restore 时把 `None` 规范化为解析后的 `Some`」单点修复）。
3. 实现 + 消除该路径上的 `expect`/`debug_assert!` 差异行为。
4. 单元测试：A/B 构造场景（手工快照数据）restore，断言解析结果与 validation 语义一致
   且 debug 构建不 panic。
5. `cargo test -p agent-lib --lib conversation::history` 全过；全量门禁。
6. 文档同步（如有）、`docs/review-2026-07.md` M-CONV-6 标注、TODO.md [DONE] + 完成记录；
   提交 `[M3-7] ...`。

### 进度记录

- [x] 读取 TODO.md，确认首个未完成任务为 M3-7
- [x] 探索代码（explore 子代理）：`resolved_provider_call_id`（index.rs:394-434）、
  validation 两趟 claimed 排除（pairing.rs:115-164）、restore→rebuild 流程、测试设施
- [x] 选型：(a) index 重推导复刻 validation claimed 排除。否决 (b)（restore 时把 None
  规范化为 Some）：会改变内存 Turn 的 pairing 内容 → 重导出的 ToolPairingRecord 与旧存
  行同键不同内容 → `insert_set_against` 误报 InsertConflict，破坏 M3-5 的 insert-only
  代次模型；(a) 不动 wire 形状、单点（`turn_locations` 同时覆盖 restore rebuild 与运行时
  `push_committed_turn`）
- [x] 实现：`PairingProviderIdResolver`（每 turn 两趟：显式 id 先 claimed，None 按 pairing
  顺序取唯一未 claimed 锚点匹配）；expect/debug_assert 现在与 validation 保证一致、
  结构性不可达，debug/release 差异行为消除
- [x] 回归测试 `rebuild_replays_the_validators_claimed_exclusion_for_optional_ids`：
  A/B 共锚点 + 显式 claim B + None pairing，已验证对旧代码失败（debug_assert panic）、
  对新代码通过
- [x] `cargo test -p agent-lib --lib conversation::history` 8 条全过
- [x] fmt + clippy（默认 + external features）全过
- [x] `docs/review-2026-07.md` M-CONV-6 标注 ✅ 已修复（M3-7）
- [x] 全量测试套件 exit 0（44s，无挂起）；cargo doc（-D warnings）通过
- [x] TODO.md 标 [DONE] + 完成记录

## 任务完成总结

M3-7 已完成。选型 (a)：index 重推导复刻 validation 的 claimed 排除两趟语义
（`PairingProviderIdResolver`），否决 (b)（规范化 None→Some 会破坏 M3-5 insert-only
代次模型）。回归测试已对拍验证（旧代码 debug panic / 新代码通过）。全量门禁全过。
下一任务：M3-8（fork 不继承 compaction projection 的文档化，纯文档任务）。
