# M5-3 增加 cassette replay 层用于 runtime parser 回归

**当前执行 = TODO.md 第一个未完成任务 = M5-3**（M1..M4、M5-1、M5-2 均 `[DONE]`）。

## 任务理解
runtime adapter parser（M6-M8 才实现）会消费真实 CLI 的 JSON/JSONL 输出，需要
cassette 冻结「原始帧 → 解析出的 ExternalObservedEvent + decision point」以防协议漂移。
这是一个**独立于 M3 effect-level cassette** 的新概念（runtime parser cassette）。

## 目标（TODO.md 做什么）
1. 定义 cassette 格式：runtime kind/version/probe、input frames、expected
   ExternalObservedEvent + decision point、redaction metadata。
2. cassette loader/parser test helper（磁盘加载 + schema 版本护栏 + 未知字段保守保留）。
3. 最小 synthetic cassette 覆盖：text delta / command start-finish / permission request /
   tool call / completion。
4. 预留目录：tests/fixtures/external/{claude_code,codex,opencode}/（暂不需真实 cassette）。

## 验证条件
- loader 对未知字段保守：raw 保留（#[serde(flatten)] extra）并测试。
- redaction test 确认 fixture 不含 API_KEY / AUTH_TOKEN / sk- 等。
- `cargo test -p agent-lib external_cassette`
- 完整验证序列 1-6 全过。

## 设计
### 新模块 crates/agent-testkit/src/external/cassette.rs
Schema（serde）:
- `EXTERNAL_CASSETTE_SCHEMA_VERSION: u32 = 1`
- `ExternalRuntimeCassette { schema_version, runtime: CassetteRuntimeInfo, redaction:
  RedactionMetadata(default), turns: Vec<CassetteTurn>(default), #[flatten] extra }`
- `CassetteRuntimeInfo { kind: ExternalRuntimeKind, version?, probe?, session_id?, #[flatten] extra }`
- `RedactionMetadata { applied(default false), placeholder?, notes? }`
- `CassetteTurn { expect_input?: CassetteInputKind, input_frames: Vec<CassetteFrame>(default),
  expected_events: Vec<ExternalObservedEvent>(default), decision: CassetteDecision, #[flatten] extra }`
- `CassetteFrame { stream: CassetteStream(default Stdout), payload: String, #[flatten] extra }`
- `CassetteStream { Stdout, Stderr }`（snake_case）
- `CassetteInputKind { Start, Continue, RespondInteraction, RespondToolResults, RespondSubagent,
  Shutdown }`（snake_case）+ matches(&ExternalSessionInput) + expected(): assertions::ExternalInputKind
- `CassetteDecision`（tag="kind", snake_case）{ Completed{output}, PausedForInteraction{action_id,request},
  PausedForToolCalls{batch_id,calls}, PausedForSubagent{request}, Failed{error} }

Loader / error（手写 Display/Error，仿 M3，testkit 无 thiserror）:
- `ExternalRuntimeCassette::{from_json_str, load(path), to_json_string[_pretty]}`；schema_version 先读后 parse。
- `ExternalCassetteError { Serialize, Deserialize, Io{path,err}, MissingSchemaVersion, UnsupportedSchemaVersion{found,supported} }`

Redaction:
- `SECRET_PATTERNS`（含 API_KEY / AUTH_TOKEN / sk- / Bearer  / -----BEGIN）
- `scan_secrets(&str) -> Vec<SecretHit{pattern,offset}>`
- `ExternalRuntimeCassette::assert_no_secrets()`（序列化后扫描，命中则 panic 列出）

Replay 层（复用 M5-1 registry + M5-2 ScriptedSinkLog/ScriptedRuntimeStartLog + ExternalAgentCallLog）:
- `CassetteExternalRuntimeSession`(impl ExternalRuntimeSession)：VecDeque<CassetteTurn>；advance
  弹一 turn、断言 expect_input、**按记录 seq 原样** emit expected_events 到 sink 且缓冲为
  observations、last_event_seq = max seq、由 CassetteDecision 生成 RuntimeDecisionPoint（Failed->Err）。
- `CassetteExternalRuntimeAdapter`(impl ExternalRuntimeAdapter)：capabilities.resume=false，start 领 turns、记 start log。
- `CassetteRuntimeExternalSessionHandler`(impl ExternalSessionHandler)：registry-backed，同 scripted 形状。
- `CassetteRuntimePlayer::load(cassette) -> handler`（+ log/sink/start_log/registry 访问器）。

### fixtures
- `tests/fixtures/external/synthetic/full_stream.json`：单 turn Start→Completed，observations 覆盖
  text_delta/command_started/command_finished/permission_requested/tool_started/tool_finished/session_completed；
  drain 到 Done 无需 tool/interaction handler。由 env-gated 生成器写出后提交。
- `tests/fixtures/external/synthetic/forward_compat.json`：含未知顶层/turn/frame 字段，测保守保留。
- `tests/fixtures/external/{claude_code,codex,opencode}/README.md`：预留目录。

### 测试
- `tests/agent_external_cassette.rs`：`external_cassette_*`
  - loads_synthetic_fixture（磁盘加载 + 字段断言）
  - replay_drains_to_done（加载 full_stream → drain → Done + sink seq）
  - replay_tool_batch / interaction / subagent（in-code 造 cassette，JSON round-trip 后 drain）
  - rejects_unknown_schema_version / preserves_unknown_fields（forward_compat）
  - fixtures_are_redacted（扫描磁盘所有 synthetic fixture 原文 + assert_no_secrets）
  - regenerate_fixtures（env-gated 生成器，正常 no-op）
- cassette.rs 内 `#[cfg(test)]` 单测若干（cassette_* 前缀，走 -p agent-testkit）。

### 导出
- external/mod.rs `pub mod cassette;` + re-export；prelude.rs 追加导出。

## 验证序列（TODO.md 1-6）
1. `cargo fmt --all -- --check`
2. `cargo test -p agent-lib external_cassette`
3. `cargo clippy --all-targets -- -D warnings`
4. `cargo test --all --all-targets`（有代码改动，≤30min）
5. `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`
6. `git diff --check`

## 状态：完成
- [x] 实现 cassette.rs（schema/loader/redaction/replay）
- [x] 生成并提交 synthetic fixtures + 预留目录
- [x] 新增 tests/agent_external_cassette.rs（9 个 external_cassette_*）
- [x] mod/prelude 导出 + ScriptedRuntimeStartLog::record 提 pub(crate)
- [x] 验证序列 1-6 全过（fmt/focus 9passed/clippy/full-suite 40 ok 0 failed/doc/diff-check clean）
- [x] 标记 [DONE] + 完成记录写入 TODO.md
- [ ] 提交 `[M5-3] ...`（本轮最后一步）
