# Claude 执行计划

## 当前任务:M2-5 Milestone 2 Review

**前置依赖**:M2-1..M2-4(均 DONE)。

### 目标(TODO M2-5)
Review external session effect 边界:确认完整、自洽、与既有 requirement 家族对齐,且未回归。

### 检查清单
1. 通读 M2-1..M2-4 diff:
   - external DTO serde round-trip 覆盖(`src/agent/external/mod.rs`)。
   - `RequirementKind` / `Tag` / `Result` 三处新增变体对齐(`tag()` 与 `accepts()`)——`src/agent/requirement.rs`。
   - `HandlerScope::external()` 分派正确、未加深 scope(`src/agent/drive.rs`)。
   - testkit scripted 组件可用(`crates/agent-testkit/src/external.rs`、`assertions/external.rs`)。
2. 记录 §14 决策点:`NeedExternalSession` 落在核心 `agent-lib`(本计划采用)vs 上层 crate。
3. 全量测试确认无回归。

### 验证门(完整序列)
1. `cargo fmt --all -- --check`
2. 聚焦:`cargo test -p agent-testkit external` + `cargo test --lib external`
3. `cargo clippy --all-targets -- -D warnings`
4. `cargo test --all --all-targets`(≤30min)
5. `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`
6. `git diff --check`

### 进度
- (进行中)开始核对源码与 diff。

## 进度更新(M2-5 完成)
- Review 无生产代码改动;四项自洽性检查全部通过:
  - DTO serde round-trip 覆盖三态(external/mod.rs 测试)。
  - Kind/Tag/Result 三处变体 + tag()/accepts() 对齐(requirement.rs)。
  - HandlerScope::external() 默认 None、就地兑现、未加深 scope(drive.rs)。
  - testkit scripted 组件(handler/step/fixture/call-log/assertions)可用,脚本耗尽折叠为同族 Failed。
- §14 决策点回填:NeedExternalSession 落在核心 agent-lib(增量家族),真实 runtime 隔离在 driver 侧 handler。
- 遗留项:CassetteExternalSessionHandler(§12)、InteractionKind::Permission(M4-1)、machine(M3-1+)、profile(M6)。
- 完整验证序列全绿:fmt OK;lib external 13 + testkit external 5;clippy 0 告警;full suite 全过 0 failed;
  doc 0 告警;git diff --check 干净。
- TODO.md 已标 [DONE] 并填完成记录。待提交。M2 签核,放行 M3。
