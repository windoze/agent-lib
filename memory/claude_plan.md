# M1-5 `ChatSession::stream` + `RunStream`（基于 `Accumulator`）

## 目标（TODO.md M1-5）
- `ChatSession::stream(&mut self, input) -> Result<RunStream<'_>, FacadeError>`。
- `RunStream`：`Stream<Item = Result<RunEvent, FacadeError>>`（并提供 `next().await` 便捷）。
  转发 `TextDelta`、以及 `RawStream(StreamEvent)`（escape hatch）；末尾 `Done(RunOutput)`。
- 内部用 `stream::accumulator::Accumulator` 折叠出完整 `Response`，再走与非流式
  `drive_pending` 相同的尾巴：`start_assistant_response` → `finish_assistant` →
  tool-use 则 `UnexpectedToolUse`、否则 `commit_pending`。`Done` 的 `RunOutput` 与非流式一致。
- 流中出现 tool-use → `UnexpectedToolUse`（Chat 不执行工具）。
- rustdoc + 离线 `no_run` doctest。

## 设计
- 新文件 `src/facade/chat/stream.rs`（模块化，chat.rs 已 21KB）：
  - `pub struct RunStream<'a>` 持 `&'a mut Conversation` + `BoxStream<'static, Result<StreamEvent, ClientError>>`
    + `Option<Accumulator>` + `FacadeIds`（clone）+ `VecDeque<RunEvent>` buffer + 状态机
    (`Streaming` / `Finishing` / `Done`)。
  - `impl Stream`（RunStream 全字段 Unpin，`inner.poll_next_unpin`；finish 全同步，无需 async）。
  - 每个上行 `StreamEvent`：先 buffer `RunEvent::TextDelta`（若 text delta），再
    `RunEvent::RawStream(event.clone())`；push 进 accumulator，错误→回滚+FacadeError。
  - inner 耗尽→Finishing：`accumulator.finish()`→`Response`；折叠进 conversation；
    ready→`commit_pending`+buffer `Done(Box<RunOutput>)`；tool-use→回滚+`UnexpectedToolUse`。
  - 任意错误 `cancel_pending(DiscardTurn)` 回滚 pending turn。
  - `AccumulatorError` 映射：`Stream(e)`→`Client(e)`；其余→`Client(Protocol(..))`。
  - 便捷 `pub async fn next(&mut self)`（inherent，包 `StreamExt::next`）。
- `chat.rs`：`mod stream; pub use stream::RunStream;`
  - `build_request` 增 `stream: bool` 参数（drive_pending 传 false，stream 传 true）。
  - `ChatSession::stream`：begin_turn → build_request(stream=true) → `client.chat_stream().await`
    （失败即回滚 pending + 直接返回 Err）→ `RunStream::new(&mut conversation, inner, ids.clone())`。
- `mod.rs` / `prelude.rs`：重导 `RunStream`；更新 rustdoc（stream 落地本任务）。

## 测试（离线 fake `chat_stream`，`src/facade/chat/tests.rs` 追加）
- fake client 支持脚本化 stream 事件序列（`chat_stream` 返回固定 `StreamEvent` 序列）。
- text 流：`TextDelta` 顺序正确、拼接文本 == 非流式；`Done` 的 `RunOutput.reply.text`/`usage`
  与非流式一致；流结束后 `conversation()` 已提交该轮（effective_view 含 user+assistant）。
- `RawStream` 事件被转发。
- tool-use 流（BlockStart ToolInput...）→ `UnexpectedToolUse`，且 conversation 回到 committed。
- 多轮：stream 之后再 stream / send 能续接历史。

## 验证序列
1. `cargo fmt --all`
2. `cargo clippy --all-targets -- -D warnings`
3. `cargo test -p agent-lib --lib facade::chat`
4. `cargo test --all --all-targets`（≤30min）
5. `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`
6. `git diff --check`

## 状态：DONE
- 全序列绿：fmt ✅ / clippy ✅ / facade::chat 16 passed ✅ / full suite（--lib 702 passed, doctest 12 passed）✅ / doc ✅ / diff --check ✅。
- TODO.md 已标 [DONE] M1-5 并补完成记录。
