# Claude 执行计划

## 当前任务:M2-2 新增 `NeedExternalSession` requirement 与结果变体

**前置依赖**:M2-1(已 DONE)。

### 思路
在 `src/agent/requirement.rs` 的三个枚举里增量新增 external 家族,复用 M2-1 已落地的
`ExternalSessionRequest` / `ExternalSessionResult` DTO,保持既有变体与 serde tag 名零回归。

### 做什么(精确改动点)
1. `RequirementKindTag` 增加 `ExternalSession` 变体(snake_case → `external_session`),更新其 `Display`。
2. `RequirementKind` 增加 `NeedExternalSession { request: ExternalSessionRequest }`(可 serde);
   更新 `RequirementKind::tag()`。
3. `RequirementResult` 增加 `ExternalSession(Box<ExternalSessionResult>)`(Box 防 enum 膨胀);
   更新 `RequirementResult::tag()`。
4. `RequirementKind::accepts`:tag 对齐已覆盖 external(无需额外校验,与 Reconfig 一致)。
5. drive.rs 编译修复(exhaustive match):
   - `scope_handles`:`ExternalSession => false`(M2-3 才接 `external()` 访问器)。
   - `fulfill_with_scope`:`NeedExternalSession { .. } => None`(暂无 handler,pop 到 outer;M2-3 接入)。
   这不是 workaround 掩盖 spec:external handler 属 M2-3 范围,本轮无 handler 是正确增量态。
6. 编译器指出的其它 exhaustive match 一并补齐。
7. mod.rs 重导出:新增的是枚举变体,`ExternalSessionRequest`/`ExternalSessionResult` 及三枚举已在重导出列表,
   无需新增命名类型。

### 测试(requirement.rs tests 模块)
- `ALL_TAGS` 增加 `ExternalSession`;`kind_of`/`result_of` 增加 external 分支(构造 sample request/result)。
  这会让既有 `accepts_matrix`、`every_requirement_kind_round_trips` 自动覆盖 external。
- 新增 `external_requirement_accepts_only_external_result`:断言 `NeedExternalSession` 只 accept
  `RequirementResult::ExternalSession`,拒绝其它家族。
- 新增 `external_requirement_tag_roundtrip`:`RequirementKind`/`RequirementResult` 的 `tag()` 一致 +
  `RequirementKind` serde round-trip。
- 过滤名:`cargo test --lib external_requirement`。

### 验证门
1. `cargo fmt --all -- --check`
2. `cargo test --lib external_requirement`(聚焦)
3. `cargo clippy --all-targets -- -D warnings`
4. `cargo test --all --all-targets`(≤30min;credential-gated 保持 ignored)
5. `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`
6. `git diff --check`

### 进度
- (进行中)已完成前置代码核对,开始改 requirement.rs。

## 进度更新(M2-2 完成)
- requirement.rs 三枚举各加 external 家族(tag `external_session`、`NeedExternalSession { request }`、
  `ExternalSession(Box<ExternalSessionResult>)`),两个 tag() + Display 各加一臂;accepts 无特例(tag 对齐)。
- 编译修复:drive.rs `scope_handles => false`、`fulfill_with_scope => None`(M2-3 才接 external() 访问器,
  正确增量态,非 workaround);testkit describe_requirement 加 external 摘要臂。
- 测试:ALL_TAGS 5→6、kind_of/result_of 加 external;新增 accepts_only / tag_roundtrip 两测;display 补齐。
- 完整验证序列全绿:fmt ✓;`cargo test --lib external_requirement` 2 passed ✓;clippy -D warnings ✓;
  `cargo test --all --all-targets` 全绿(lib 427、testkit 131,0 failed)✓;doc -D warnings ✓;diff --check ✓。
- TODO.md M2-2 [TODO]→[DONE] + 完成记录。PLAN.md 未改(纯 milestone 内推进)。
- 下一步:提交 [M2-2],停止;下轮从 M2-3 继续。
