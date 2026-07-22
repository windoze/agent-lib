## Execution Plan — M1-2 收口：[TODO] → [DONE]

本文件记录本轮（2026-07-23）可执行计划与进度。

## 现状判定

- TODO.md 第一个未完成任务：**M1-2**（标题仍 `[TODO]`，规则上未完成）。
- 但 commit `b29fa6d` 已实现 M1-2 全部代码并提交，完成记录也已写入 TODO.md（line 156-192）。
- 上一次调用（计划文件旧版）声称「已标 [DONE] + 提交」，但**实际漏了标题翻转**：
  commit 只追加了完成记录正文，标题仍为 `[TODO]`。这是上一轮的遗留缺口。
- 已逐文件核对：4 个源文件 + 模板 `openai_resp` + `src/client/mod.rs` 导出，实现完整、
  形状一致、与完成记录吻合。**唯一缺失 = 标题 `[TODO]`→`[DONE]`。**

## 本轮步骤

1. ✅ 已核对实现完整正确（mod.rs / request.rs / response.rs / stream/mod.rs + 模板 + 导出）。
2. 跑验证（faithful reporting，确认当前 HEAD 绿）：
   - `cargo test -p agent-lib --lib adapter::openai_chat`（应 3 通过）
   - `cargo clippy --all-targets -- -D warnings`
3. TODO.md line 119：`### M1-2 [TODO]` → `### M1-2 [DONE]`（完成记录已存在，不追加）。
4. 提交（仅文档状态收口；无代码改动，全量测试套件复用 b29fa6d 绿结果）。
5. 停。

## 备注

- 纯状态收口，不触碰 M1-3 及之后。
- 完成记录已充分，无需追加内容。

## 进度日志
- 核对 4 源文件 + 模板 + 导出 ✓
- 待跑：目标测试 + clippy
- 待改：TODO.md 标题翻转
- 待提交
