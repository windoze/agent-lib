# M5-2 执行计划：对齐 `into_parts`、snapshot 和 builder 文档

## 任务性质
文档对齐任务。M5-1 已扩展 `AgentParts` 覆盖 external/协作/交互状态并更新了
Rust rustdoc。M5-2 负责把三条 API（snapshot=持久化、into_parts=接管 live
handles、builder=常规构造）的用途边界在文档层写清，并让 `docs/refine.md` §6
反映 M5-1 的修复，避免任何文档继续声称 `into_parts` 覆盖不完整。

## 现状分析（已 code walk）
- `src/facade/agent.rs` `into_parts` rustdoc：M5-1 已写清资源范围与「非 restore
  API」，但只交叉引用 snapshot/restore，未提 builder。
- `src/facade/agent/snapshot.rs` `AgentParts` rustdoc：已完整（M5-1）。
- `docs/facade-api.md` §8.2：仅在 API 形状里列出 `into_parts`，无任何 prose 说明
  三者用途区别 → 需要新增。
- `docs/refine.md` §6：仍把 into_parts 描述为「不完整、丢失 external/collab/
  interaction」，未标注 M5-1 已修复 → 需补状态行 + 修复结果块（镜像 §2/§4/§5）。
- README：无 into_parts 示例，故无字段示例需更新；可选补 capability 表一行。

## 编辑计划
1. `docs/facade-api.md` §8.2：在 API 形状块后新增 prose，说明 `into_parts` 交出的
   live/owned 部件清单，且它是拆解逃生舱、非 restore；给出三向选择准则
   （snapshot→持久化恢复，into_parts→接管 live handle，builder→常规构造）。
2. `src/facade/agent.rs` `into_parts` rustdoc：补 builder 交叉引用，凑齐三向对齐。
3. `docs/refine.md` §6：加「状态：已修复（M5-1；M5-2 文档对齐；M5-3 复核待进行）」
   状态行 + 末尾「修复结果」块，列 M5-1 实际改动。
4. README：确认无 into_parts 示例需改；在 capability 表补一行逃生舱入口（保持诚实）。

## 验证命令
- cargo fmt --all
- cargo clippy --all-targets -- -D warnings
- RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
- cargo test -p agent-lib --lib facade::agent::
（本任务仅改文档 + rustdoc；facade::agent 测试用于确认 rustdoc doctest 无破坏。）

## 进度
- [x] facade-api.md §8.2 prose（snapshot/restore/into_parts/builder 四向用途边界）
- [x] agent.rs into_parts rustdoc 补 builder 交叉引用
- [x] refine.md §6 状态行 + 修复结果块（M5-1 扩展、M5-2 对齐）
- [x] README 能力表补逃生舱行（无 into_parts 示例需改字段）
- [x] fmt/clippy(default clean)/doc(clean)/test(facade::agent 49 passed) 全绿
- [x] TODO.md M5-2 标 [DONE] + 完成记录
- [ ] commit（待执行）

## 结论
纯文档 + rustdoc 对齐任务，无 spec 偏差、无新增前置任务、无失败测试。
snapshot=持久化恢复、into_parts=接管 live handle、builder=常规构造 的用途边界已在
facade-api.md §8.2 / agent.rs rustdoc / refine.md §6 写清；无文档再声称 into_parts
覆盖不完整。仅改文档与注释，未改编译产物，故未重跑全量套件。
