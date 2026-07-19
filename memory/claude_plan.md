# 执行计划

## 当前任务：M3-8 fork 不继承 compaction projection 的文档化（M-CONV-7，方案 a）

任务来源：`TODO.md` M3-8。前置状态：M3-7 已完成并提交（0cbd544），工作区干净。
纯文档任务，不改行为。

### 首项 bookkeeping（已完成）

- `TODO.md` M3-5 父标题补标 `[DONE]`（全部子任务 M3-5-1 ~ M3-5-4 已完成，父任务无
  独立工作）+ 一行父级完成记录；review 文档 M-CONV-3 标注仍留待 M3-9。

### 进度记录

- [x] 读取 TODO.md / git 状态，确认首个未完成任务为 M3-8
- [x] 写入本计划
- [x] 读取 fork.rs、projection/mod.rs（`raw_for_active_turns`、`CheckedTurnRange`
      owner 锚点）、conversation-core.md §6、DESIGN.md §Revert/Fork、
      review-2026-07.md M-CONV-7
- [x] `fork.rs`：模块文档 + `fork_at` “# Projection is not inherited” 节
      （取舍/理由/影响；私有 `raw_for_active_turns` 不做 intra-doc link）
- [x] `docs/conversation-core.md` 新增 §6.2（TODO 所称“§7 compaction/projection 节”
      即当前 §6，按括号意图落位）
- [x] DESIGN.md Fork 段补交叉引用 bullet
- [x] `docs/review-2026-07.md` M-CONV-7 标注 `📄 已降级（文档承认现状，M3-8）`
      （方案 a = 行为不变、文档承认现状；本任务单首个 📄）
- [x] 验证：fmt、两道 clippy、`cargo test -p agent-lib --lib conversation::boundary`
      （23 过）、`cargo test -p agent-lib --doc`（12 过，含 fork_at doctest）、
      cargo doc（-D warnings）全过；全量套件按政策跳过（仅注释/文档变更，沿用
      M3-7 绿色结果）
- [x] TODO.md M3-8 标 [DONE] + 完成记录
- [ ] 提交 `[M3-8] ...` 并停止

## 任务完成总结

M3-8 已完成（纯文档）：fork 不继承 compaction projection 的取舍在 rustdoc
（fork.rs）、conversation-core.md §6.2、DESIGN.md 三处一致文档化；M-CONV-7 以
📄 已降级标注。另顺手修正 M3-5 父标题的 [DONE] 标记。
下一任务：M3-9（M3 review：Conversation 正确性收口）。
