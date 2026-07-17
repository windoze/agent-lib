# M6-3 实现 Claude Code session adapter 与 ignored real e2e

**当前执行 = TODO.md 第一个未完成任务 = M6-3**（M1..M5 全 `[DONE]`，M6-1/M6-2 `[DONE]`）。

## 任务
在 `external-claude-code` feature 下，实现真实 Claude Code runtime adapter：
- start / resume / advance / cleanup（`ExternalRuntimeAdapter` + `ExternalRuntimeSession`）。
- 包裹 M6-2 的 `ClaudeStreamDecoder`，把真实 CLI stream-json stdout 逐行喂给 decoder，
  驱动到**下一个** decision point（禁止一次跑到底）。
- 接入 live sink（每行 drain 后 emit）。
- host tools：本 adapter 不实现 MCP server，因此 `host_tools=false`/`host_subagents=false`
  （§12.3 允许的分支）；start 请求携带 `tools` 时返回 `UnsupportedCapability{HostTools}`。
- ignored real e2e：检测 `CLAUDE_CODE_BIN`/PATH + 登录态，缺失则 skip；临时 worktree；
  验证 text -> permission -> completion 单进程多步会话。

## 设计
新增 feature-gated 私有模块 `src/agent/external/claude_code/adapter.rs`：
- `ClaudeSessionIo` trait（可注入 IO，离线测试用 fake，生产用真实 process）：
  - write_frame / read_frame -> Option<String> / close -> ExternalSessionShutdown。
- `ClaudeProcessIo`：tokio::process spawn（stdin/stdout piped、kill_on_drop、working_dir、env、
  per-read timeout）；close = drop stdin -> wait(grace) -> Graceful / kill->ForcedKill / Failed。
- `ClaudeCodeSession<Io>`（实现 `ExternalRuntimeSession`）：持有 io + ClaudeStreamDecoder
  + session_id + last_event_seq + sink + capabilities + carried(prelude 观测)。
  - prelude 时序修正（真机验证）：Claude Code **在收到首条 stdin turn 前不产出任何 frame**（连
    `system/init` 都不发）。故 start 先写首个输入(prompt)，再 read 到 `init` 帧拿 session_id 作 key；
    该 turn 余帧留给第一次 advance 续读（`first_turn_pending` 标记，避免重复写）。resume 已知 id，
    begin 不预读，首次 advance 写续跑 turn 并读新 init。
  - advance(input)：非首turn 先写 input 对应 stdin 帧，再 drive 到 decision：
    - Start{prompt}/Continue{msg} -> user text frame；
    - RespondInteraction(Permission) -> control_response allow/deny(request_id=action_id)；
    - RespondToolResults/RespondSubagent -> UnsupportedCapability（防御，正常不会到达）。
  - drive loop：read_frame -> push_line -> take_observations -> emit sink + 累积；
    Some(decision)->返回；EOF 未决策 -> SessionLost；push_line Err -> 透传（Protocol）。
  - shutdown -> io.close()。
- `ClaudeCodeAdapter`（实现 `ExternalRuntimeAdapter`）：config + effective capabilities
  = 实现能力(streaming/permission_bridge/resume/artifacts/usage/graceful=true, host_tools/subagents=false)
  与传入(probe)能力交集。start（gate tools）/resume（--resume <id>）/kind/capabilities。
- decode context：StepId::new(*ctx.run_id().as_uuid())（caller-supplied run id 派生，不随机生成）
  + actor = request.agent_id。
- stdin 帧用 serde_json 构造（转义安全）。

## 测试
- 内联 #[cfg(test)] mod tests（claude_code_adapter*，离线，仅用 agent-lib 类型 + 手写 FakeIo，
  规避 agent-testkit<->agent-lib 依赖环）：
  - session 全程 text->permission->completion 回放、sink 观测、session_ref/last_event_seq、
    EOF->SessionLost、malformed->Protocol、shutdown 分类、resume prelude。
  - adapter kind/capabilities（host_tools/subagents=false 其余 true、与 probe 交集）、
    start tools->UnsupportedCapability。
- ignored real e2e：tests/external_claude_code.rs（feature-gated，#[ignore]）。

## mod / 导出 / 文档
- claude_code/mod.rs 增 mod adapter; + re-export；external/mod.rs re-export 新公有类型。
- docs：managed-external-agent.md §12.1/§12.3 增「实现状态(M6-3)」；capability-matrix.md Claude 行。

## 验证序列（TODO.md 1-6 + 聚焦）
1. cargo fmt --all -- --check
2. cargo test -p agent-lib --features external-claude-code claude_code_adapter +
   cargo test -p agent-lib claude_code_cassette（feature off -> 0 test；on -> pass）
3. cargo clippy --all-targets -- -D warnings（+ --features external-claude-code）
4. cargo test --all --all-targets（feature off）
5. RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace（+ feature）
6. git diff --check
- e2e：本机若有真实 Claude Code + 登录态则跑 ignored，否则记录 skip 原因。

## 进度
- [x] adapter.rs 实现（含首turn时序修正 + first_turn_pending）
- [x] mod / 导出
- [x] 内联离线测试（15 个 claude_code_adapter_*，全过）
- [x] ignored e2e（tests/external_claude_code.rs）
- [x] 文档（managed-external-agent §12.1/§12.3 时序修正 + capability-matrix）
- [x] 验证序列（fmt/clippy on+off/full suite feature-on 42 ok/doc on+off/diff --check 全过）
- [x] 本机真机 e2e 实跑通过（10 观测事件，started→text→completed，23s，graceful shutdown）
- [ ] TODO.md [DONE] + 完成记录
- [ ] commit

## 关键发现（真机）
Claude Code `--print --output-format stream-json --input-format stream-json` 模式下，CLI 在读到第一条
stdin `user` 帧前不发任何 frame（含 `system/init`）。最初的 adapter 假设 init 先于 stdin，导致 start 的
prelude 读阻塞到超时（`SessionLost{TimedOut}`）。已按真实时序修正 begin/advance（见上），真机 e2e 随后
跑通。权限桥接在本次非交互运行中未触发 `control_request`（CLI 直接阻塞受管工具并在文本里说明），故
真机未覆盖 permission 分支；该分支由离线单测（control_response 映射 + text→permission→completion 回放）覆盖。
