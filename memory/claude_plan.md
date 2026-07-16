# M1-1 — 定义 `StepError` 内部错误类型与 `From` 转换

**当前执行 TODO.md 第一个未完成任务 = M1-1**（新任务单「Effect 层清理（三刀重构）」，
M1-1 是首个 `[TODO]`）。这是刀 (C) 的第一步:只定义类型,不改调用点。

## 任务要求（TODO.md M1-1）
1. 在 `src/agent/machine/default/` 内定义仅 crate 内可见、不对外暴露的 `StepError` 枚举。
2. 为每个非 `Protocol` 变体实现 `From<...>`,让 `?` 可用;显式分别实现 `From<ConversationError>`
   与 `From<AgentStateError>`,映射到不同变体(避免经 `AgentStateError: From<ConversationError>`
   的歧义)。
3. 提供 `message()` 复刻现有文案前缀,折叠后落在 `ErrorCursor` 的文本逐字节一致。

## 现有文案前缀（来自 mod.rs 的 self.fail(format!(...))）
- `conversation operation failed: {error}`  <- ConversationError
- `agent state operation failed: {error}`   <- AgentStateError（state 操作）
- `cursor transition failed: {error}`        <- AgentStateError（transition_cursor）★同类型不同文案
- `tool runtime operation failed: {error}`   <- ToolRuntimeError
- `requirement id unavailable: {error}`      <- RequirementError
- 其余纯协议违例走 `Protocol(String)` 原样透传。

## 设计决定
`cursor transition failed` 与 `agent state operation failed` 都是 `AgentStateError` 但文案不同,
单一 `From<AgentStateError>` 无法同时复刻两种文案。因此在建议 5 变体基础上**新增
`CursorTransition(AgentStateError)` 变体**(TODO 允许「字段名可微调」,且 M1-1 明确要求 message()
能复刻 `cursor transition failed`)。`From<AgentStateError>` -> `State`(默认 `?` 路径);cursor
transition 站点在 M1-2 用 `.map_err(StepError::CursorTransition)` 显式构造,不算 workaround。

新增私有子模块 `src/agent/machine/default/error.rs`(mod.rs 加 `mod error;`),提高模块化:
- `pub(super) enum StepError { Conversation, State, CursorTransition, ToolRuntime, Requirement, Protocol }`
- `From<ConversationError|AgentStateError|ToolRuntimeError|RequirementError> for StepError`
- `pub(super) fn message(&self) -> String`
- 本任务未接线 -> 全部 `#[allow(dead_code)]`(M1-2 移除)。

错误类型路径:`crate::agent::{AgentStateError, RequirementError, ToolRuntimeError}`、
`crate::conversation::ConversationError`。

## 验证条件（M1-1）
- `cargo build` 通过。
- `cargo test -p agent-lib agent::machine::default` 全绿(调用点未改)。
- `cargo fmt --all -- --check`、`cargo clippy --all-targets -- -D warnings` 通过。
- `git diff --check` 干净。

## 完成后
TODO.md M1-1 标 `[DONE]` + 完成记录;commit `[M1-1] ...`;停(不做 M1-2)。

## 进度
- [x] 读 TODO/源码,确定文案前缀与类型路径
- [x] 新增 error.rs + `mod error;`
- [x] fmt/clippy/build/聚焦测试(+doc) 全绿
- [x] TODO.md 标 DONE + commit
