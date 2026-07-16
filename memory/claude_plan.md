# Claude 执行计划

## 当前任务:M2-3 定义 `ExternalSessionHandler` trait 并接入 `HandlerScope`

**前置依赖**:M2-2(已 DONE)。

### 思路
按既有 handler 家族(llm/tool/interaction/subagent/reconfig)的模式,在 `src/agent/drive.rs`
增量新增 external-session handler 家族:定义 trait、给 `HandlerScope` 加 `external()` 访问器、
把 M2-2 已留的两个占位分派臂(`scope_handles` / `fulfill_with_scope`)接到真实 handler。

### 精确改动点
1. `drive.rs` 导入:agent 导入列表加入 `ExternalSessionRequest`。
2. 新增 `#[async_trait] pub trait ExternalSessionHandler: Send + Sync`,
   `async fn fulfill(&self, request: &ExternalSessionRequest, ctx: &RunContext) -> RequirementResult;`
   rustdoc 写明语义(把 session 推进到下一决策点 Completed/PausedForInteraction/Failed,
   期间 event 放进 observations;返回值必须是 `RequirementResult::ExternalSession`)。设计 §5.5。
3. `HandlerScope` 加 `fn external(&self) -> Option<&dyn ExternalSessionHandler> { None }`。
4. `scope_handles`:`ExternalSession => scope.external().is_some()`(替换 M2-2 的 `=> false`)。
5. `fulfill_with_scope`:`NeedExternalSession { request } => Some(scope.external()?.fulfill(request, ctx).await)`
   (替换 M2-2 的 `=> None`)。external 不加深 scope(与非 subagent 家族一致)。
6. `mod.rs`:`pub use drive::{...}` 加 `ExternalSessionHandler`。
7. 模块 rustdoc:去掉 "up to four handlers" 陈旧计数(现有 reconfig 已使其失真),
   改为不带数字的措辞,并把 reconfig/external 补进家族列表。

### 测试(drive.rs tests,过滤名 `external_session_handler`)
- 新增 external 构造子(参考 requirement.rs 测试:request=Start、result=Completed)。
- `TestScope` 增加 `external: Option<...>` 字段与 `external()` 访问器;新增计数型
  `CountingExternalSessionHandler` fixture 返回固定 `ExternalSessionResult::Completed`。
- `external_session_handler_result_is_accepted_by_its_requirement`:直接 fulfill,结果被
  `NeedExternalSession` 的 `accepts` 接受(对齐既有 `*_handler_result_is_accepted_by_its_requirement`)。
- `external_session_handler_drain_fulfills_locally`:drain 一个 `NeedExternalSession`,scope 提供
  `external()`,cursor Done、handler 被调 1 次、resume tag == ExternalSession。
- `external_session_handler_default_scope_pops_to_outer`:inner 无 external、outer 有 external,
  drain 后 outer handler 被调 1 次(popped to outer),inner 未被触及。

### 验证门
1. `cargo fmt --all -- --check`
2. `cargo test --lib external_session_handler`(聚焦)
3. `cargo clippy --all-targets -- -D warnings`
4. `cargo test --all --all-targets`(≤30min;credential-gated 保持 ignored)
5. `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`
6. `git diff --check`

### 进度
- (进行中)已完成代码核对,开始改 drive.rs。

## 进度更新(M2-3 完成)
- drive.rs 新增 `ExternalSessionHandler` trait + `HandlerScope::external()` 访问器;
  `scope_handles`/`fulfill_with_scope` 两处 external 分派臂由 M2-2 占位改为真实 handler(不加深 scope)。
- mod.rs 重导出 `ExternalSessionHandler`;模块/trait rustdoc 去掉陈旧 "four handlers" 计数并补 reconfig/external。
- 3 个新测(过滤名 external_session_handler)全过;完整验证序列全绿(lib 430、testkit 131,0 failed)。
- TODO.md 已标 [DONE] 并填完成记录。待提交。
