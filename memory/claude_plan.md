# M3-3 — 切换到宏产物、删除手写版(刀 (A) 第三步)

**当前执行 = TODO.md 第一个未完成任务 = M3-3**(M1-*/M2-*/M3-1/M3-2 已 DONE)。

## 目标(TODO M3-3 / design §4.2–4.4)
- 删除手写的三个 enum + `accepts` + `HandlerScope`/`scope_handles`/`fulfill_with_scope`,
  让宏产物接管**正式名**(去掉 `*Gen` 临时名)。
- 全库编译:引用 `RequirementKind::NeedLlm{..}` 等的地方(machine/drive/testkit/tests/examples)
  无需改动(变体名/字段/serde 形状不变)。
- 删除 M3-2「对比手写版」的等价性测试,保留对宏产物本身的行为测试(serde 往返、accepts 矩阵、
  fan-out 路由 + `scope_handles⟺fulfill_with_scope` 不变量)。
- 在 `docs/effect-refine.md` 补附录:演示「新增 effect = 清单加一段」的完整 diff。

## 关键设计决策:回调式清单宏(保持 design §4.1 文件落位)
单个 `define_effects!` 只能在**一个模块**展开;但 design 要求 coproduct 在 requirement.rs、
fan-out 在 drive.rs,且 `HandlerScope` 是公开 API(`agent::HandlerScope`,经 `drive` 再导出),
`scope_handles`/`fulfill_with_scope` 是 drive.rs 私有。为同时满足「单一清单」+「各就各位」+
「零对外 API 改动」,改成**回调式**:
- `effect_manifest.rs`:`with_effect_manifest!($gen:ident)` 持有唯一清单(6 段),把 token 传给
  `$gen`;两个生成器 `define_effect_coproduct!`(coproduct)与 `define_effect_fan_out!`(fan-out)。
- `requirement.rs`:`with_effect_manifest!(define_effect_coproduct);` → 生成
  `RequirementKindTag`/`RequirementKind`/`RequirementResult` + `Display`/`tag()`/`accepts`(正式名)。
- `drive.rs`:`with_effect_manifest!(define_effect_fan_out);` → 生成 `HandlerScope`(pub)+
  `scope_handles`/`fulfill_with_scope`(私有,取 `&RequirementKind`)。

### 卫生性验证(已用 rustc 探针确认)
- 回调式 `$gen!{...}` 可行。
- 清单里裸写的类型路径(ChatRequest/LlmHandler…)在**展开处**解析:coproduct 类型在 requirement.rs
  已 import;fan-out 只发出 handler trait(drive.rs 本地定义)与字段名,无需额外 import。
- match 绑定的 `$field` 与调用实参 `$fulfill_arg` 同源(同一 `with_effect_manifest!` 体),局部变量
  卫生一致,可协同解析(探针 probe5=15 通过)。

## 落点与改动
- `effect_manifest.rs`:重写为回调式三宏 + 更新模块/宏 doc(去掉「*Gen 并存/transitional」措辞,
  改成「单清单驱动、跨 requirement/drive 生成正式产物」)。`pub(crate) use` 三宏。
- `requirement.rs`:删手写 `RequirementKindTag`(126-160)/`RequirementKind`(443-543)/
  `RequirementResult`(545-587);把 `define_effects!{…}` 换成 `with_effect_manifest!(define_effect_coproduct);`
  并将清单数据迁入 effect_manifest.rs;更新 import;删 M3-2 `*Gen` 等价性测试(1128-1253)与
  相关 import;保留 accepts 矩阵/serde 往返/tag display 等行为测试(现在直接测正式产物)。
- `drive.rs`:删手写 `HandlerScope`(114-147)/`scope_handles`(532-542)/`fulfill_with_scope`(544-579);
  加 `with_effect_manifest!(define_effect_fan_out);` + import;两处调用改传 `&requirement.kind`;
  测试:删 `HandlerScopeGen for EqScope`、`gen_tag_of`、把 `generated_fan_out_*` 改为直接测正式
  fan-out 路由 + 不变量;清理 `*Gen`/`*_gen` import。
- `docs/effect-refine.md`:新增「§7 附录:新增一个 effect」的一段清单 diff 指南。

## 第 8 处(机器 resume 分派)保持手写不动。

## 验证序列(default)
1. `cargo fmt --all`
2. `cargo clippy --all-targets -- -D warnings`
3. 聚焦:`cargo test -p agent-lib --lib requirement drive`
4. 全量:`cargo test --all --all-targets`(≤30min)
5. `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`
6. `git diff --check`

## 步骤
- [x] 读 TODO/PLAN/design/源码,选定 M3-3
- [x] 探针验证回调宏 + 卫生性
- [x] 写 memory 计划(本文件)
- [x] 重写 effect_manifest.rs(回调式三宏 + 清单迁入 + doc)
- [x] requirement.rs 删手写 + 换调用 + 清理测试
- [x] drive.rs 删手写 + 换调用 + 改测试
- [x] docs/effect-refine.md 附录
- [x] fmt/clippy/聚焦/全量/doc/diff --check
- [ ] 写完成记录 → TODO.md 标 [DONE] → commit → 停

## 验证记录
- `cargo build`:通过(库先行编译,确认回调宏在真实代码里展开无误)。
- `cargo fmt --all`:无改动残留。
- `cargo clippy --all-targets -- -D warnings`:0 warning。
- `cargo test -p agent-lib --lib`:552 passed / 0 failed(含 requirement/drive 模块)。
- `cargo test --all --all-targets`:**778 passed / 0 failed**(断言未改,证明对外形状/行为不变)。
- `cargo test --all --doc`:doctest 全绿(7+12+2,1 ignored 为 effect_manifest 的 ```ignore 语法示意)。
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`:通过(宏产物 rustdoc 可编译、有文档)。
- `git diff --check`:CLEAN。
- diff 范围:requirement.rs / drive.rs / effect_manifest.rs / docs/effect-refine.md / memory,
  净 −333 行(删手写三 enum+accepts+三处扇出+M3-2 等价性测试,换成两行宏调用)。PLAN.md 无需改
  (阶段计划未变);PROMPT.md 未被改动。
- 遗留引用检查:`define_effects`/`*Gen` 仅存于 TODO.md 的 M3-1/M3-2 历史 DONE 记录(应保留为历史)
  与本 memory 文件;生产代码/公开 doc 已无这些名字。
