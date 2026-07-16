# M1-5 — Milestone 1 review：刀 (C) 正确性与完整性

**当前执行 TODO.md 第一个未完成任务 = M1-5**（M1-1~M1-4 已 DONE）。这是刀 (C) 的里程碑
review 任务，验收核心：「对外行为逐字节不变、噪音显著下降」。仅审阅 + 跑验证 + 补完成记录，
不改运行时代码（除非发现行为漂移）。

## 审阅清单（做什么）
1. 通读 mod.rs / tools.rs，确认：
   - `step()` 是唯一 `StepError → Error` 折叠点（`fail_from`），无残留临时桥接。
   - 纯失败已 `?` 化；带副产品失败保留 `fail_with_notifications` 且 notification 完整。
   - 错误文案与改造前逐字节一致（git 对照）。
2. 统计 `if let Err` 与 `self.fail*` 前后计数，量化噪音下降。
3. 跑完整验证序列 1–6 + 额外全量机器测试。
4. 确认 `git diff --stat` 代码改动只在 `src/agent/machine/default/`。
5. 发现行为漂移 → 记录并修复后再关闭。

## 审阅结论（已核对）
- **fail_from 唯一折叠点**：`step()`@mod.rs:842-845 `match result { Ok=>outcome, Err=>self.fail_from(error) }`；
  `fail_from`@830 = `self.fail(error.message())`；裸 `self.fail(` 仅出现在 fail_from 内部。✅
- **残留 self.fail\* 全为带副产品**：mod.rs 仅 4 处（797 fail_with_notifications 定义体、828 doc、
  830 fail_from、844 step 折叠）；tools.rs 10 处全部 `Ok(self.fail_with_notifications(notifications/vec![finished], ..))`
  且携带此前已发的 notifications。✅
- **残留 if let Err 全为带副产品 cursor transition**：mod.rs 0 处；tools.rs 3 处（281/317/503）均 `return Ok(fail_with_notifications(notifications, "cursor transition failed"))`。✅
- **文案逐字节一致**：baseline(7ee6254) 与 current 的失败字符串集合完全相同（已 diff，24 条全等）。✅
- **改动范围**：代码改动仅 error.rs/mod.rs/tools.rs，全在 src/agent/machine/default/；无 trait/drive/cursor/state 误伤。✅

## 噪音计数（baseline 7ee6254 → HEAD）
| 文件 | self.fail\* | if let Err |
|------|-------------|-----------|
| mod.rs   | 33 → 4  | 10 → 0 |
| tools.rs | 32 → 10 | 8 → 3  |
- self.fail\*：65 → 14（-51，-78%）；if let Err：18 → 3（-15，-83%）。
- 残留均为「带副产品失败」就地折叠，符合 M1-4 设计，非噪音。

## 验证序列（1–6 + 额外）
1. cargo fmt --all -- --check
2. cargo test -p agent-lib agent::machine::default（聚焦）
3. cargo clippy --all-targets -- -D warnings
4. cargo test --all --all-targets（≤30min）
5. RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
6. git diff --check

## 完成后
TODO.md M1-5 标 [DONE] + 完成记录（含计数表 + 行为不变确认）；commit `[M1-5] ...`；停。

## 进度
- [x] 审阅 mod.rs/tools.rs：fail_from 唯一折叠点、残留全带副产品、文案逐字节一致
- [x] 噪音计数（65→14 fail*，18→3 if let Err）
- [x] 验证序列 1–6 + 全量（fmt/聚焦39/clippy/全量全绿/doc/diff 全过）
- [x] TODO.md 标 DONE + 完成记录（含计数表 + 行为不变确认）
- [x] commit；停
