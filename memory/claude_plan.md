# 执行计划：M4-1 修复 `blackboard_read` 丢弃正文 + 补 mailbox 读工具（H-STATE-6）

## 状态：✅ 已完成（待提交）

## 执行结果

1. 探索代码：完成。定位 `src/agent/collab/tools.rs`（blackboard_read 只计数、无 mailbox 读工具、plan_read 缺 owner/deps）、`blackboard.rs`/`mailbox.rs`（`read_from` 游标 API）、`collab/tests.rs`（tool_adapter 测试模式）、文档枚举点（`docs/external-agent.md` §3.4 表、`docs/managed-external-agent.md` §8.1 清单）。
2. 实现：完成。
   - `blackboard_read` 返回正文：头部行保留原前缀 + 每消息 `#<offset> <sender>: <body>`；新增 `limit` 参数（默认 50）+ resume 提示 + 单条正文 200 字符截断（`… [truncated]`）。
   - 新增 `mailbox_read` 工具：读注入 identity 自己的收件箱（recipient 不可由模型提供），seq 游标分页 + 同一截断设施；注册进 declarations/dispatch，常量经 `collab/mod.rs` 导出。
   - `plan_read` 补 `@owner` 与 ` deps:[..]`（无 owner/deps 时输出与旧格式逐字一致）。
   - 共享 helper：`read_limit` / `format_read_page` / `truncate_body`。
3. 测试：新增 4 条（blackboard 正文、分页+截断、mailbox 读+bystander 空收件箱、plan owner/deps）+ declarations 清单补 MAILBOX_READ；`agent::collab` 32 条全过。
4. 文档：`docs/external-agent.md` §3.4 表加 mailbox_read 行；`docs/managed-external-agent.md` §8.1 清单同步；`docs/agent-layer.md` §6.4 不枚举工具面无需更新；`docs/review-2026-07.md` H-STATE-6 标注 `✅ 已修复（M4-1）`。
5. 验证全过：fmt、clippy（默认 + external features）、`cargo test --all --all-targets`（exit 0，31s）、doc。
6. TODO.md：M4-1 标 [DONE] + 完成记录。下一步：commit 后停止。
