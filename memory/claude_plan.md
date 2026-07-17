# M4-3 扩展 `ExternalSessionPolicy` / `ExternalAgentSpec` 支持 managed mode 配置

**当前执行 = TODO.md 第一个未完成任务 = M4-3**（M1..M3、M4-1、M4-2 已 `[DONE]`）。

## 任务分析（TODO.md M4-3 body，权威）
- 现状：`ExternalSessionPolicy` 只有 permission/isolation/max_turns/stream_events（runtime-facing hints，保留不动）。
- machine 需要 machine-local policy：tool failure policy、capability requirement、loop limit。
- 不能让 `ExternalAgentMachine::new` 参数膨胀 → 用独立 config + builder。
- 约束：live handler/sink 不进 serializable `ExternalAgentState`；默认 config 与当前行为兼容。

## 方案（machine-local config，与 runtime-facing policy 分离）
1. 新增 `src/agent/external/config.rs`：
   - `ExternalToolFailurePolicy`（`ReturnErrorToRuntime` 默认 / `StopRun`，snake_case serde）。
   - `ExternalAgentMachineConfig`（serde DTO，纯数据，Default）：
     - `tool_failure: ExternalToolFailurePolicy`
     - `required_capabilities: BTreeSet<ExternalCapability>`（覆盖 require host tools / require subagents，"capability set" 形态）
     - `max_decision_loops: Option<u32>`（None = 不限，兼容当前行为）
     - builder：`with_tool_failure_policy` / `with_max_decision_loops` / `require_capability` /
       `require_host_tools` / `require_subagents`；accessor：`tool_failure` / `requires(cap)` /
       `max_decision_loops` / `required_capabilities`。
   - `ExternalCapability` 加 `PartialOrd, Ord`（BTreeSet 需要）。
2. `mod.rs`：`mod config;` + `pub use config::{ExternalAgentMachineConfig, ExternalToolFailurePolicy};`。
3. `src/agent/mod.rs`：re-export 两个新类型。
4. `state.rs`：`ExternalAgentState` 新增持久化计数 `decision_loops: u32`
   （record 用 `#[serde(default, skip_serializing_if = is_zero)]` → 干净态字节兼容；`deny_unknown_fields`
   下旧快照 default=0）；accessor `decision_loops()` + `record_decision_loop() -> u32`。
   计数是纯数据、可序列化——不是 live handler/sink，符合约束；跨 restore 存活。
5. `machine.rs`：
   - `ExternalAgentMachine` 新增 `config: ExternalAgentMachineConfig`（`new` 用 Default）。
   - builder：`with_external_config` / `with_tool_failure_policy` / `with_max_decision_loops`（保留 `with_tool_execution_ids`）。
   - `block_on_session`：先 `record_decision_loop()`，超过 `max_decision_loops` → `LimitExceeded` fail。
     （所有 session round-trip 唯一漏斗：begin/RespondToolResults/RespondInteraction/RespondSubagent）
   - `pause_for_tool_calls` 两处 tool-id mint 失败：`config.requires(HostTools/HostSubagents)` 时
     升级为 classified `UnsupportedCapability`，否则保留原 "tool id unavailable"（默认兼容）。
   - `resume_tool` 的 `Tool(Err)`：`StopRun` → fail turn；`ReturnErrorToRuntime`（默认）→ 原行为回灌。

## 测试
- config.rs：`external_machine_config_roundtrip`（serde DTO round-trip + default 语义）。
- machine/tests.rs：
  - `external_loop_limit_fails_before_unbounded_pause_loop`（TODO 指定）。
  - `external_tool_failure_stop_run_fails_turn`。
  - `external_require_host_tools_reports_unsupported_capability`。
  - `external_require_subagents_reports_unsupported_capability`。
- state.rs：断言 default `decision_loops` 不进快照（可复用现有 round-trip 断言 forbidden keys 思路）。

## 验证条件（TODO.md）+ 完整序列 1-6
- 默认 config 与当前行为兼容；config serde round-trip；loop 超限 unit test。
- 1 fmt / 2 焦点测试 / 3 clippy -D warnings / 4 全量 test / 5 doc -D warnings / 6 git diff --check。

## 文档
- `docs/managed-external-agent.md` §7：把 config 由「拟新增」改为「已落地(M4-3)」，写实际字段与 builder。

## 状态：已完成（M4-3 [DONE]）
