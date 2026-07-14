# 当前任务：M7-R Milestone 7 与 Testability 总 Review

## 定位
- `TODO.md` 第一个未完成任务 = **M7-R**（line 1490，标题 `[TODO]`）。M1..M6 与 M7-1/M7-2 全部 `[DONE]`。
- HEAD=a19fb15 [M7-2]。前置依赖 M7-1..M7-2 已完成，无阻塞。
- 工作树干净，唯一未跟踪文件 `docs/external-agent.md`（无关 External Agent 草案，TODO/PLAN 未引用，不纳入提交）。
- 这是 Review 任务：不做代码功能改动，除非 review 发现 spec 偏差/失败测试需修复或新增前置任务。不得拆分 review 任务。

## 任务要求（TODO.md M7-R）
做什么：
- 回溯 `PLAN.md`、`TODO.md`、`docs/TESTABILITY.md`。
- 确认 testkit 没有引入 provider wire mock。
- 确认基础 Rust suites 与 recorded replay suites 默认离线可跑。
- 确认 cassette 脱敏与 update 护栏有效。
- 确认 scenario DSL 是否足以作为未来 TS/NAPI 输入；若不足，列出缺口。
- 总结是否仍无需拆 trait crate；若 Cargo 拓扑证明需拆，提出单独后续计划。

验证：
- 全套验证命令全部通过。
- 总 Review 结论与后续项写入完成记录。

## Review 核查清单（逐条取证）
1. [ ] 无 provider wire mock：grep testkit 源码无 reqwest/hyper/http/sse/base_url/headers/auth 传输层 mock。
2. [ ] 离线可跑：core suites 与 recorded replay 默认不触网；replay 默认 skipped/opt-in。
3. [ ] cassette 护栏：record/update 需显式 env opt-in；writer 经 redactor；verify 不写盘。
4. [ ] 负例覆盖仍在：UnhandledRequirement / misaligned / cancel never-resume。
5. [ ] scenario DSL 对 TS/NAPI 充分性评估 + 缺口清单。
6. [ ] trait crate：cargo metadata 拓扑分析，是否仍无需拆。
7. [ ] 文档/README/PLAN/TODO 与代码一致。

## 验证顺序
- `cargo fmt --check`
- `cargo clippy --all-targets -- -D warnings`
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`
- `cargo test --all --all-targets`（<=30min timeout）
- `git diff --check`

## 进度
- [进行中] 撰写本计划、执行 review 取证

## 结果（M7-R 完成）
- Review 六项全部核实通过：无 provider wire mock / 默认离线 / cassette 脱敏+update 护栏 / 负例保留 /
  scenario DSL 为 TS-NAPI seam（列缺口）/ 仍无需拆 trait crate。
- 发现并修复唯一验证门失败：prelude.rs:6 rustdoc redundant_explicit_links（M7-2 因 doc fingerprint 缓存漏检）。
  改裸链接 [`TestScope`]。顺带更新 TESTABILITY.md 状态横幅为「M1–M7 全部完成」。
- 验证全绿：fmt --check / clippy -D warnings / RUSTDOCFLAGS=-D warnings doc（强制重跑 testkit）/
  cargo test --all --all-targets（24 suite,609 passed,0 failed,0 ignored 实跑）/ git diff --check。
- TODO.md 30/30 [DONE]。M7-R 标 [DONE] + 完成记录已写。
- 收尾：commit [M7-R]，创建 endtag（所有任务完成）。
