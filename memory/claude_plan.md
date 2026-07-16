# Claude 执行计划

## 当前任务:M2-4 在 testkit 实现 `ScriptedExternalSessionHandler` 与 fixtures

**前置依赖**:M2-3(已 DONE)。

### 目标(设计 §12 / TODO M2-4)
在 `agent-testkit` 扩展 external-agent effect 边界的脚本化测试组件:
- `ScriptedExternalSessionHandler`:按 dispatch 顺序返回预置 `ExternalSessionResult`
  (Completed / PausedForInteraction / Failed),把每次 `fulfill` 的 request 与 result
  记录进 call log;脚本耗尽时折叠为**同族** `ExternalSession(Failed{Runtime})`,不返回错族。
- `ExternalSessionStep`:实现 `ScriptStep`(FAMILY=ExternalSession),`into_result` →
  `RequirementResult::ExternalSession(Box::new(result))`。
- `ExternalAgentFixture`:构造典型 `ExternalSessionRequest`(Start/Continue)、permission 型
  `PausedForInteraction`(以 `Interaction::question` 表达权限澄清,配 `PermissionRequested`
  observation,M4 落地 `InteractionKind::Permission` 后可升级)、`FilePatch`/`CommandFinished`
  event、`ExternalAgentOutput`、`ExternalSessionRef`、`ExternalArtifactRef`。
- `ExternalAgentCallLog`:call log 类型别名 `CallLog<ExternalSessionRequest, RequirementResult>`
  (记录调用序号、request、result、完成顺序,对齐 §12)。
- `assert_external_calls` / `ExternalAgentCallAssertions`:fluent 断言 helper,风格对齐
  `assertions/calls.rs`,并新增 external 专属摘要断言 `input_kinds` / `result_kinds`
  (`ExternalInputKind` / `ExternalResultKind` 判别枚举)。

### 模块布局
- 新增 `crates/agent-testkit/src/external.rs`:`ExternalAgentCallLog` 别名、`ExternalSessionStep`、
  `ScriptedExternalSessionHandler`、`ExternalAgentFixture`。
- 新增 `crates/agent-testkit/src/assertions/external.rs`:`ExternalInputKind`、`ExternalResultKind`、
  `assert_external_calls`、`ExternalAgentCallAssertions`(复用 `assert_calls` 处理 count/order)。
- `lib.rs`:`pub mod external;`;`assertions/mod.rs`:`mod external; pub use ...`。
- `prelude.rs`:导出全部新组件。

### 测试(过滤名含 `external`,`cargo test -p agent-testkit external`)
1. 顺序返回 Completed → PausedForInteraction:两次 fulfill(start/continue),断言 result 族与
   `assert_external_calls` 的 count/completion_order/input_kinds/result_kinds。
2. 脚本耗尽折叠为同族 Failed(Runtime),call log count=1、result_kinds=[Failed]。
3. fixture 的 completed 结果携带 FilePatch/CommandFinished observations(结构化断言)。

### 验证门
1. `cargo fmt --all -- --check`
2. 聚焦:`cargo test -p agent-testkit external`
3. `cargo clippy --all-targets -- -D warnings`
4. `cargo test --all --all-targets`(≤30min;credential-gated 保持 ignored)
5. `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`
6. `git diff --check`

### 进度
- (进行中)已完成代码核对(external DTO / requirement / drive handler / testkit 模式),开始写 external.rs。

## 进度更新(M2-4 完成)
- 新增 `crates/agent-testkit/src/external.rs`(step/handler/fixture/call-log 别名)与
  `crates/agent-testkit/src/assertions/external.rs`(判别枚举 + fluent 断言);已接线 lib.rs/assertions/prelude。
- 5 个新测(过滤名 external)全过;完整验证序列全绿(testkit lib 136 passed = 131 基线 +5;clippy 0 告警;
  full suite 30×ok 0 failed;doc 0 告警;git diff --check 干净)。
- TODO.md 已标 [DONE] 并填完成记录;`CassetteExternalSessionHandler` 按 §12 留待后续。待提交。
