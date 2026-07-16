# M1-3 — 在 `step()` 最外层收敛错误折叠(`fail_from`)

**当前执行 TODO.md 第一个未完成任务 = M1-3**（M1-1、M1-2 已 DONE）。刀 (C) 第三步：
在 `src/agent/machine/default/mod.rs` 新增 `fail_from`，把 `step()` 改成唯一折叠点，
并把 M1-2 埋下、标注 `// M1-3 will replace with fail_from` 的三处临时桥接接到 `fail_from`。

## 做什么
1. mod.rs 新增 `fn fail_from(&mut self, error: StepError) -> StepOutcome`：
   复用 `fail()` 收尾（discard pending → cancel_pending(DiscardTurn) → 清 in_flight →
   迁 LoopCursor::Error → quiescent），文案取 `error.message()`。等价于 `self.fail(error.message())`，
   逐字节不变。ErrorCursor 仍只带 message（可选分类信息本任务跳过，避免改动现有断言）。
2. step()（mod.rs:823）改成 match 形状：`Ok(outcome) => outcome, Err(error) => self.fail_from(error)`，
   移除 M1-2 临时桥接 `.unwrap_or_else(|e| self.fail(e.message()))`。
3. tools.rs 两处桥接（finish_tool_phase→block_on_llm @552、abandon_tool_phase→finish_cancel @641）
   的 `.unwrap_or_else(|error| self.fail(error.message()))` 换成 `self.fail_from(error)`，
   注释更新为 M1-4 会让该方法返回 Result 并由 step() 折叠。
4. `fail()` / `fail_with_notifications()` 保留不动。

## 边界
- 不改 tools.rs 方法签名（M1-4）；不改 ErrorCursor serde 形状；对外语义零变化。
- tools.rs 仍存在 fail_from 局部折叠，属 M1-4 消除范畴；M1-3 只保证 mod.rs 无临时桥接。

## 验证序列
1. cargo fmt --all -- --check
2. cargo test -p agent-lib agent::machine::default（断言不改）
3. cargo clippy --all-targets -- -D warnings
4. cargo test --all --all-targets（超时上限 30 min）
5. RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
6. git diff --check

## 完成后
TODO.md M1-3 标 [DONE] + 完成记录；commit `[M1-3] ...`；停（不做 M1-4）。

## 进度
- [x] 加 fail_from
- [x] 改 step() match 形状
- [x] tools.rs 两处桥接改 fail_from + 更新注释
- [x] fmt/聚焦(39 passed)/clippy/全量测试/doc/diff 全绿
- [x] TODO.md 标 DONE + 完成记录
- [x] commit `[M1-3] ...`；停