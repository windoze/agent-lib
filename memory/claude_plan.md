# 当前任务：M4-R Milestone 4 总 Review

## 定位
- `TODO.md` 第一个未完成任务 = **M4-R**（行 971，唯一 `[TODO]`）。M4-1..M4-4 均 `[DONE]`（HEAD=83067f8 [M4-4]）。
- 工作树干净。这是收尾 review 任务：核对复杂 mock 测试设计已按计划落地，无 flakiness / 过度抽象。

## Review 检查项（来自 TODO.md M4-R「做什么」）
1. P0 场景全部落地：P0-1 主 flow、P0-2 subagent、P0-3 cancel。
2. P1 场景落地或明确 deferred：P1-1 claim conflict/dependency block、P1-2 cancel 区分、P1-3 pivot 后 subagent brief。
3. 所有复杂测试可单独运行，文件命名与 `docs/complex-tests.md` 一致。
4. 无真实 sleep / 网络 / credentials / provider wire mock。
5. failure diagnostics 含 store ops / handler log / role sequence / outstanding ids。
6. helper 未扩张成通用 DSL；三处以上复用的 helper 记录是否后续提到 agent-testkit。

## 验证门（全部必须跑）
- cargo fmt --all -- --check
- cargo clippy --all-targets -- -D warnings
- cargo test --test agent_complex_support
- cargo test --test agent_complex_flow
- cargo test --test agent_complex_subagent
- cargo test --test agent_complex_cancel
- cargo test --all --all-targets
- RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
- git diff --check

## 完成
- Review 结论写入 M4-R 完成记录；TODO.md M4-R [TODO]->[DONE]；提交 `[M4-R] ...`；停止。
- 全部任务完成后：最终 review + 打 tag `endtag`。

## 进度
- (进行中) 已定位 M4-R。开始代码核对 + 启动验证门。

## 进度更新（M4-R 完成）
- 全部验证门通过：fmt OK、clippy exit0、support=10/flow=4/subagent=2/cancel=2、`cargo test --all --all-targets` 全绿、doc exit0、git diff --check 干净。
- Review 结论：P0/P1 全落地无 deferred；无真实 sleep/网络/credentials/wire mock；诊断充分；helper 未成 DSL。
- TODO.md M4-R 标 [DONE] 并写完成记录。全部 15 个任务均 [DONE]。
- 下一步：提交 [M4-R]，随后按 Completion & Release 打 tag `endtag`。
