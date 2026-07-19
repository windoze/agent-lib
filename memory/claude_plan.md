# 执行计划：M3-2 `MessageRecord` 增加 `meta` 字段（H-STATE-2）

当前时间：2026-07-19。前序 M1/M2 及 M3-1 已完成（git log 确认最新提交为 M3-1）。
TODO.md 中第一个未标 `[DONE]` 的任务是 **M3-2**。

## 任务理解

`src/conversation/persistence/rows.rs`：
- `MessageRecord`（rows.rs:123-133 附近）只有 `payload: Message`，没有 meta。
- 分解 snapshot → rows 时（rows.rs:351-356）`payload: message.payload().clone()` 丢弃 envelope 的 `MessageMeta`。
- meta 由 `inject_user_message`（`src/conversation/pending/turn.rs:328-332`）经 `ConversationMessage::new_with_meta`（`src/conversation/message.rs:70-75`）写入。
- 现有 e2e（`persistence/tests/e2e.rs:44-47` 的 `assert_eq!(rebuilt_snapshot, snapshot)`）因夹具未用 `inject_user_message`，从未覆盖 meta round-trip。

## 实现要求（来自 TODO.md）

1. `MessageRecord` 增加 `meta: Option<MessageMeta>`，serde `#[serde(default)]` 保持旧行数据可反序列化。
2. `to_rows`/`into_snapshot` 双向携带 meta；`ConversationMessage` 构造走 `new_with_meta`。
3. 检查 `ConversationRows` 文档（rows.rs:3-5），恢复"与 snapshot 描述同一一致点"的承诺（如文档承诺丢失 meta 需更新）。

## 验证要求

1. e2e 夹具增加一条经 `inject_user_message` 注入的消息（带 source meta），断言 `to_rows → into_snapshot` round-trip 相等。
2. `cargo test -p agent-lib --lib conversation::persistence` 与 `cargo test --test conversation_persistence*` 全过。

## 执行步骤

1. [ ] 读代码：`rows.rs`（MessageRecord、to_rows/into_snapshot、模块文档）、`message.rs`（MessageMeta/new_with_meta）、persistence e2e 夹具与测试目标名。
2. [ ] 写实现：MessageRecord 加 `meta` 字段 + serde default；to_rows 写入 meta；into_snapshot 用 new_with_meta 还原。
3. [ ] 加测试：e2e 夹具注入带 meta 的消息，round-trip 相等断言；（可能加旧行数据缺 meta 字段反序列化兼容测试）。
4. [ ] 更新 `ConversationRows` 模块文档中关于 meta 的承诺。
5. [ ] 门禁：`cargo fmt --all` → `cargo clippy --all-targets -- -D warnings` → 定向测试 → `cargo test --all --all-targets` → `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`。
6. [ ] 标注 `docs/review-2026-07.md` H-STATE-2 `✅ 已修复（M3-2）`。
7. [ ] TODO.md 标 `[DONE]` + 完成记录；提交 git。

## 进展日志

- 2026-07-19 开始：选定任务 M3-2，开始读代码。
- 2026-07-19 完成 M3-2：
  - `MessageRecord` 新增 `meta: Option<MessageMeta>`（serde default + skip none），`from_snapshot`/`messages_for_turn` 双向携带，缺 meta 的旧行数据可反序列化。
  - rows 模块文档恢复"与 snapshot 同一一致点"承诺；`docs/conversation-core.md` §10 messages 行形态补 meta 列。
  - 新增 e2e `rows_round_trip_preserves_injected_user_message_meta`（inject_user_message 带 source meta，全路径 round-trip + 旧行兼容断言）。
  - 门禁全过：fmt、clippy（默认 + external features）、`cargo test --all --all-targets`（exit 0，914 lib 测试）、cargo doc。
  - `docs/review-2026-07.md` H-STATE-2 标注 ✅；TODO.md M3-2 标 [DONE] 并写完成记录。
  - 提交后停止，等下一次调用处理 M3-3。
