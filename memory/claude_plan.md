# 执行计划 — M1-1 建立 `agent-testkit` 拓扑与最小 crate 骨架

## 选中的任务
`TODO.md` 第一个未完成任务 = **M1-1**(全部任务均为 `[TODO]`;这是接续已归档的 Agent Effect
迁移的新任务列表)。HEAD=b4e9b2d,工作树 clean。非 Review 任务,不拆分。

## 目标(TODO.md M1-1)
1. 新建 `crates/agent-testkit/` skeleton:`Cargo.toml`、`src/lib.rs`、`src/prelude.rs`。
2. root `Cargo.toml` 增加 `[workspace]`,members = `.` + `crates/agent-testkit`;若拓扑破坏
   root package 测试,记录原因并改用过渡方案。
3. testkit 依赖 `agent-lib = { path = "../.." }`,复用 async-trait/futures/serde/serde_json/
   tokio/uuid;不引入 mockall/proptest/insta。
4. lib.rs 预声明模块:ids/fixtures/script/handlers/cassette/scope/machine/harness/
   assertions/concurrency/subagent/prelude(除 prelude 外可空 stub)。
5. 增加 smoke test,证明 testkit 能引用 `agent_lib::agent::AgentMachine` 等公开类型。

## 拓扑决策
- Rust 1.97,edition 2024 → workspace resolver "3"。
- root package 仍是 member;testkit 只 dep agent-lib(不反向 dev-dep),无 Cargo 周期。
  → 直接采用首选 `crates/agent-testkit` 方案,无需过渡目录。

## 步骤
1. [ ] root Cargo.toml 加 `[workspace]`(members + resolver="3")。
2. [ ] 创建 crates/agent-testkit/{Cargo.toml, src/lib.rs, src/prelude.rs, 各模块 stub, tests/smoke.rs}。
3. [ ] fmt → clippy(-D warnings)→ cargo test -p agent-testkit → cargo test --all --all-targets
       → RUSTDOCFLAGS=-D warnings cargo doc --no-deps → git diff --check。
4. [ ] TODO.md 把 M1-1 标 `[DONE]` + 写完成记录。
5. [ ] 提交,停止。

## 进度/发现
- 完成 M1-1。采用首选 `crates/agent-testkit` 拓扑(无 Cargo 周期,root 测试正常)。
- workspace resolver="3";testkit 单向 dep agent-lib;未引入 mockall/proptest/insta。
- 11 模块 stub + prelude re-export + smoke test(泛型约束证明 DefaultAgentMachine: AgentMachine)。
- 全套验证绿:fmt / clippy -Dwarnings(两 crate)/ test -p agent-testkit(2)/ test --all(0 failed)/
  doc -Dwarnings(agent-lib 与 -p agent-testkit)/ diff --check。
- 备注:cargo doc --no-deps 默认只文档化 root package;额外 -p agent-testkit 验证 testkit rustdoc。
- 已标 TODO.md M1-1 [DONE] 并写完成记录。PLAN.md 无需改(拓扑符合首选,最终理由归 M1-R)。
