# M4-2 新增 `ExternalRuntimeCapabilities` 与 unsupported capability 错误

**当前执行 = TODO.md 第一个未完成任务 = M4-2**（M1..M3、M4-1 已 `[DONE]`）。

## 结构修复（前置，必须先做）
- 上一 commit `69a0060 [M4-1]` 在插入 M4-1 完成记录时**误删**了
  `### [TODO] M4-2 新增 \`ExternalRuntimeCapabilities\` 与 unsupported capability 错误` 标题，
  导致 M4-2 的 body（上下文/做什么/验证条件，TODO.md 约 1209-1254）变成 M4-1 的孤儿续写。
- 属于「任务条目本身损坏，必须结构性修复」——恢复 M4-2 标题（放回其 body 之前），不新增/拆分任务。

## 任务分析（TODO.md M4-2 body + docs §15）
- 现状 `ExternalAgentError` 无 `UnsupportedCapability`；无 session-level capability model。
- TODO body（权威）定义 8 能力：streaming/resume/permission_bridge/host_tools/host_subagents/
  artifacts/usage/graceful_shutdown —— 与 M4-4 review 清单逐项一致。
- docs §15 是更细的「拟新增」草图（13 字段 + capability:String）。以 TODO body 为准（enum capability，
  8 能力集），与 M4-4 acceptance 对齐；PLAN 非目标要求「能力差异显式暴露、不静默假装支持」→ 保守默认全 false。

## 方案
1. 新增 `src/agent/external/capability.rs`：
   - `ExternalCapability` enum（8 变体，snake_case serde，Copy/Hash，`as_str`/`Display`，`ALL` 常量）。
   - `ExternalRuntimeCapabilities`（runtime + 8 bool，serde）：`none(runtime)` 保守全 false；
     `supports(cap)`；`require(cap, detail) -> Result<(), ExternalAgentError>` 产 classified error。
   - `impl ExternalRuntimeKind { conservative_capabilities() }` 保守 helper。
2. `mod.rs`：`mod capability;` + `pub use capability::{ExternalCapability, ExternalRuntimeCapabilities};`；
   `ExternalAgentError` 新增 `UnsupportedCapability { runtime, capability: ExternalCapability, detail }`
   （`#[error("{runtime:?} runtime does not support {capability}: {detail}")]`，不含 prompt/tool input 字段）。
3. `src/agent/mod.rs`：re-export 新类型。
4. 测试：
   - `capability.rs`: `external_capabilities_roundtrip`（serde round-trip + none/supports/ALL）。
   - `mod.rs`: `external_error_roundtrips`（全 error 变体 round-trip + UnsupportedCapability Display 不泄漏 prompt/tool input）。

## 验证条件（TODO.md）
- capability/error DTO serde round-trip。
- `UnsupportedCapability` Display 不含 raw prompt/tool input。
- `cargo test -p agent-lib external_capabilities_roundtrip`
- `cargo test -p agent-lib external_error_roundtrips`
- 完整验证序列 1-6：fmt / 焦点测试 / clippy -D warnings / 全量 test / doc -D warnings / git diff --check。

## 状态：已完成（M4-2 [DONE]）
- 结构修复 commit c9df411 恢复 M4-2 标题；实现 commit 本轮提交。
- 完整验证序列 1-6 全过。下一任务 = M4-3，本轮不启动。
