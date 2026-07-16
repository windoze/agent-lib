# M3-4 — Milestone 3 review:刀 (A) 正确性、等价性与可维护性

**当前执行 = TODO.md 第一个未完成任务 = M3-4**(M1-*/M2-*/M3-1/M3-2/M3-3 均 [DONE])。
这是 TODO.md 的最后一个任务;完成后应做整体收尾并打 `endtag`。

## 这是 review 任务:验收刀 (A),不改生产代码(只补 TODO 完成记录)

### 验收点(TODO M3-4)
1. 第 1–7 处已全部由宏生成、第 8 处(机器 resume 分派)仍手写且未被误宏化。
2. 两处特例在宏产物里正确:
   - `NeedSubagent`(`needs_outer: true`)→ `fulfill_with_scope` 返回 `None`,走
     `resolve_requirement` + `ScopePop` 串行路径。
   - `NeedInteraction`(`accepts_check: request.accepts_response`)→ accepts 后置校验生效
     (`accepts_delegates_permission_action_id_check`)。
3. 「加 effect 成本」实验:临时加一个虚构 effect,确认只改清单一处即可编译通过,验证后回退。
4. 完整验证序列 1–6 + `cargo test --all --all-targets` 全绿;`git diff --stat` 范围合理。
5. `docs/effect-refine.md` §5 矩阵三行(语义变化=无、序列化风险=无)兑现。

## 已复核(静态)
- 清单唯一来源:`effect_manifest.rs::with_effect_manifest!`(6 段 stanza)。
- 两个生成器:`define_effect_coproduct`(requirement.rs 第 1–4 处)、`define_effect_fan_out`
  (drive.rs 第 5–7 处),各只在一个模块 invoke(requirement.rs:333 / drive.rs:116)。
- 第 8 处手写:`machine/default/mod.rs::resume/resume_llm/resume_reconfig`,不引用
  effect_manifest;两处对 `RequirementResult` 的 match 均有 `other =>` 兜底(加变体不破坏编译)。
- 无遗留 `define_effects` / `*Gen` / `_gen` 名字于生产代码。
- §5 矩阵三行 + §7 附录(加 Timer effect 的一段 diff)存在。

## 执行步骤
- [x] 读 TODO/memory/源码,选定 M3-4
- [x] 静态复核第 1–8 处 + 两处特例 + §5/§7 文档
- [x] 更新 memory 计划(本文件)
- [ ] 验证序列:fmt --check / clippy / 聚焦 requirement+drive / 全量 --all-targets / doc / diff --check
- [ ] 加 effect 成本实验:临时给清单加一段复用现有类型的 stanza → `cargo build -p agent-lib --lib` 通过 → 回退
- [ ] 写 M3-4 完成记录 → 标 [DONE]
- [ ] commit
- [ ] 整体收尾复核 + 打 `endtag`(TODO 全部完成)

## 验证记录(2026-07-17,M3-4)
- `cargo fmt --all -- --check`:FMT_OK。
- `cargo clippy --all-targets -- -D warnings`:0 warning。
- `cargo test -p agent-lib --lib`:552 passed / 0 failed。
- `cargo test --all --all-targets`:全绿(所有 test result `0 failed`,无 FAILED/panicked)。
- `cargo test --all --doc`:7+12+2 passed(1 ignored=effect_manifest ```ignore 语法示意)。
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`:通过。
- `git diff --check`:CLEAN。
- 加 effect 成本实验:临时给清单加 `TimerProbe`(复用现有类型)→ `cargo build -p agent-lib --lib`
  EXIT=0(仅改清单一处)→ `git checkout` 回退,`grep TimerProbe`=0。
- 刀 (A) 累计 diff stat(`62b9e76..1ba7875`):effect_manifest.rs +435 / drive.rs 306 / requirement.rs 188
  / docs/effect-refine.md 114 / mod.rs +1;未引入 proc-macro crate。
- 第 8 处(machine resume 分派)仍手写、未误宏化;两处特例(Subagent needs_outer→None、Interaction
  accepts_check)正确;§5 矩阵三行兑现。
- 结论:M3-4 通过。TODO.md 13 个任务全部 [DONE],下一步整体收尾 + 打 `endtag`。
