# 当前任务：M7-2 文档、README 与开发指南更新

## 定位
- `TODO.md` 第一个未完成任务 = **M7-2**（line 1443，标题 `[TODO]`）。M1..M6 与 M7-1 全部 `[DONE]`。
- HEAD=df2ac70 [M7-1]。前置依赖 M7-1 已完成，无阻塞。
- 未追踪文件 `docs/external-agent.md` = 无关 External Agent 草案，TODO/PLAN 未引用，不纳入本次提交。

## 任务要求（TODO.md M7-2）
做什么：
- 更新 `docs/TESTABILITY.md`，把已落地模块从规划改为当前状态。
- 更新 `README.md` 当前计划链接，说明当前根 `PLAN.md`/`TODO.md` 是 Testability 阶段。
- 给 `crates/agent-testkit` 添加 crate-level rustdoc，包含 quickstart 示例。
- 记录 cassette record/update 环境变量。
- 记录“不 mock HTTP provider”的边界。

验证：
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` 通过。
- README archive/current plan 链接有效。
- 全套验证命令全部通过。

## 关键事实
- cassette 环境变量：`AGENT_TESTKIT_RECORD_CASSETTES=1`（record）、`AGENT_TESTKIT_UPDATE_CASSETTES=1`（update）；
  常量 `RECORD_ENV_VAR`/`UPDATE_ENV_VAR`。Verify 模式不写盘。§8.3 已详述。
- agent-testkit 模块已全部落地：ids/fixtures/script/handlers/cassette/scope/machine/harness/assertions/
  concurrency/subagent/scenario/prelude（源在 crates/agent-testkit/src）。
- README 现有链接全部有效；archive 目录含 agent-layer/client-layer/conversation/agent-effect-migration。
- lib.rs 已有 crate-level doc（Boundaries + Module map），缺 Quickstart 示例与 cassette env 说明。
- crate 内仅 assertions/mod.rs 有一个 `no_run` doctest；agent-lib 是普通依赖，doctest 可 `use agent_lib::...`。

## 编辑计划
1. `crates/agent-testkit/src/lib.rs`：新增 `# Quickstart`（`no_run` doctest，基于 drain+ScriptedLlmHandler，
   编译通过）；新增 `# Recording cassettes` 说明 record/update env 护栏；强化“不 mock provider wire”边界。
2. `docs/TESTABILITY.md`：line 3 状态横幅改为“部分落地/M7 进行中”；§5 顶部加“落地状态”映射表；
   §9 迁移计划标注各 phase 已落地；§13/§8.4 注明 scenario model 草案（M7-1）已落地。
3. `README.md`：line 75 与 150-152 更新为“当前根 PLAN.md/TODO.md 属 Testability 阶段”，
   补 docs/TESTABILITY.md 与 agent-effect-migration archive 链接。

## 验证顺序
- `cargo fmt` + `cargo fmt --check`
- `cargo clippy --all-targets -- -D warnings`
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p agent-testkit`
- `cargo test -p agent-testkit --doc`（doctest 编译）
- `cargo test --all --all-targets`（<=30min timeout）
- README 链接有效性脚本
- `git diff --check`

## 进度
- [完成] 编辑三处文件
- [完成] 验证全绿（fmt/clippy/doc/doctest/链接/diff-check）
- [完成] TODO.md M7-2 [DONE] + 完成记录
- [进行中] commit [M7-2] 停止
