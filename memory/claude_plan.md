# 当前任务：M6-R Milestone 6 Review

## 定位
- `TODO.md` 第一个未完成任务 = **M6-R**（line 1342，标题 `[TODO]`）。M6-1..M6-4 已 `[DONE]`（HEAD=c890ad7 [M6-4]）。
- 前置依赖 M6-1..M6-4 全部完成。无阻塞。
- 未追踪文件 `docs/external-agent.md` = 无关的 External Agent 设计草案，TODO/PLAN 未引用，不纳入本次提交。

## 任务要求（TODO.md M6-R）
做什么：
- 对比迁移前后重复 fake 数量、测试行数、可读性变化。
- 核对 Core Rust / Scripted Scenario / Recorded Replay 三类覆盖矩阵。
- 核对所有 replay 测试 CI 离线可跑。
- 更新 docs/TESTABILITY.md 现状描述。
验证：全套验证命令通过；Review 结论写入完成记录。

## 关键发现（评审判断）
- §8.1 Core Rust Suites 规划 8 套，M6-3 仅调度并交付 5 套（step/tool/interaction/driver/trace_budget）；
  pivot/reconfig/cancel 3 套未调度。§7 明言“不要求一次性全部迁移”，非 spec 违背，属已知 deferred。
- §8.2 Scripted Scenario Suites 非 M6 任务范围（M6 只做迁移+基础 coverage+cassette replay）；未交付，deferred。
- §8.3 Recorded Replay：M6-4 交付 text/tool/approval 三套 + M3-4 cassette_replay。

## 执行步骤
1. [进行中] 全套验证：fmt --check → clippy -D warnings → 聚焦 replay 离线 → cargo test --all --all-targets → doc → diff check
2. 定量核对：test 文件行数、删除的 fake 类型数（引用完成记录 + 现网 grep 佐证）
3. 覆盖矩阵核对：现存 suite ↔ §7/§8.1/§8.3 行；记录 §8.1 3 套 + §8.2 未交付的 deferred 缺口
4. 离线可跑核对：无 env 变量运行 replay suites
5. 更新 docs/TESTABILITY.md 现状（§8.1 现状块 + §8.2 deferred 说明 + M6 里程碑小结）
6. TODO.md M6-R 标 [DONE] + 写完成记录
7. commit [M6-R]，停止

## 校验顺序
fmt --check → clippy(-D warnings) → replay 离线 → cargo test --all --all-targets(≤30min) → RUSTDOCFLAGS=-D warnings doc → git diff --check → 文档更新 → TODO [DONE] → commit

## 完成
- [x] 全套验证全绿(fmt/clippy/601 tests/doc/diff-check)
- [x] 迁移前后对比 + 覆盖矩阵核对 + replay 离线核对
- [x] docs/TESTABILITY.md 现状更新(§8.1/§8.2/§8.3)
- [x] TODO.md M6-R 标 [DONE] + 完成记录
- [ ] commit [M6-R] 后停止 ← 进行中
