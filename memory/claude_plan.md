# M3-2 — 宏覆盖 `accepts` 与 `drive.rs` 扇出(第 4–7 处),并加等价性断言

**当前执行 = TODO.md 第一个未完成任务 = M3-2**(M1-*/M2-*/M3-1 已 DONE)。刀 (A) 第二步。

## 目标(TODO M3-2 / design §4.2–4.4)
- 扩展 `define_effects!` 宏体,从**同一清单**再生成设计 §4.1 表格:
  - 第 4 处 `accepts`:`RequirementKindGen::accepts(&self, &RequirementResultGen) -> Result<(), RequirementError>`
    (tag 对齐 + `accepts_check` 后置校验特例)。
  - 第 5 处 `HandlerScopeGen` trait:每 family 一个访问器,默认 `None`。
  - 第 6 处 `scope_handles_gen(&dyn HandlerScopeGen, RequirementKindTagGen) -> bool`。
  - 第 7 处 `fulfill_with_scope_gen(&RequirementKindGen, &dyn HandlerScopeGen, &RunContext) -> Option<RequirementResult>`;
    `needs_outer`(Subagent)生成 `None` 分支。
- 产物仍与手写版**并存**(`*Gen` / `*_gen` 名),不删旧码(删旧码是 M3-3)。
- 加等价性测试证明宏版 == 手写版(serde / accepts / scope_handles / fulfill_with_scope)。

## 关键发现(阻塞点 → 必须处理,非绕过)
M3-1 清单**未编码 handler 调用的按值/按引用参数形状**(`fulfill(request, *mode, ..)` vs `fulfill(*call_id, call, ..)`)。
`mode:LlmStepMode`、`call_id:ToolCallId` 是 Copy 按值传;其余按引用。design §4.3 明确承认"各 handler 参数形状差异大"。
仅凭 `field: Ty` 无法在 `macro_rules` 里区分按值/按引用,故**必须给清单每个非 needs_outer effect 增补 `fulfill: ( args )` 子句**。
- 这不是绕过:它是生成 `fulfill_with_scope` 的正确清单信息,与 design §4 一致。
- 它修正了 M3-1 完成记录里"清单自 M3-1 起即终态"的乐观说法(该说法不是权威 spec;权威是 design + TODO)。
- 在完成记录里透明记录此清单增补。

## 落点
- `src/agent/effect_manifest.rs`:matcher 增补可选 `fulfill: ( $($fulfill_arg:tt)* )`;宏体新增第 4–7 处生成,
  用 `concat!/stringify!` 合成 doc 满足 `#![warn(missing_docs)]`。模块 doc 更新(M3-2 覆盖 4–7)。
- `src/agent/requirement.rs`:清单每个非 needs_outer effect 加 `fulfill:` 子句;`use` 增补 `RunContext`;
  扩展/新增 accepts+serde 等价性测试(在本文件 tests,可访问手写 accepts + *Gen)。
- `src/agent/drive.rs`:tests 里加 scope_handles/fulfill_with_scope 等价性测试(手写 fn 私有,须在 drive.rs;
  宏版 `*_gen` pub 从 requirement 引入)。构造覆盖 6 family 的测试替身,同时 impl `HandlerScope` + `HandlerScopeGen`。

## 宏技巧要点
- accepts 的 expected/actual 直接映射到手写 `RequirementKindTag::$tag`,使错误复用手写 `RequirementError`(可 `==` 比较)。
- accepts_check 用 `?`(经 `RequirementError::Interaction` 的 `#[from]`)自动转换;要求该 effect 单字段(仅 Interaction)。
- fulfill_with_scope_gen 用两个互斥可选块:`$(needs_outer→None)?` 与 `$(fulfill→handler 调用)?`,每 effect 恰好命中一个。
- HandlerScopeGen 访问器返回 `Option<&dyn crate::agent::drive::$handler>`。

## 验证序列(default)
1. `cargo fmt`  2. `cargo clippy --all-targets -- -D warnings`
3. 聚焦:`cargo test -p agent-lib --lib requirement drive`(含新等价性测试)
4. 全量:`cargo test --all --all-targets`(≤30min)
5. `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`
6. `git diff --check`

## 步骤
- [x] 写 memory 计划(本文件)
- [x] 扩展宏体(accepts + HandlerScopeGen + scope_handles_gen + fulfill_with_scope_gen)+ 更新模块 doc
- [x] 清单加 fulfill 子句 + import RunContext
- [x] cargo build 通过(先验证宏展开)
- [x] 加等价性测试(requirement.rs + drive.rs)
- [x] fmt / clippy / 聚焦测试 / 全量测试 / doc / git diff --check
- [x] 写完成记录 → TODO.md 标 [DONE] → commit → 停

## 验证记录
- `cargo fmt --all`:通过。
- `cargo clippy --all-targets -- -D warnings`:零告警。
- `cargo test -p agent-lib --lib`:**556 passed / 0 failed / 0 ignored**。
- `cargo test --all --all-targets`:全绿,36 个测试二进制全部 `test result: ok`,无失败。
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`:通过。
- `git diff --check`:无空白/冲突标记错误。
- 新增测试:`drive::tests::generated_fan_out_matches_hand_written_across_families`,以及 `requirement.rs`
  的 serde/`tag`/`accepts` 矩阵/`accepts_response` 委派等价性测试。
- 说明:因 M3-1 清单未编码 handler 实参形态,按设计 §4.3 为清单新增 `fulfill: (args)` 子句并把
  `accepts_check` 改为 `receiver.method`;手写版未删(留待 M3-3)。
