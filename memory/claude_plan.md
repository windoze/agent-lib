# M1-4 `ChatSession` + `send` / `send_full` + `conversation()` + snapshot/restore

**当前任务 = TODO.md 首个未完成 = M1-4**（`### [TODO] M1-4`）。M1-1..M1-3 已 `[DONE]`。
设计输入：`docs/facade-api.md` §5.1–§5.3、§15.1。装配层，不新增 effect family，复用同一 `Conversation` 续接多轮。

## 目标
在 `src/facade/chat.rs` 落地：
- `ChatSession`（有状态：持 `Conversation` + `Arc<dyn LlmClient>` + `ModelConfig` + `FacadeIds`）。
- `ChatSessionBuilder`（`chat.session().system(..).build()`，继承 Chat 的 client/model/system/ids）。
- `Chat::session(&self) -> ChatSessionBuilder`。
- `send(&mut self, input) -> Result<Reply, FacadeError>`、`send_full(&mut self, input) -> Result<RunOutput, FacadeError>`：
  - 复用内部同一 `Conversation` + 同一 id source 续接多轮（每轮 `begin_turn`）。
  - 复用已有共享 `drive_turn`（无 tool-use 才 commit；tool-use → `UnexpectedToolUse`；失败兜底 cancel）。
- `conversation(&self) -> &Conversation`。
- `snapshot(&self) -> Result<ConversationSnapshot, FacadeError>`（只在 committed 一致点成功；pending → `ConversationError::Snapshot`）。
- `restore(snapshot, chat) -> Result<Self, FacadeError>`：`Conversation::restore(snapshot)?` 重建历史，从 `chat` 重新注入 client/model/ids。

## 关键锚点（已核实）
- `Conversation::snapshot() -> Result<ConversationSnapshot, ConversationError>`（pending 时 `SnapshotError::PendingTurn`）。
- `Conversation::restore(ConversationSnapshot) -> Result<Conversation, ConversationError>`。
- `FacadeError::Conversation(#[from] ConversationError)` → `?` 可直接转换。
- `drive_turn(&mut Conversation, &dyn LlmClient, &ModelConfig, &FacadeIds, input)` 已存在，多轮复用即续接历史。
- `ChatSession` / `ChatSessionBuilder` 与 `Chat` 同模块 → 可访问 `Chat` 私有字段。
- `ConversationSnapshot` / `ConversationError` 由 `crate::conversation` 重导。

## 设计取舍
- `ChatSession` 从 `Chat` 克隆 client/model/ids（`FacadeIds` 是 Arc 共享计数器，唯一性 OK）。
- 快照不含 client/凭据：类型层面 `ConversationSnapshot` 不携带任何 client/ProviderConfig（断言字段即可）。
- `ChatSessionBuilder::build` 返回 `Result`（对齐 doc `chat.session().build()?`，未来可失败）。

## 测试（离线 fake client，追加到 `src/facade/chat/tests.rs`）
- 两轮 `send` 后 `conversation().effective_view()` 含前一轮历史（请求 message 数递增，如 [1,3,...]）。
- `snapshot()` 在 committed 点成功；pending 场景不构造（正常路径 send 后即 committed）。
- `restore()` 后继续 `send` 能接上历史（请求含之前轮次消息）。
- snapshot 不含 client/凭据（`ConversationSnapshot` 类型层面无此字段——注释 + 序列化断言不含 key）。
- tool-use → `UnexpectedToolUse`（session 版）。

## 验证
1. `cargo fmt --all -- --check`
2. `cargo test -p agent-lib --lib facade::chat`
3. `cargo clippy --all-targets -- -D warnings`
4. `cargo test --all --all-targets`（≤30min）
5. `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`
6. `git diff --check`

## 完成状态：DONE
- 落地 `ChatSession` / `ChatSessionBuilder` / `Chat::session`；`send`/`send_full`/`conversation`/`snapshot`/`restore`。
- 前置缺陷修复：`FacadeIds::seeded` + `FacadeIds::continuing_after`，`restore` 用 `continuing_after` 避免 id 冲突
  （实测无修复时 restore 后 send 撞 `DuplicateMessageId(...3)`）。
- mod.rs / prelude 重导 `ChatSession`；chat.rs / mod.rs / prelude rustdoc 更新。
- 全序列绿：fmt ✅ / facade 聚焦 35 passed ✅ / clippy ✅ / full suite（--lib 698 passed，各 suite 0 failed）✅ / doc ✅ / diff --check ✅。
- TODO.md 已标 `[DONE] M1-4` 并补完成记录。
