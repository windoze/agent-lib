# M2-2 — 用相位 match 消灭 resume/pivot/abandon 的 scratch 对齐防御

**当前执行 TODO.md 第一个未完成任务 = M2-2**（M1-1~M1-5、M2-1 已 DONE）。
前置依赖 M2-1 已完成:两个游离 Option 已收敛为单个 `scratch: TurnScratch`,
`resume_reconfig` 走 `take_pending_reconfig()`,`fold_llm_response` 走 `in_flight()`,
`matches_cursor` 已实现但标 `#[allow(dead_code)]`。

## 关键认知(已核对源码)
- M2-1 已把 accessor 转换做完:`resume_reconfig`(mod.rs:754)已用 `take_pending_reconfig()`
  且保留 "reconfig resume with no deferred reconfiguration in flight" 文案;
  `fold_llm_response`(mod.rs:606)已用 `in_flight().map(..)` 且保留
  "missing in-flight assistant message id for the LLM response" 文案。
- `inject_pivot`(mod.rs:463)只判 cursor 相位(StreamingStep + requirement_id),从不摸 scratch。
- 故 M2-2 的实质增量 = 把 `matches_cursor` 不变量接入 debug_assert!,并去掉 dead_code allow。

## 做什么(编辑清单,mod.rs)
1. 去掉 `matches_cursor` 的 `#[allow(dead_code)]`(现 line 154);rustdoc 保留"wired into
   debug_assert!s on the resume/pivot/abandon paths"。
2. `resume()`(line 544)顶部插:
   `debug_assert!(self.scratch.matches_cursor(self.state.loop_cursor()), "...")`
   —— 覆盖 resume_llm/fold_llm_response、resume_tool、resume_approval、resume_reconfig 分派。
3. `abandon()`(line 793)顶部插同样的 `matches_cursor` debug_assert
   —— 覆盖 abandon_llm_step/abandon_tool_phase/abandon_reconfig。
4. `inject_pivot()`(line 463)在 cursor 相位判定通过后插:
   `debug_assert!(self.in_flight().is_some(), "...")`
   —— 记录 "StreamingStep ⇒ scratch=InTurn ⇒ in_flight() 必 Some" 不变量。

## 不变量正确性核对
- resume/abandon 入口:每个 cursor 相位下 scratch 相位与之同构(open_user_turn→InTurn,
  emit_reconfig_effect→Reconfig,finish_cancel/fail→None);Idle/Done/Error/CancelRecovery→None,
  matches_cursor 允许。during-turn Commit reconfig 已在 emit_reconfig_effect 把 InTurn 覆盖为
  Reconfig 后才 transition 到 AwaitingReconfig,入口处一致。
- inject_pivot:cursor==StreamingStep ⇒ scratch==InTurn ⇒ in_flight().is_some()。

## 文案/语义约束
- 不改任何对外错误文案;不改 serde 形状(cursor.rs 不动)。
- debug_assert 仅 debug 构建生效,release 语义零变化。

## 验证序列(1–6)
1. cargo fmt --all -- --check
2. cargo test -p agent-lib --lib agent::machine::default(聚焦,尤其 reconfig/pivot/tool)
3. cargo clippy --all-targets -- -D warnings
4. cargo test --all --all-targets(<=30min)
5. RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
6. git diff --check;确认仅 mod.rs 改动,cursor.rs 未触及

## 完成后
TODO.md M2-2 标 [DONE] + 完成记录;commit [M2-2] ...;停。

## 进度
- [x] 编辑 mod.rs(去 allow + 3 处 debug_assert:resume/abandon/inject_pivot)
- [x] fmt / 聚焦测试(39) / clippy / 全量(36 bin 全绿) / doc / diff 全过
- [x] TODO.md 标 DONE + 完成记录(M2-3 heading 误删后已恢复)
- [ ] commit;停
