# M8-4 Review：OpenCode adapter 正确性检查

**当前任务 = TODO.md 第一个未完成任务 = M8-4**（TODO.md `### [TODO] M8-4`，line 2595）。
M8-1/M8-2/M8-3 已 `[DONE]`。M8-4 = review：对比三个 runtime adapter 一致性 + 更新 capability-matrix OpenCode 行。

## Review 结论（已逐项核对源码）

### 1. trait 实现一致 ✓
- 三者都 impl `ExternalRuntimeAdapter`(kind/capabilities/start/resume) + `ExternalRuntimeSession`
  (session_ref/advance/shutdown)。
- **Codex 与 OpenCode 同构**（自主、一进程/一 turn）：begin/read_line/drain_and_emit/
  spawn_follow_up_turn/finish(仅 Completed/Failed)/advance(cancel→SessionLost)/shutdown
  (close stream 或 Graceful) 结构逐行一致；OpenCodeLauncher/OpenCodeTurnStream 镜像
  CodexLauncher/CodexTurnStream。
- **Claude Code 差异是设计意图**：常驻 stdio 进程（非一进程/一 turn），finish 多出
  PausedForToolCalls/PausedForInteraction 臂（permission bridge），write_input 替代
  spawn_follow_up_turn，shutdown 直接关 io。

### 2. capability fallback 一致 ✓
- 三者：new()→implemented_capabilities()；with_probed_capabilities()→intersect_capabilities()
  （逐位 AND，helper 三份逐行一致）。
- 三者：reject_unsupported_tools(host_tools 门禁)+turn_message 拒绝式
  (PermissionBridge/HostTools/HostSubagents→UnsupportedCapability，Shutdown→Protocol)。
- 能力位：Claude={streaming,resume,permission_bridge,artifacts,usage,graceful=true;
  host_tools,host_subagents=false}；Codex & OpenCode={streaming,resume,artifacts,usage,graceful=true;
  permission_bridge,host_tools,host_subagents=false}。唯一差异 = permission_bridge，已文档化。

### 3. parser cassette 覆盖层级一致 ✓
- 三份 agent_<rt>_cassette.rs 各 7 个并行层：regenerate_fixture / matches_in_code_builder /
  is_secret_free / decodes_full_session / tolerates_unknown_and_blank_frames /
  rejects_malformed_frames / decodes_*_as_failed。各有 committed fixture。
- inline adapter 单测同构；OpenCode 多 resume_survives_a_session_that_never_re_reports_its_id（无 init 帧特性）。

### 4. cleanup/trace 一致 ✓
- 三者 advance 均先 ctx.is_cancelled()→SessionLost；shutdown→ExternalSessionShutdown。
- adapter 层不自发 tracing（trace 经 RunContext trace node 透传）。
- session_ref 均暴露 session_id + resume_token(=session id) + last_event_seq 高水位去重。

## 交付物
1. docs/capability-matrix.md：OpenCode 小节标注 M8-4 review 定案 + 新增三 adapter 统一接入路径对照表。
2. TODO.md M8-4 → [DONE] + 完成记录（OpenCode 支持/不支持能力 + 真实 e2e 状态）。

## 验证序列
1. cargo fmt --all -- --check
2. cargo clippy --all-targets --features external-opencode -- -D warnings
3. cargo test --features external-opencode -p agent-lib opencode_cassette
4. cargo test --features external-opencode -p agent-lib --lib opencode
5. git diff --check
6. 全量 cargo test --all --all-targets：M8-4 仅改 docs，无编译产物变化，复用 M8-3 绿结果。

## 进度
- [x] 调研 + 逐项一致性核对（trait/fallback/cassette/cleanup）
- [x] 更新 capability-matrix.md OpenCode 行 + 统一对照表
- [x] 目标验证（fmt/clippy/opencode 测试 + 全量 cargo test --all --all-targets exit 0）
- [x] TODO.md DONE + commit
