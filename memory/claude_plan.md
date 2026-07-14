# 当前任务：M3-R Milestone 3 Review

## 定位
- `TODO.md` 第一个未完成任务 = **M3-R**（行 657，首个 `[TODO]`）。前置 M3-1 / M3-2 均已 `[DONE]`。
- HEAD=fb0e82e（[M3-2]），工作树干净。这是 **review 任务，不拆分、不改被测/生产代码**（除非发现真实 bug）。
- 产出：核对 M3-1/M3-2 覆盖点，跑全部验证命令，把 review 结论写入 M3-R 完成记录并标 `[DONE]`。

## Review 核对清单（来自 TODO M3-R + PLAN Review 标准）
1. child headless interaction 确实 pop 到 parent（agent_complex_subagent）。
2. `plan_claim_first_available` 在 child 场景跳过 dependency-blocked item（跳过 implement，认领 review）。
3. shared store side effect 只发生一次，无 retry/resume 重复写。
4. cancel path 是 abandon/never-resume，不是 wrong-family error 或 panic（agent_complex_cancel）。
5. cancel 后继续新 turn 的断言真实检查 committed conversation。
6. 测试仍只 mock agent effect 边界，无 provider wire mock。
7. 无过早抽象成 DSL。

## 验证命令
- `cargo fmt --all -- --check`
- `cargo clippy --all-targets -- -D warnings`
- `cargo test --test agent_complex_subagent`
- `cargo test --test agent_complex_cancel`
- `cargo test --all --all-targets`（<30min）
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`
- `git diff --check`

## 完成
- 若全部通过且核对无误：TODO.md M3-R `[TODO]`→`[DONE]` + 写 review 结论；提交 `[M3-R] ...`；停止。
- 若发现 bug/spec 偏差：按流程加 prerequisite 任务，不 workaround，提交并停止。

## 进度
- (完成) 核对 7 项 review 点全部通过；全部验证命令绿；M3-R 标 [DONE]，写入 review 结论；提交并停止。
