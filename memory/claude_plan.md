# M3-1 — 设计 `define_effects!` 清单语法与宏骨架(与手写版并存)

**当前执行 TODO.md 第一个未完成任务 = M3-1**(M1-*、M2-* 已 DONE)。刀 (A) 第一步。

## 目标(设计文档 §4.2 / TODO M3-1)
- 设计单一 effect 清单 `define_effects!` 的语法(先文档化再落宏骨架)。
- 宏生成设计文档 §4.1 表格**第 1–3 处**:三个 coproduct enum + `RequirementKindTag::Display` + `tag()`。
- 宏产物以**新名字** `RequirementKindGen` / `RequirementResultGen` / `RequirementKindTagGen` 与手写版**并存**,不替换(便于 M3-2 等价性断言、M3-3 替换)。

## 选型:`macro_rules!`(非 proc-macro crate)
- 已用 /tmp 探针验证 `macro_rules!` 足以表达:半区 derive 差异(Kind derive serde / Result 不 derive)、
  struct-variant(Kind) vs tuple-variant(Result) vs unit(Tag)、per-field serde 属性(result_schema 的
  `#[serde(default, skip_serializing_if)]`)、`Box<..>` 结果、可选 `needs_outer` / `accepts_check` 标记。
- 满足 `#![warn(missing_docs)]` + clippy `-D warnings`:宏用 `concat!`/`stringify!` 合成 doc(仿 id.rs 的
  `define_id!`),每个 enum/变体/字段都有 doc,零 missing_docs 警告。
- 无需新增 proc-macro crate(设计文档 §4.4 的退化路径未触发)。理由写进完成记录。

## 落点
- 新增 `src/agent/effect_manifest.rs`:仅含 `macro_rules! define_effects` + 语法 doc + `pub(crate) use`。
- `src/agent/mod.rs`:加 `mod effect_manifest;`。
- `src/agent/requirement.rs`:`use` 宏并 invoke,生成三个 `*Gen`(pub,可达→无 dead_code;不从 agent/mod.rs 再导出,最小暴露面)。
- 清单一次写全 6 个 effect(Llm/Tool/Interaction/Subagent/Reconfig/ExternalSession),含 handler/accessor/
  needs_outer(Subagent)/accepts_check(Interaction)——M3-1 只消费 tag/kind/result 生成 1–3,M3-2 扩展宏体消费其余。

## 等价性/形状要求
- Kind derive: `Clone, Debug, PartialEq, Serialize, Deserialize` + `#[serde(rename_all="snake_case")]`,变体名/字段/
  serde 名与手写一致 → serde 逐字节相等(M3-2 断言)。
- Result derive: `Clone, Debug`(不 serde)。
- Tag derive: `Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize` + snake_case;`Display` 用 tag_name。

## 步骤
- [x] 读 TODO/PLAN/design §4 + requirement.rs + drive.rs 扇出点
- [x] /tmp 探针验证 macro_rules 方案(missing_docs + unused 干净)
- [x] baseline: cargo test -p agent-lib agent::requirement = 13 passed
- [ ] 写 effect_manifest.rs(宏 + 语法 doc)
- [ ] mod 声明 + requirement.rs invoke 清单
- [ ] 加最小 smoke test(#[cfg(test)] 用 *Gen,确保生成物可用;完整等价性留 M3-2)
- [ ] fmt / clippy --all-targets -D warnings / cargo test -p agent-lib agent::requirement / doc -D warnings / git diff --check
- [ ] 全量 cargo test --all --all-targets(有代码改动)
- [ ] 写完成记录 → TODO.md 标 [DONE] → commit → 停

## 验证记录(待填)

## 验证记录(已填)— 全过
1. fmt --check ✅  2. agent::requirement = 15 passed(13+2 smoke)✅  3. clippy --all-targets -D warnings ✅
4. test --all --all-targets = 0 failed(lib 553 passed)✅  5. doc -D warnings ✅  6. git diff --check ✅
- 选型 macro_rules(未退化 proc-macro crate);/tmp 探针先验;concat!/stringify! 合成 doc 满足 missing_docs
- 新增 effect_manifest.rs;mod 声明;requirement.rs invoke 6-effect 清单(终态)+ 2 smoke test;手写码零删
- 生成第 1–3 处(*Gen 三 enum + Display + tag());4–7 处留 M3-2;第 8 处不宏化
- TODO.md M3-1 标 [DONE] + 完成记录已写(修复误删的 M3-2 heading);准备 commit;停
