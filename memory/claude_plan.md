# M7-2 实现 Codex stream decoder cassette 测试

**当前执行 = TODO.md 第一个未完成任务 = M7-2**（M1..M7-1 全 `[DONE]`）。
M7-1 已提交（commit 98bc5fb）。本文件从 M7-1 重写为 M7-2。

## 任务范围（TODO.md M7-2）
在 feature `external-codex` 下，新增 Codex `codex exec --json` 私有 JSONL 流解码器：
- assistant text / command execution / patch(file edit) / permission / tool(MCP) call / completion / error。
- 添加 committed cassette fixtures（合成、离线、无真实 Codex、无 secret）。
- 明确 unknown frame 容忍策略。
范围 = **仅 decoder + cassette 测试**（对齐 M6-2；live adapter/e2e 留给 M7-3）。

## 关键事实：当前 codex CLI（0.144.1）`exec --json` 实测 = `ThreadEvent` schema
经本机实测 `codex exec --json` 输出（已确认）：
- `{"type":"thread.started","thread_id":"..."}`
- `{"type":"turn.started"}` / `{"type":"turn.completed","usage":{...}}` / `{"type":"turn.failed","error":{"message":...}}`
- `{"type":"item.started"|"item.updated"|"item.completed","item":{"id":"item_N","type":<snake>,...}}`
- `{"type":"error","message":"..."}`（顶层非致命/瞬时错误，如 Reconnecting…；turn 继续，非终止）
item.details（flatten，tag=`type` snake_case）：`agent_message{text}` / `reasoning{text}` /
`command_execution{command,aggregated_output,exit_code,status:in_progress|completed|failed|declined}` /
`file_change{changes:[{path,kind:add|delete|update}],status}` /
`mcp_tool_call{server,tool,arguments,result,error,status}` / `web_search` / `collab_tool_call` / `todo_list` / `error{message}`。
上游源码确认：`exec/src/exec_events.rs`（ThreadEvent），`event_processor_with_jsonl_output.rs`：
- agent_message / reasoning 仅在 `item.completed` 出现（started 返回 None）。
- **exec `--json` 流里没有 host-pausable 审批/permission 事件**：审批由预设 `-a/-s` 策略内部解决，
  被拒动作表现为 `command_execution status=declined`（或 patch declined→failed）。
- MCP 工具由 codex 自身执行并回报 result（非 host bridge pause）。

## 忠实映射（无 workaround）
codex exec `--json` 自主运行，不向 host 中途让渡工具执行或审批 → decoder 每 turn 只落定
`Completed`（turn.completed）或 `Failed`（turn.failed / 无 error 时 lenient）。**无** PausedForToolCalls /
PausedForInteraction（这是 codex 与 claude 的真实能力差异，已被 M7-1 capability 反映）。
- thread.started → `SessionStarted{session_id=thread_id}`，记录 session_id。
- turn.started → 容忍（无观测）。
- turn.completed → `SessionCompleted` 观测 + `CodexDecision::Completed{output}`；usage 映射，summary=本 turn
  最后一条 agent_message 文本，cost_micros=None。
- turn.failed → `CodexDecision::Failed{Runtime{code:None,message}}`。
- 顶层 error → 容忍（瞬时，无观测）。
- item.started command_execution → `CommandStarted{command,cwd(来自 context)}`。
- item.completed command_execution：completed→`CommandFinished{exit,stdout_tail=output}`；
  failed→`CommandFinished{exit,stderr_tail=output}`；declined→`PermissionRequested{action_id=item_id,summary}`
  （这是 exec 流里唯一的 permission 信号：策略拒绝的门控动作，信息性，非可应答 pause）。
- item.completed file_change → 每个 change 一个 `FilePatch{path,summary="{kind} {path}"}`。
- item.started mcp_tool_call → `ToolStarted{name="{server}/{tool}"}`；
  item.completed mcp_tool_call → `ToolFinished{name,status: completed→Ok / failed|error→Error}`。
- reasoning / web_search / collab_tool_call / todo_list / error-item / item.updated / 未知 item type → 容忍。

## 容忍/Protocol 策略（稳定，永不 panic）
- 空行 / 顶层 error / turn.started / 未知顶层 type / 未知或缺失 item.type / item.updated → 容忍(Ok None)。
- 非法 JSON / 非对象帧 / 缺字符串 `type` / thread.started 缺 thread_id / item.* 缺 `item` 对象或 item 非对象
  → `ExternalAgentError::Protocol`。
- turn.completed 缺/非法 usage → usage=None（lenient）；turn.failed 缺 error → generic message（仍落定 Failed）。
- 所有诊断为固定字符串，绝不夹带 prompt/命令/输出/凭据。

## 设计（mirror M6-2 claude decoder）
新增 `src/agent/external/codex/decoder.rs`（feature-gated `pub`，`#![allow(clippy::result_large_err)]`）：
- `CodexDecodeContext{cwd}`：`new()` 空 cwd + `.with_cwd()`（命令 cwd 由 host 配置 worktree 提供；
  codex 不在流里给 cwd）。（不加 step_id/actor：decoder 不铸造 Interaction，避免 dead_code。）
- `CodexDecision{Completed{output}, Failed{error}}`（忠实：codex exec 无 host pause）。
- `CodexStreamDecoder`：`new(ctx)` / `push_line(&str)->Result<Option<CodexDecision>,Err>` /
  `take_observations()->Vec<ExternalObservedEvent>` / `session_id()`。seq 跨 turn 单调；决策时清 per-turn 状态。
在 `codex/mod.rs` 挂载 `mod decoder;` 并 `pub use decoder::{CodexDecision,CodexDecodeContext,CodexStreamDecoder};`
在 `external/mod.rs` codex re-export 追加这三个类型。

## cassette fixture（committed，合成，离线）
`tests/fixtures/external/codex/full_session.json`：2 turn。
- turn1（Start）：thread.started/turn.started/agent_message(text)/command(started+completed ok)/
  file_change(patch)/mcp_tool_call(started+completed tool)/declined command(permission)/turn.completed(usage)
  → Completed。覆盖 text/command/patch/tool/permission/completion。
- turn2（Continue=resume）：agent_message/turn.failed → Failed。覆盖 error/failure + resume input。
in-code builder 为 source of truth；`AGENT_LIB_UPDATE_EXTERNAL_CASSETTES=1` 再生成；assert_no_secrets。

## 集成测试 `tests/agent_codex_cassette.rs`（feature-gated，离线）
mirror claude_code_cassette：regenerate guard、cassette==in-code builder、secret-free、full-session 逐帧逐决策、
容忍未知/空行/顶层 error 帧、malformed→Protocol、turn.failed→Failed。
（decoder 放 src 内 feature-gated pub，测试放 tests/ 集成，避免 agent-testkit 依赖环。）

## 文档
- `docs/managed-external-agent.md`：Codex 小节增补「decoder 实现状态（M7-2）」。
- `docs/capability-matrix.md`：说明 codex 离线 decoder 已落地（仍非 e2e），保守。
- `tests/fixtures/external/codex/README.md`：从预留占位改为描述已落地 fixture。

## 验证序列（TODO 1-6）
1. cargo fmt --all -- --check
2. cargo test -p agent-lib --features external-codex --test agent_codex_cassette（on）；未启 feature → 0 test
3. cargo clippy --all-targets -- -D warnings（+ --features external-codex）
4. cargo test --all --all-targets（feature off，<=30min）
5. RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace（+ feature）
6. git diff --check

## 进度（全部完成）
- [x] decoder.rs（CodexDecodeContext/CodexDecision{Completed,Failed}/CodexStreamDecoder，永不 panic）
- [x] mod.rs / external mod.rs re-export（CodexDecision/CodexDecodeContext/CodexStreamDecoder）
- [x] cassette fixture（full_session.json 2 turn）+ README
- [x] tests/agent_codex_cassette.rs（7 个，全过）
- [x] docs 更新（managed §13.2 实现状态+订正表格 / capability-matrix M7-2 段 / fixtures README）
- [x] 验证 1-6 全过（fmt / feature test 7 + off 0 / clippy on+off / full suite 43 ok / doc on+feature / diff --check）
- [x] TODO.md [DONE] + 完成记录
- [ ] commit（下一步）
