# TODO：Effect 层清理(三刀重构)任务单

> 依据 [`PLAN.md`](PLAN.md) 与 [`docs/effect-refine.md`](docs/effect-refine.md)。任务按实现顺序编号
> (`M<里程碑>-<序号>`);coding agent 每次只执行首个标题带 `[TODO]` 的任务,完成后把该标题的 `[TODO]`
> 改为 `[DONE]`,并在任务末尾补充「完成记录」。
>
> 上一轮任务单(External Agent 接入)已归档在
> [`docs/archive/2026-07-17-external-agent/`](docs/archive/2026-07-17-external-agent/)。

通用约束:三刀都**不得改变 `agent-lib` 任何对外运行时语义**——失败仍落在 `LoopCursor::Error` 且
quiescent,`step` 的 sans-io 契约、`HandlerScope`/`Pop`/`drain` 路由、never-resume cancel 语义保持
不变;不得改 `LoopCursor` 的 serde 形状(刀 (B) 走落点 2);新增公开 API 必须带 rustdoc;宏生成项的
rustdoc 需可编译。核心验收标准:**现有测试无需修改断言即全绿**(顺带丰富诊断时只允许新增断言)。

**默认完整验证序列**(除非任务另行放宽):

1. `cargo fmt --all -- --check`
2. 聚焦测试(任务内给出精确过滤名)
3. `cargo clippy --all-targets -- -D warnings`
4. `cargo test --all --all-targets`
5. `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`
6. `git diff --check`

---

## Milestone 1 — 刀 (C):给 `step` 内部一个 `Result` 层(设计文档 §2)

> 目的:把 `DefaultAgentMachine` 里数十处 `if let Err(..) return self.fail(..)` 塌缩成 `?`,只在
> `step()` 最外层一处把 `Err` 折成 `LoopCursor::Error`。语义零变化、序列化零风险、只动
> `src/agent/machine/default/`。当前 `mod.rs` 有 33 处 `self.fail*`、10 处 `if let Err`,`tools.rs`
> 有 32 处 `self.fail*`。

### [DONE] M1-1 定义 `StepError` 内部错误类型与 `From` 转换

**前置依赖**:无。

**上下文**:

`AgentMachine::step`(`src/agent/machine/default/mod.rs:852`)返回 `StepOutcome`,不能返回 `Result`,
所以每个 fallible 调用当前手写成 `if let Err(error) = ... { return self.fail(format!("...: {error}")); }`。
fallible 调用会返回这些错误类型:

- `AgentStateError`(`src/agent/state.rs:483`)——来自 `transition_cursor`(`state.rs:242`,返回
  `Result<(), AgentStateError>`)、`queued_reconfig_application`、`plan_reconfig_with`、pivot
  `validate`。
- `ConversationError`(`src/conversation/error.rs:1255`)——来自 `begin_turn` /
  `start_assistant_response` / `finish_assistant` / `commit_pending` / `append_tool_response` /
  `register_tool_calls` / `inject_user_message` / `cancel_pending`。注意 `AgentStateError` 已经
  `#[from] ConversationError`(`state.rs:485-486`),但机器里很多 `conversation_mut()` 调用直接返回
  `ConversationError`。
- `ToolRuntimeError`(`src/agent/tool.rs`)——来自 `ToolExecutionIds` 的 `tool_call_id` /
  `tool_result_message_id` / `next_assistant_message_id` / `next_step_id`(`tool.rs:90-115`)、以及
  `RequirementResult::Reconfig(Err(_))` / `Tool(Err(_))` 的 `StopRun` 路径。
- `RequirementError`(`src/agent/requirement.rs:535`)——来自
  `requirement_ids.next_requirement_id(..)`(`requirement.rs:174`)。
- 纯字符串的**协议违例**(如「pivot injection requires a streaming step boundary」「resume received
  while cursor is ...」「missing in-flight assistant message id」),当前直接 `self.fail("...")`。

**做什么**:

- 在 `src/agent/machine/default/mod.rs` 顶部(或新增私有 `error.rs` 子模块,由 `mod.rs` `mod error;`
  引入)定义一个**仅 crate 内可见、不对外暴露**的错误枚举,建议形状(字段名可微调):

  ```rust
  /// 机器内部一步计算的失败:携带分类信息与人读消息。
  /// 只在 step() 最外层被折叠成 LoopCursor::Error,不对外暴露。
  #[derive(Debug)]
  enum StepError {
      Conversation(ConversationError),
      State(AgentStateError),
      ToolRuntime(ToolRuntimeError),
      Requirement(RequirementError),
      /// 语义违例(如 resume 落在错误的 cursor 上、缺失 in-flight scratch)。
      Protocol(String),
  }
  ```

- 为每个非 `Protocol` 变体实现 `From<...> for StepError`,让 `?` 直接可用。注意 `AgentStateError`
  已含 `From<ConversationError>`;为避免 `?` 在 `ConversationError` 上产生歧义,**显式分别实现
  `From<ConversationError>` 与 `From<AgentStateError>`**,并保持二者映射到不同的 `StepError` 变体。
- 提供一个把 `StepError` 转成稳定人读字符串的方法(`fn message(&self) -> String` 或 `Display`),复刻
  现有 `self.fail(format!(...))` 的文案前缀(如 `conversation operation failed: {error}`、
  `agent state operation failed: {error}`、`cursor transition failed: {error}`、
  `tool runtime operation failed: {error}`、`requirement id unavailable: {error}`),使折叠后落在
  `ErrorCursor` 的文本与现状**逐字节一致**(现有测试若断言了错误文案则不能变)。先 `grep` 出现有测试
  对错误文案的断言(`rg "loop_cursor|ErrorCursor|Error\(" src/agent/machine/default/tests/`),确认要
  保留的确切文案。

**验证条件**:

- 新增 `StepError` 及其 `From`/`message` 实现;`cargo build` 通过。
- 本任务尚未改造调用点,故所有现有测试仍全绿:`cargo test -p agent-lib agent::machine::default`。
- `cargo fmt --all -- --check`、`cargo clippy --all-targets -- -D warnings` 通过(允许暂时的
  `#[allow(dead_code)]`,若本任务内 `StepError` 尚未被使用;M1-2 会移除)。
- `git diff --check` 干净。

**完成记录**:

- 新增私有子模块 `src/agent/machine/default/error.rs`（mod.rs 增 `mod error;`，提升模块化，
  不改任何调用点），定义 crate 内可见、不对外暴露的 `pub(super) enum StepError`：
  - `Conversation(ConversationError)`、`State(AgentStateError)`、
    `CursorTransition(AgentStateError)`、`ToolRuntime(ToolRuntimeError)`、
    `Requirement(RequirementError)`、`Protocol(String)`。
- **设计增量**：在建议 5 变体基础上新增 `CursorTransition(AgentStateError)`。因为
  `transition_cursor` 与其它 state 操作同为 `AgentStateError`，但历史文案分别是
  `cursor transition failed:` 与 `agent state operation failed:`；单一 `From<AgentStateError>`
  无法同时逐字节复刻两种前缀。故 `From<AgentStateError>` 默认映射到 `State`（供裸 `?` 使用），
  cursor-transition 站点在 M1-2 用 `.map_err(StepError::CursorTransition)` 显式构造（非
  workaround，仅是文案分流）。此变体不改变任何对外语义。
- 显式实现 `From<ConversationError>`、`From<AgentStateError>`、`From<ToolRuntimeError>`、
  `From<RequirementError>`（因 `AgentStateError: From<ConversationError>`，两者分别显式实现以
  避免 `?` 歧义，并映射到不同变体）。
- `pub(super) fn message(&self) -> String` 逐字节复刻现有 `self.fail(format!(..))` 前缀：
  `conversation operation failed:` / `agent state operation failed:` / `cursor transition failed:`
  / `tool runtime operation failed:` / `requirement id unavailable:`；`Protocol` 原样透传。
- 本任务未接线调用点，`error.rs` 顶部加临时 `#![allow(dead_code)]`（含注释说明 M1-2/M1-3 移除）；
  所有新增项均带 rustdoc（含 `intra-doc` 链接，`cargo doc -D warnings` 通过）。
- 验证：`cargo fmt --all -- --check` 干净；`cargo build` 通过；
  `cargo clippy --all-targets -- -D warnings` 无告警；`cargo test -p agent-lib --lib
  agent::machine::default` 39 passed / 0 failed（断言未改）；
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` 无告警；`git diff --check` 干净。
  （M1-1 明确放宽为聚焦测试，未跑 `cargo test --all`；调用点零改动，运行时语义不变。）

### [DONE] M1-2 把 `mod.rs` 的 fallible 方法改造为返回 `Result<StepOutcome, StepError>`

**前置依赖**:M1-1。

**上下文**:

改造范围是 `src/agent/machine/default/mod.rs` 里除 `tools.rs` 外的全部 fallible 方法(见 PLAN.md 锚点
清单):`begin_user_turn`@299、`open_user_turn`@353、`inject_pivot`@384、`block_on_llm`@435、
`resume`@468、`resume_llm`@494、`fold_llm_response`@525、`commit_text_turn`@565、
`finalize_text_commit`@591、`emit_reconfig_effect`@636、`resume_reconfig`@669、`abandon`@733、
`abandon_llm_step`@768、`abandon_reconfig`@787、`finish_cancel`@803。

目标形状(设计文档 §2.2):

```rust
fn open_user_turn(&mut self, user: AgentUserInput) -> Result<StepOutcome, StepError> {
    self.state.conversation_mut().begin_turn(
        user.turn_id(), user.message_id(), user.message().clone(),
    )?;                              // ← if-let-Err 塌缩成一个 ?
    self.in_flight = Some(InFlight::new(user.assistant_message_id()));
    self.block_on_llm(user.step_id(), Vec::new())   // block_on_llm 也返回 Result
}
```

**做什么**:

- 把上述方法签名改为返回 `Result<StepOutcome, StepError>`,把内部 `if let Err(error) = ... { return
  self.fail(format!("...: {error}")); }` 改成 `...?`。纯协议违例(cursor 相位不对、缺 scratch、
  `other => self.fail(...)` 的 result-family 不匹配分支)返回 `Err(StepError::Protocol(...))`,文案
  保持与现状一致。
- `begin_user_turn` 顶部的 `Done/Error → Idle` 复位、`Idle` 且有 pending 时的 `cancel_pending`——这些
  也用 `?`。
- **保留 `fail()` 与 `fail_with_notifications()` 方法本身不动**(供 `tools.rs` 与最外层折叠复用,
  且 M1-3 会新增 `fail_from`)。
- 本任务**不改 `tools.rs`、不改 `step()` 最外层**;`step()` 最外层折叠留到 M1-3。为让编译通过,在 M1-3
  之前 `step()` 需临时对返回 `Result` 的方法 `.unwrap_or_else(|e| self.fail(e.message()))` 或等价桥接
  (M1-3 会替换为正式的 `fail_from`)。桥接代码要显式标注 `// M1-3 will replace with fail_from`。
- 注意方法间调用链:`begin_user_turn → open_user_turn → block_on_llm`、`fold_llm_response →
  commit_text_turn → finalize_text_commit`、`resume_reconfig → open_user_turn/finalize_text_commit`
  等,内层改成 `Result` 后外层的调用点直接 `?` 或 `return` 传播即可。

**验证条件**:

- 上述方法全部返回 `Result<StepOutcome, StepError>`,`mod.rs` 里针对这些方法的 `if let Err` 数量显著
  下降(目标:从 10 处降到接近 0,仅 `fail_with_notifications` 内部保留必要的 `let _ =`)。
- `cargo test -p agent-lib agent::machine::default` 全绿,**断言未修改**。
- `cargo test --all --all-targets` 全绿。
- 完整验证序列 1–6 全过。

**完成记录**:

- 将 `mod.rs` 中除 `tools.rs` 外的 15 个 fallible 方法签名改为
  `-> Result<StepOutcome, StepError>`：`begin_user_turn`、`open_user_turn`、`inject_pivot`、
  `block_on_llm`、`resume`、`resume_llm`、`fold_llm_response`、`commit_text_turn`、
  `finalize_text_commit`、`emit_reconfig_effect`、`resume_reconfig`、`abandon`、
  `abandon_llm_step`、`abandon_reconfig`、`finish_cancel`。方法体内 `if let Err(error) = ..
  { return self.fail(format!("..: {error}")); }` 全部塌缩为 `?`；`mod.rs` 的 `if let Err`
  计数从 10 降至 **0**。
- **错误变体路由**（保证折叠文案逐字节不变）：
  - `ConversationError`（`begin_turn` / `cancel_pending` / `inject_user_message` /
    `start_assistant_response` / `finish_assistant` / `commit_pending`）经裸 `?` →
    `From<ConversationError>` → `Conversation` → `conversation operation failed:`。
  - `queued_reconfig_application` 与 `pivot.validate()`（均 `AgentStateError`）经裸 `?` →
    `From<AgentStateError>` → `State` → `agent state operation failed:`。
  - `transition_cursor`（同为 `AgentStateError` 但历史文案不同）用
    `.map_err(StepError::CursorTransition)?` → `cursor transition failed:`。
  - `next_requirement_id`（`RequirementError`）经裸 `?` → `Requirement` →
    `requirement id unavailable:`。
  - `RequirementResult::Reconfig(Err(error))` 的 `error` 经核实为 `ToolRuntimeError`
    （`requirement.rs:485`），改用 `StepError::ToolRuntime(error)` →
    `tool runtime operation failed:`（与旧 `self.fail(format!(..))` 逐字节一致）。
  - 纯协议违例与 `client operation failed:`（`ClientError`，非 typed 前缀）走
    `StepError::Protocol(format!(..))` 原样透传。
- **跨模块桥接**（M1-2 不改 `tools.rs` 失败路径、不做 `step()` 正式折叠，均留给 M1-3/M1-4）：
  - `mod.rs`(Result) 调 `tools.rs`(仍 `StepOutcome`) 处包 `Ok(..)`：`resume` 里
    `resume_tool`/`resume_approval`、`fold_llm_response` 里 `begin_tool_phase`、`abandon`
    里 `abandon_tool_phase`。
  - `tools.rs`(`StepOutcome`) 调现返回 `Result` 的 `mod.rs` 方法两处
    （`finish_tool_phase→block_on_llm`、`abandon_tool_phase→finish_cancel`）临时桥接
    `.unwrap_or_else(|error| self.fail(error.message()))`，注释标注 `// M1-3 will replace
    with fail_from`。
  - `step()` 最外层临时桥接 `result.unwrap_or_else(|error| self.fail(error.message()))`，
    同样标注 `// M1-3 will replace with fail_from`。
- `fail()` / `fail_with_notifications()` 方法体保留不动（供 `tools.rs` 与桥接复用，M1-3 再加
  `fail_from`）。移除 `error.rs` 顶部临时 `#![allow(dead_code)]`（变体已全部接线）并更新其模块
  doc 注释。
- 验证（完整序列 1–6 全过）：`cargo fmt --all -- --check` 干净；`cargo build` 通过；
  `cargo test -p agent-lib --lib agent::machine::default` 39 passed / 0 failed（**断言未改**）；
  `cargo clippy --all-targets -- -D warnings` 无告警；`cargo test --all --all-targets` 全绿；
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` 通过；`git diff --check` 干净。

### [DONE] M1-3 在 `step()` 最外层收敛错误折叠(`fail_from`)

**前置依赖**:M1-2。

**上下文**:

设计文档 §2.2 的最终形状:`step()` 是**唯一的错误折叠点**。

```rust
fn step(&mut self, input: StepInput) -> StepOutcome {
    let result = match input {
        StepInput::External(AgentInput::UserMessage(user)) => self.begin_user_turn(user),
        StepInput::External(AgentInput::Pivot(pivot))       => self.inject_pivot(pivot),
        StepInput::Resume(resolution)                       => self.resume(resolution),
        StepInput::Abandon(id)                              => self.abandon(id),
    };
    match result {
        Ok(outcome) => outcome,
        Err(error) => self.fail_from(error),   // = 旧 fail() 的收尾语义 + 分类
    }
}
```

**做什么**:

- 新增 `fn fail_from(&mut self, error: StepError) -> StepOutcome`,复用现有 `fail()` 的收尾(discard
  pending → `cancel_pending(DiscardTurn)` → 清 `in_flight` → 迁 `LoopCursor::Error` → quiescent),
  错误文案取 `error.message()`,与现状逐字节一致。
- 把 `step()`(`mod.rs:852`)改成上面的形状,移除 M1-2 里的临时桥接。
- `fail()` / `fail_with_notifications()` 保留:前者仍被内部少数直接失败点与 `tools.rs` 使用,后者服务
  「带副产品 notification 的失败」(见 M1-4)。
- (可选,设计文档 §2.2)顺带把 `StepError` 的分类信息记进 `ErrorCursor` 以增强可诊断性。**若这样做,
  只能新增断言,不能改动现有断言**;不确定则跳过,保持 `ErrorCursor` 仅 `message`。

**验证条件**:

- `step()` 是唯一把 `StepError` 折成 `Error` cursor 的地方;`mod.rs` 里不再有临时桥接。
- 失败行为逐字节不变:构造一个会触发失败的输入(如在非 streaming cursor 上注入 pivot),断言落在
  `LoopCursor::Error` 且 message 与改造前一致。`cargo test -p agent-lib agent::machine::default` 全绿。
- 完整验证序列 1–6 全过。

**完成记录**:

- 在 `src/agent/machine/default/mod.rs` 新增私有 `fn fail_from(&mut self, error: StepError)
  -> StepOutcome`：直接委托 `self.fail(error.message())`，复用既有 `fail()` 收尾
  （discard pending → `cancel_pending(DiscardTurn)` → 清 `in_flight` → 迁 `LoopCursor::Error`
  → quiescent），因此折叠后落在 `ErrorCursor` 的文本与 M1-2 逐字节一致。带 rustdoc，说明它是
  内部 `Result` 层折回 `step()` infallible 契约的唯一转换点。
- 把 `step()`（`mod.rs`）改成设计文档 §2.2 的 `match result { Ok(outcome) => outcome,
  Err(error) => self.fail_from(error) }` 形状，移除 M1-2 埋下的临时桥接
  `result.unwrap_or_else(|error| self.fail(error.message()))`；`mod.rs` 内**不再有临时桥接**。
- 兑现 M1-2 在三处 `// M1-3 will replace with fail_from` 注释处许下的承诺：`step()` 桥接已彻底
  移除；`tools.rs` 两处（`finish_tool_phase→block_on_llm`、`abandon_tool_phase→finish_cancel`）
  仍需局部折叠（这两个方法 M1-4 才改成返回 `Result`），其 `.unwrap_or_else(|error|
  self.fail(error.message()))` 改为 `.unwrap_or_else(|error| self.fail_from(error))`，注释更新为
  `// M1-4 will make this method return Result so step() folds via fail_from`。
- **可选增强跳过**：未把 `StepError` 分类信息写入 `ErrorCursor`，保持 `ErrorCursor` 仅 `message`，
  以确保现有断言零改动、失败文案逐字节不变。
- `fail()` / `fail_with_notifications()` 方法体保留不动（仍供 `tools.rs` 带副产品失败与桥接复用）。
  同步更新 `error.rs` 模块 doc，将「M1-3 将落地折叠点」改述为已完成事实。
- 验证（完整序列 1–6 全过）：`cargo fmt --all -- --check` 干净；
  `cargo test -p agent-lib --lib agent::machine::default` 39 passed / 0 failed（**断言未改**）；
  `cargo clippy --all-targets -- -D warnings` 无告警；`cargo test --all --all-targets` 全绿；
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` 通过；`git diff --check` 干净。
  `git diff --stat` 源码改动仅限 `src/agent/machine/default/`（未触及 trait/drive/cursor/state）。

### [DONE] M1-4 把 `tools.rs` 的纯失败路径改为 `?`,保留带副产品失败

**前置依赖**:M1-3。

**上下文**:

`src/agent/machine/default/tools.rs`(672 行,32 处 `self.fail*`)的方法:`begin_tool_phase`@129、
`advance_tool_phase`@193、`emit_tool_batch`@220、`emit_approval`@286、`resume_tool`@335、
`resume_approval`@400、`finish_tool_phase`@501、`abandon_tool_phase`@626。

设计文档 §2.2 的取舍(本计划采纳):**纯失败路径改成 `?`;带副产品的失败(已在同一 step 内产出
notification,如 `emit_tool_batch` / `emit_approval` 里的 requirement-id-unavailable、
`finish_tool_phase` 的 step-limit)仍走显式 `self.fail_with_notifications(notifications, ...)`**,
不强求统一。

**做什么**:

- 把 `tools.rs` 中返回 `StepOutcome` 的方法签名统一改为 `Result<StepOutcome, StepError>`,使其能被
  `mod.rs` 里 `fold_llm_response` / `resume` / `resume_tool` / `resume_approval` 等 `?` 传播。
- **纯失败**(尚未产出 notification 就失败)改 `?` 或返回 `Err(StepError::Protocol(...))`:如
  `pending_tool_calls` 的 `Err(String)`、`tool phase opened without an in-flight turn`、
  `resume targets requirement {}, which is not an in-flight tool call` 等。
- **带副产品失败**保留 `self.fail_with_notifications(notifications, msg)`,但由于该方法返回
  `StepOutcome` 而外层现在返回 `Result`,用 `return Ok(self.fail_with_notifications(...))` 包裹,使这类
  路径**在方法内就地折叠**、不向上抛 `StepError`。这样带副产品失败不经过 `step()` 最外层的 `fail_from`
  (它无法携带 notification),语义与现状一致。
- `pending_tool_calls`(`tools.rs:557`,现返回 `Result<Vec<ToolCall>, String>`)可保留其 `String` 错误
  或改为 `StepError::Protocol`,调用点 `begin_tool_phase` 相应用 `?` 或 `map_err`。
- 复核 `mod.rs` 里调用 `tools.rs` 方法的点(`fold_llm_response` 调 `begin_tool_phase`/`commit_text_turn`;
  `resume` 调 `resume_tool`/`resume_approval`),确保 `Result` 链贯通。

**完成记录**:

- `tools.rs` 8 个返回 `StepOutcome` 的方法统一改签名为 `Result<StepOutcome, StepError>`:
  `begin_tool_phase`、`advance_tool_phase`、`emit_tool_batch`、`emit_approval`、`resume_tool`、
  `resume_approval`、`finish_tool_phase`、`abandon_tool_phase`;新增 `use super::error::StepError;`。
  `pending_tool_calls` 保留 `Result<Vec<ToolCall>, String>`(调用点 `.map_err(StepError::Protocol)?`)。
- **纯失败路径**(尚未产出 notification 就失败)已 `?` 化,不再是 `if let Err(..) return self.fail(..)`:
  - `register_tool_calls` / `append_tool_response`(×2) / `cancel_pending` 均返回 `ConversationError`,
    直接裸 `?`,经 `From<ConversationError> → StepError::Conversation` 渲染
    `"conversation operation failed: {e}"`,文案逐字节不变。
  - `pending_tool_calls`(`Err(String)`)、`in_flight` 缺失、`tool phase advanced without an active phase`、
    `tool result resumed ...`、`resume targets requirement ... not an in-flight tool call`、
    `NeedTool/NeedInteraction ... cannot accept`、`` tool `{}` failed ``(StopRun)、approval 各校验失败等,
    改为 `return Err(StepError::Protocol(..))`,文案与旧 `self.fail(..)` 一致。
  - `tool_ids.tool_call_id` / `tool_result_message_id` 因 `StepError::ToolRuntime` 渲染前缀是
    `"tool runtime operation failed"`(≠ 现有 `"tool id unavailable"`,且 `tests/mod.rs:386` 断言该文案),
    改用 `.map_err(|e| StepError::Protocol(format!("tool id unavailable: {e}")))?` 保留原文案。
  - `accepts_response` / `ApprovalResponse::try_from` 同理用
    `.map_err(|e| StepError::Protocol(format!("interaction result rejected: {e}")))?`。
- **带副产品失败**(已在同一 step 内产出 notification)仍走显式
  `return Ok(self.fail_with_notifications(notifications, ..))` 在方法内**就地折叠**,不向上抛
  `StepError`(避免经 `step()` 的 `fail_from` 丢 notification):共 10 处——`emit_tool_batch`
  (requirement-id / cursor build / cursor transition)、`emit_approval`(requirement-id / cursor
  transition)、`resume_approval` 拒绝分支(cursor build / cursor transition)、`finish_tool_phase`
  (step-limit / next_step_id / next_assistant_message_id)。notification 逐字节保留。
- `finish_tool_phase`→`block_on_llm`、`abandon_tool_phase`→`finish_cancel` 两处 M1-3 遗留的
  `.unwrap_or_else(|error| self.fail_from(error))` 局部折叠**已消除**,改为直接传播
  (`self.block_on_llm(..)` / `self.finish_cancel(..)`),连同 `// M1-4 will make this method return
  Result ...` 注释一并删除。`block_on_llm` 失败经 `step()` 的 `fail_from` 折叠——与旧局部 `fail_from`
  同样丢弃 notification,语义等价。
- `mod.rs` 4 处调用点去掉临时 `Ok(..)` 包裹,让 `Result` 链贯通:`fold_llm_response`→
  `begin_tool_phase`、`resume`→`resume_tool`/`resume_approval`、`abandon`→`abandon_tool_phase`。
- 至此 `tools.rs` 内**不再有** `self.fail(..)`(仅剩 10 处带副产品的 `fail_with_notifications`);
  `fail_from` 只存活于 `mod.rs` `step()` 最外层——刀 (C) 的单一折叠点契约完整。同步在 `error.rs`
  模块 doc 追加一句 M1-4 已把 `Result` 层贯通到 `tools`。
- 验证(完整序列 1–6 全过):`cargo fmt --all -- --check` 干净;
  `cargo test -p agent-lib --lib agent::machine::default` 39 passed / 0 failed(**断言未改**);
  `cargo clippy --all-targets -- -D warnings` 无告警;`cargo test --all --all-targets` 全绿;
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` 通过;`git diff --check` 干净。
  源码改动仅限 `src/agent/machine/default/`(`tools.rs` / `mod.rs` / `error.rs` doc)。

### [DONE] M1-5 Milestone 1 review:刀 (C) 正确性与完整性

**前置依赖**:M1-1 ~ M1-4。

**上下文**:

刀 (C) 的验收核心是「对外行为逐字节不变、噪音显著下降」。

**做什么**:

- 通读 `src/agent/machine/default/mod.rs` 与 `tools.rs`,确认:
  - `step()` 是唯一的 `StepError → Error` cursor 折叠点(`fail_from`);无残留临时桥接。
  - 纯失败路径已 `?` 化;带副产品失败仍就地 `fail_with_notifications` 且 notification 完整。
  - 所有错误文案与改造前一致(用 `git diff` 对照失败文案字符串)。
- 统计并在完成记录中给出:`mod.rs` / `tools.rs` 改造前后的 `if let Err` 与 `self.fail*` 计数对比,量化
  噪音下降。
- 跑一遍完整验证序列 1–6,并**额外**运行全量机器测试:
  `cargo test -p agent-lib agent::machine::default`、`cargo test --all --all-targets`。
- 确认 `git diff --stat` 只触及 `src/agent/machine/default/`(未误伤 trait / drive / cursor / state)。
- 若发现任何行为漂移(失败文案变化、notification 丢失、cursor 落点变化),记录并修复后再关闭里程碑。

**验证条件**:

- 完整验证序列 1–6 全过;全量测试 `cargo test --all --all-targets` 全绿。
- 完成记录包含噪音下降的量化数据与「对外行为不变」的确认。
- `git diff --stat` 仅限 `src/agent/machine/default/`。

**完成记录**:

刀 (C) 里程碑验收通过,对外行为逐字节不变、噪音显著下降。纯 review 任务,未改运行时代码
(未发现任何行为漂移)。

**审阅结论(逐条对照验证条件)**:

- **`step()` 是唯一 `StepError → Error` 折叠点**:`step()`(`mod.rs:835-846`)以
  `match result { Ok(outcome) => outcome, Err(error) => self.fail_from(error) }` 收敛,
  `fail_from`(`mod.rs:829-831`)= `self.fail(error.message())`。裸 `self.fail(` 仅出现在
  `fail_from` 内部一处,无残留临时桥接。
- **纯失败已 `?` 化;带副产品失败就地保留**:`mod.rs` 已无 `if let Err`;`tools.rs` 残留的
  3 处 `if let Err`(`281/317/503`)与 10 处 `self.fail_with_notifications`(`241/275/282/304/318/
  497/504/540/551/560`)全部携带此前已发的 `notifications`/`vec![finished]` 副产品,符合 M1-4
  设计(带副产品失败不可裸 `?`),notification 完整。
- **错误文案逐字节一致**:提取 baseline(`7ee6254`,M1-1 之前)与 HEAD 的全部失败字符串字面量,
  两侧集合完全相同(24 条,含 `"tool id unavailable: {error}"`、`"conversation operation
  failed: {error}"`、`"agent loop step limit {max_steps} ..."`、`"interaction result rejected:
  {error}"`、`"NeedTool requirement cannot accept a \`{}\` result"` 等),`StepError::message()`
  渲染与旧 `self.fail(format!(..))` 一致。
- **改动范围仅限 `src/agent/machine/default/`**:M1 全部代码改动仅
  `error.rs`(+104)/`mod.rs`/`tools.rs`,`git diff --name-only c50ec79^..HEAD -- src` 无任何
  `default/` 之外的源文件;`LoopCursor`/`state/cursor.rs`/`drive.rs`/trait 均未误伤。

**噪音下降量化(baseline `7ee6254` → HEAD)**:

| 文件 | `self.fail*` | `if let Err` |
|------|-------------|--------------|
| `mod.rs`   | 33 → 4  | 10 → 0 |
| `tools.rs` | 32 → 10 | 8 → 3  |
| **合计**   | **65 → 14(-51,-78%)** | **18 → 3(-15,-83%)** |

- `mod.rs` 残留 4 处 `self.fail*` 均为定义体/doc/折叠点(`797` `fail_with_notifications` 定义、
  `828` doc 注释、`830` `fail_from`、`844` `step` 折叠),非失败噪音。
- 残留的 `self.fail_with_notifications` + `if let Err` 全部是「带副产品失败」的就地折叠,是刀 (C)
  刻意保留的语义,非可消除噪音。

**验证序列(1–6 全过 + 额外全量机器测试)**:

1. `cargo fmt --all -- --check` — 通过。
2. `cargo test -p agent-lib agent::machine::default`(聚焦)— 39 passed; 0 failed(断言未改)。
3. `cargo clippy --all-targets -- -D warnings` — 无警告。
4. `cargo test --all --all-targets` — 全绿。
5. `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` — 无警告。
6. `git diff --check` — 干净。

Milestone 1(刀 C)关闭:内部 `Result` 层落地,`step` 最外层单点折叠,sans-io 契约与序列化
形状零变化。

---

## Milestone 2 — 刀 (B):cursor 相位与 scratch 合一(设计文档 §3,落点 2)

> 目的:消灭游离在 cursor 之外、靠隐式约定与 cursor 相位对齐的两个非序列化 `Option`
> (`in_flight`@`mod.rs:156` / `pending_reconfig`@`mod.rs:161`),让「卡住状态」只有一个真相。
> **严格走落点 2**:`LoopCursor` 保持纯地址、全序列化不变;scratch 收敛成一个非序列化 enum,序列化
> 零风险。

### [DONE] M2-1 引入非序列化 `TurnScratch` enum 收拢 mid-turn scratch

**前置依赖**:Milestone 1 完成(在 `?` 化之后的干净基线上做)。

**上下文**:

现状(设计文档 §3.1):两个非序列化字段靠隐式约定与 cursor 相位对齐——「cursor 是 `StreamingStep`
⇒ `in_flight` 必 `Some`」「cursor 是 `AwaitingReconfig` ⇒ `pending_reconfig` 必 `Some`」。字段:

- `in_flight: Option<InFlight>`(`mod.rs:156`);`InFlight`(`tools.rs:68`)携带
  `assistant_message_id` / `steps_started` / `tools: Option<ToolPhase>`。
- `pending_reconfig: Option<PendingReconfig>`(`mod.rs:161`);`PendingReconfig`(`mod.rs:86`)是
  `BeginTurn { user, application }` / `Commit { step_id, application, records }`。

设计文档 §3.3 落点 2 的形状:保留 `LoopCursor` 不变,另立一个非序列化 `TurnScratch` enum,用类型保证
「scratch 相位与 cursor 相位同构」,并可给一个 `debug_assert!(scratch.matches(cursor))` 兜底。

**做什么**:

- 在 `src/agent/machine/default/mod.rs` 定义一个非序列化(不 derive serde,与 `InFlight` /
  `PendingReconfig` 现状一致)的 enum,建议:

  ```rust
  /// 单一 mid-turn scratch:相位与 LoopCursor 同构,消灭游离的两个 Option。
  #[derive(Debug)]
  enum TurnScratch {
      /// 无进行中的 turn(cursor 在 Idle / CancelRecovery / Done / Error)。
      None,
      /// 一个 turn 正在进行(cursor 在 StreamingStep / AwaitingTool / AwaitingApproval)。
      InTurn(InFlight),
      /// 一个 turn-boundary reconfig 被 park(cursor 在 AwaitingReconfig)。
      Reconfig(PendingReconfig),
  }
  ```

- 用**单个** `scratch: TurnScratch` 字段替换 `in_flight` + `pending_reconfig` 两个字段(改
  `DefaultAgentMachine` 结构体@`mod.rs:129`、`new()`@`mod.rs:174` 的初始化为
  `scratch: TurnScratch::None`)。
- 提供访问器,替换现有对 `self.in_flight` / `self.pending_reconfig` 的直接读写:
  - `fn in_flight(&self) -> Option<&InFlight>` / `fn in_flight_mut(&mut self) -> Option<&mut InFlight>`
    (仅 `InTurn` 时 `Some`)——`tools.rs` 的 `tool_phase`/`tool_phase_mut`(`tools.rs:597`/`604`)、
    `finish_tool_phase` 对 `in_flight` 的读写都改走这里。
  - `fn take_pending_reconfig(&mut self) -> Option<PendingReconfig>`(仅 `Reconfig` 时取出并置 `None`)
    ——替换 `resume_reconfig` 的 `self.pending_reconfig.take()`(`mod.rs:696`)。
  - 设置入口:`open_user_turn` 的 `self.in_flight = Some(InFlight::new(..))`(`mod.rs:362`)改成
    `self.scratch = TurnScratch::InTurn(InFlight::new(..))`;`emit_reconfig_effect` 的
    `self.pending_reconfig = Some(pending)`(`mod.rs:647`)改成 `TurnScratch::Reconfig(pending)`;
    各清空点(`finalize_text_commit`@620-621、`finish_cancel`@808-809、`fail_with_notifications`@843、
    `abandon_reconfig`@796、`finish_tool_phase` 的 `in_flight.tools = None`)改成对 `TurnScratch` 的
    对应转移。
- 加 `fn matches_cursor(&self, cursor: &LoopCursor) -> bool` 供 `debug_assert!` 使用(可选但推荐)。

**验证条件**:

- `DefaultAgentMachine` 只剩一个 `scratch: TurnScratch` 字段,`in_flight` / `pending_reconfig` 两个
  字段已删除;`tools.rs` 与 `mod.rs` 全部改走新访问器。
- `LoopCursor` / `state/cursor.rs` **完全未改动**(serde 形状不变);`git diff` 确认。
- `cargo test -p agent-lib agent::machine::default` 全绿,断言未改。
- 完整验证序列 1–6 全过。

**完成记录**:

严格走落点 2:`LoopCursor` / `state/cursor.rs` **完全未改**(serde 形状零变化,`git diff
--name-only` 确认 cursor.rs 未触及),仅把两个游离的非序列化 `Option` 收敛成单个非序列化
`scratch: TurnScratch`,对外运行时语义逐字节不变。

**做了什么**:

- 在 `mod.rs`(`impl PendingReconfig` 之后、struct 之前)新增非序列化(不 derive serde,与
  `InFlight` / `PendingReconfig` 现状一致)`enum TurnScratch { None, InTurn(InFlight),
  Reconfig(PendingReconfig) }`,rustdoc 说明其相位与 `LoopCursor` 同构、有意不入
  `AgentState` 序列化(跨进程恢复从持久 Conversation pending + reconfig 队列重建)。
- `DefaultAgentMachine` 结构体的 `in_flight: Option<InFlight>` + `pending_reconfig:
  Option<PendingReconfig>` 两个字段 → 单个 `scratch: TurnScratch`;`new()` 初始化改为
  `scratch: TurnScratch::None`。
- 新增私有访问器(`validate_reconfig_registry` 之后):
  - `fn in_flight(&self) -> Option<&InFlight>` / `fn in_flight_mut(&mut self) ->
    Option<&mut InFlight>`(仅 `InTurn` 相位 `Some`)——`tools.rs` 的 `tool_phase` /
    `tool_phase_mut` / `finish_tool_phase` / `begin_tool_phase` 与 `mod.rs` 的
    `fold_llm_response` 全部改走这里。
  - `fn take_pending_reconfig(&mut self) -> Option<PendingReconfig>`(仅 `Reconfig` 相位取出
    并置 `None`,否则原样还原并返回 `None`)——替换 `resume_reconfig` 的
    `self.pending_reconfig.take()`。
- 各设置/清空点改为对 `TurnScratch` 的相位转移:`open_user_turn` → `InTurn(InFlight::new)`;
  `emit_reconfig_effect` → `Reconfig(pending)`;`finalize_text_commit` / `finish_cancel` /
  `abandon_reconfig` / `fail_with_notifications` → `TurnScratch::None`;`finish_tool_phase` 的
  `in_flight.tools = None` 经 `in_flight_mut()` 就地转移。
- 加 `impl TurnScratch { fn matches_cursor(&self, &LoopCursor) -> bool }` 作为「cursor 与
  scratch 相位对齐」不变量的可测试断言辅助,带 rustdoc;因本任务尚未接入 `debug_assert!`
  (M2-2 才在 resume 路径接线),暂标 `#[allow(dead_code)]`(注释注明 M2-2 移除)。

**语义等价性核对(关键)**:

- 旧模型下 during-turn `PendingReconfig::Commit` 在 `AwaitingReconfig` 时 `in_flight` 与
  `pending_reconfig` **同时** `Some`;单枚举把它折成 `Reconfig` 会丢弃 `InFlight`。已核对:进入
  `Reconfig` 相位后 `in_flight` **永不再被读取**(`resume_reconfig` 的 `Commit` 分支走
  `finalize_text_commit` 直接置 `None`;`BeginTurn` 分支走 `open_user_turn` 重设 `InTurn`;
  `abandon_reconfig` 走 `finish_cancel` 置 `None`),故合一语义安全。
- `fail_with_notifications` 旧代码只清 `in_flight`、保留 `pending_reconfig`;新代码
  `scratch = None` 全清。差异仅出现在「`emit_reconfig_effect` 的 `transition_cursor` 失败」这一
  极端路径,此后 cursor 落在 `Error`(quiescent),scratch 为非序列化且此后永不再读,**非对外
  可观测**,符合「语义零变化」。
- `DefaultAgentMachine` 现只剩单个 `scratch: TurnScratch` 字段;`in_flight` /
  `pending_reconfig` 两字段已删除;全部读写改走新访问器/相位转移。无测试直接引用这两个私有
  字段(已 `grep` 确认)。

**验证序列(1–6 全过)**:

1. `cargo fmt --all -- --check` — 通过。
2. `cargo test -p agent-lib --lib agent::machine::default`(聚焦)— 39 passed / 0 failed
   (断言未改,与 M1-5 基线一致)。
3. `cargo clippy --all-targets -- -D warnings` — 无警告。
4. `cargo test --all --all-targets` — 全绿(36 个测试二进制,0 failed)。
5. `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` — 无警告。
6. `git diff --check` — 干净。
- 附加:`git diff --stat -- src/**` 仅触及 `src/agent/machine/default/mod.rs`(+/-)与
  `tools.rs`(+/-);`state/cursor.rs`、trait、drive 均未误伤,serde 形状零变化。

### [DONE] M2-2 用相位 match 消灭 resume/pivot/abandon 的 scratch 对齐防御

**前置依赖**:M2-1。

**上下文**:

设计文档 §3.1/§3.2 指出:每条 resume 路径当前先 re-match cursor 决定分支,再去另一处摸 scratch,还要处理
「万一它俩对不上」的不可能分支。典型:

- `inject_pivot`(`mod.rs:384`):`let LoopCursor::StreamingStep(cursor) = ... else { fail }`,再从
  cursor 取 `requirement_id`。
- `resume_reconfig`(`mod.rs:669`):cursor 已是 `AwaitingReconfig`,但
  `let Some(pending) = self.pending_reconfig.take() else { fail }` 还要单独校验。
- `resume`(`mod.rs:468`)按 cursor 分派到 `resume_llm`/`resume_tool`/`resume_approval`/`resume_reconfig`。
- `fold_llm_response`(`mod.rs:525`)`let Some(assistant_message_id) = self.in_flight... else { fail }`。

有了 M2-1 的 `TurnScratch`,「match 到相位即拿到 scratch」成立,不可能不一致。

**做什么**:

- 把「先判 cursor 相位、再单独摸 scratch、再处理不一致」的地方,收敛成对 `TurnScratch` 的单次 match:
  - `resume_reconfig`:改为从 `take_pending_reconfig()` 直接拿到 `PendingReconfig`;当返回 `None` 时
    返回 `Err(StepError::Protocol("reconfig resume with no deferred reconfiguration in flight"))`
    (文案保持)。
  - `fold_llm_response`:改为从 `in_flight()` 拿 `assistant_message_id`,`None` 时
    `Err(StepError::Protocol("missing in-flight assistant message id for the LLM response"))`。
  - `inject_pivot`:cursor 相位判断保留(pivot 合法性依赖 `StreamingStep` + 其 `requirement_id`),但
    「相位 + scratch 一致性」不再需要额外防御——`in_flight()` 在 `StreamingStep` 下必 `Some`,可用
    `debug_assert!` 记录该不变量。
- 保持所有对外错误文案不变(现有测试若断言了这些文案则逐字节保留)。
- 在改造后的关键 resume 路径插入 `debug_assert!(self.scratch.matches_cursor(self.state.loop_cursor()))`
  (若 M2-1 实现了 `matches_cursor`),把「cursor + scratch 一致」变成可测试不变量。

**验证条件**:

- `resume_reconfig` / `fold_llm_response` / `inject_pivot` 不再有「摸完 scratch 再校验不一致」的双重
  防御;scratch 一律经 `TurnScratch` 相位取得。
- `cargo test -p agent-lib agent::machine::default` 全绿(尤其 reconfig / pivot / tool 路径),断言未改。
- 完整验证序列 1–6 全过。

**完成记录**:

M2-1 已把全部 scratch 读写路由到 `TurnScratch` 访问器,因此本任务聚焦「消灭对齐防御的
最后一步」:把「cursor 相位 ⇒ scratch 相位」这一由类型保证的不变量接入运行时可测试的
`debug_assert!`,并让 `matches_cursor` 正式上线(去掉 M2-1 暂挂的 `#[allow(dead_code)]`)。
对外错误文案、`LoopCursor` / `state/cursor.rs`(serde 形状)零变化。

**做了什么**(仅 `src/agent/machine/default/mod.rs`):

- 去掉 `TurnScratch::matches_cursor` 的 `#[allow(dead_code)]`,rustdoc 更新为「wired into
  `debug_assert!`s on the `resume` / `abandon` dispatch and the pivot injection path」。
- `resume()`(dispatch 入口)顶部插
  `debug_assert!(self.scratch.matches_cursor(self.state.loop_cursor()), ...)`——一处覆盖
  `resume_llm`/`fold_llm_response`、`resume_tool`、`resume_approval`、`resume_reconfig`
  四条 resume 分派。这正是设计文档 §3.2 所说「match 到 cursor 相位即拿到 scratch,二者不可能
  drift」的可测试化:旧模型里那条「先 re-match cursor、再另处摸 scratch、再处理对不上」的双重
  防御被这一条不变量收敛。
- `abandon()` 顶部插同样的 `matches_cursor` debug_assert——覆盖
  `abandon_llm_step`/`abandon_tool_phase`/`abandon_reconfig`,补齐标题里的 abandon 相位。
- `inject_pivot()` 在 `StreamingStep` + `requirement_id` 相位判定通过后插
  `debug_assert!(self.in_flight().is_some(), ...)`——记录「`StreamingStep` ⇒ `TurnScratch::InTurn`
  ⇒ `in_flight()` 必 `Some`」不变量,pivot 路径无需任何额外「turn 是否真的在飞」防御。

**语义/文案核对**:

- `resume_reconfig` 仍走 `take_pending_reconfig()` 且保留 "reconfig resume with no deferred
  reconfiguration in flight";`fold_llm_response` 仍走 `in_flight()` 且保留 "missing in-flight
  assistant message id for the LLM response"——两处 `None` 分支作为不可能路径的兜底守卫按任务
  要求逐字节保留,现由 debug_assert 佐证其不可达。
- 三处均为 `debug_assert!`,release 构建无副作用,运行时语义零变化;`git diff` 确认仅
  `mod.rs` 改动,`state/cursor.rs` / trait / drive 未触及,serde 形状不变。

**不变量正确性核对**:遍历每个 cursor 相位——`StreamingStep`/`AwaitingTool`/`AwaitingApproval`
⇒ `InTurn`;`AwaitingReconfig` ⇒ `Reconfig`(during-turn `Commit` 已在 `emit_reconfig_effect`
把 `InTurn` 覆盖为 `Reconfig` 后才 transition,入口一致);`Idle`/`Done`/`Error`/`CancelRecovery`
⇒ `None`——`matches_cursor` 在 resume/abandon 入口对全部相位为真。

**验证序列(1–6 全过)**:

1. `cargo fmt --all -- --check` — 通过。
2. `cargo test -p agent-lib --lib agent::machine::default`(聚焦,含 reconfig/pivot/tool)—
   39 passed / 0 failed(debug_assert 在 debug 测试构建下运行且未触发,与 M2-1 基线一致)。
3. `cargo clippy --all-targets -- -D warnings` — 无警告。
4. `cargo test --all --all-targets` — 36 个测试二进制全绿,0 failed。
5. `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` — 无警告。
6. `git diff --check` 干净;`git diff --stat` 确认 `src/` 仅 `mod.rs`(+22/-2),`cursor.rs`
   未触及。

### [TODO] M2-3 显式化 `rebuild_scratch_from_state()` 并加 restore 往返测试

**前置依赖**:M2-2。

**上下文**:

设计文档 §3.4:跨进程恢复时,一个卡在 turn 中途的机器应从**持久的 Conversation pending + 队列**重建
mid-turn scratch,而不是反序列化 scratch(scratch 有意非序列化)。这段逻辑现在隐式散布在
`begin_user_turn` 的补丁式判断里(如「若 pending 存在则 discard」`mod.rs:318-326`)。本任务把它显式化,
让「cursor + scratch 一致」成为一个可测试的不变量。

注意:`DefaultAgentMachine` 当前的构造入口只有 `new()`(`mod.rs:174`)+ builder(`with_*`),**没有**从
「已处于 mid-turn 的持久 `AgentState`」重建 scratch 的显式入口。落点 2 不改序列化,因此本任务的
`rebuild_scratch_from_state` 主要服务两类场景:①从一个 `AgentState`(其 `LoopCursor` 停在
`AwaitingReconfig` / `StreamingStep` 等)构造机器时,把 scratch 对齐到 cursor 相位;②作为
`begin_user_turn` 那段隐式补丁的显式替身。

**做什么**:

- 新增 `fn rebuild_scratch_from_state(&mut self)`(或 `fn with_rebuilt_scratch(mut self) -> Self`),
  依据 `self.state.loop_cursor()` 的相位,从持久态重建 `TurnScratch`:
  - `StreamingStep` / `AwaitingTool` / `AwaitingApproval`:从 `Conversation.pending()` 读出 in-flight
    的 assistant message id / 已闭合的 tool batch,重建 `InFlight`(注意 `ToolPhase` 的
    running/awaiting 明细在落点 2 下**不可**完整重建——mid-batch 精确恢复是 driver/persistence 关注点,
    设计文档 §3.5 与 `tools.rs:39-42` 已标注 deferred;本任务只重建 `InFlight` 的 message-id/steps
    层面,`tools: None`,并在 rustdoc 说明该限制)。
  - `AwaitingReconfig`:从 `queued_reconfigs` 重新 `plan_reconfig`,重建 `PendingReconfig`(注意
    `PendingReconfig::Commit` 的 `records` 可从 application 再渲染,见 `reconfig_boundary_records`,
    不必持久化)。
  - `Idle` / `CancelRecovery` / `Done` / `Error`:`TurnScratch::None`。
- 若把 `begin_user_turn` 的隐式补丁替换为对该函数/不变量的调用可读性更好且不改语义,则替换;否则至少让
  该函数与补丁逻辑显式共享同一套「一致性」定义。
- 新增测试(放 `src/agent/machine/default/tests/`,建议 `mod.rs` 或新增 `restore.rs`):
  - 构造一个 park 在 `AwaitingReconfig` 的 `AgentState`,`rebuild_scratch_from_state` 后断言 scratch 为
    `Reconfig(_)` 且与 cursor 一致;喂入 `RequirementResult::Reconfig(Ok(()))` 能正常推进(往返)。
  - `matches_cursor` 不变量:遍历几个代表性 cursor 相位,断言重建后 `matches_cursor` 为真。

**验证条件**:

- 存在显式的 `rebuild_scratch_from_state`(或等价命名)且带 rustdoc,说明落点 2 下 `ToolPhase` 明细不
  重建的限制。
- 新增 restore 往返测试通过;`cargo test -p agent-lib agent::machine::default` 全绿。
- 完整验证序列 1–6 全过。

**完成记录**:

（待补充）

### [TODO] M2-4 Milestone 2 review:刀 (B) 正确性与序列化不变性

**前置依赖**:M2-1 ~ M2-3。

**上下文**:

刀 (B) 的验收核心:唯一真相(scratch 只有一个 `TurnScratch`)、序列化零风险(`LoopCursor` 未变)、
隐式约定被类型消灭。

**做什么**:

- 通读 `mod.rs` / `tools.rs`,确认已无 `self.in_flight` / `self.pending_reconfig` 的裸字段访问,全部
  经 `TurnScratch` 访问器;确认无「相位 + scratch」双重防御残留。
- 用 `git diff src/agent/state/cursor.rs` 确认 **cursor 文件零改动**;跑 `cargo test -p agent-lib
  agent::state`(尤其 `streaming_step_cursor_round_trips_requirement_binding`、
  `awaiting_tool_cursor_round_trips_requirement_ids`、
  `agent_state_serde_round_trips_through_conversation_snapshot`)全绿,证明序列化边界未动。
- 跑完整验证序列 1–6 + `cargo test --all --all-targets`。
- 确认 `git diff --stat` 仅触及 `src/agent/machine/default/`。
- 在完成记录中记录:字段数变化(2 个 `Option` → 1 个 enum)、消灭的防御分支数、`ToolPhase` 明细不重建
  这一已知限制。

**验证条件**:

- 完整验证序列 1–6 全过;全量测试全绿。
- `state/cursor.rs` 零改动(`git diff` 证明);序列化往返测试全绿。
- 完成记录含量化数据与序列化不变性确认。

**完成记录**:

（待补充）

---

## Milestone 3 — 刀 (A):用宏收敛 effect coproduct 的扇出(设计文档 §4)

> 目的:用一份单一 effect 清单驱动的声明宏,生成设计文档 §4.1 表格第 1–7 处样板
> (`RequirementKind`/`RequirementResult`/`RequirementKindTag` 三个 enum + `accepts` +
> `HandlerScope` 访问器 + `scope_handles` + `fulfill_with_scope`),把「加一个 effect 改 8 处」收敛为
> 「清单里加一段」。effect 变体集合已稳定为 6 个(`Llm`/`Tool`/`Interaction`/`Subagent`/`Reconfig`/
> `ExternalSession`),满足设计文档 §4.4 的时机前置条件。**必须先并存 + 等价性断言,再删手写版。**

### [TODO] M3-1 设计 `define_effects!` 清单语法与宏骨架(与手写版并存)

**前置依赖**:Milestone 2 完成。

**上下文**:

现状 8 处扇出(设计文档 §4.1,均已核实):

| # | 位置 | 改什么 |
|---|---|---|
| 1 | `requirement.rs:366` `RequirementKind` | 请求变体(derive serde,`snake_case`) |
| 2 | `requirement.rs:471` `RequirementResult` | 结果变体(**不 derive serde**,运行时半) |
| 3 | `requirement.rs:132` `RequirementKindTag` | tag 变体 + `Display`(`requirement.rs:147`) |
| 4 | `requirement.rs:448` `RequirementKind::accepts` | kind↔result 对齐 + `NeedInteraction` 校验特例 |
| 5 | `drive.rs:114` `HandlerScope::<family>()` | 访问器(默认 `None`) |
| 6 | `drive.rs:533` `scope_handles` | tag→访问器 `is_some` |
| 7 | `drive.rs:548` `fulfill_with_scope` | kind→handler 调用(`NeedSubagent` 返回 `None`) |
| 8 | 机器 resume 分派 | **手写,不宏化**(依赖具体机器 cursor 相位) |

两处必须建模的差异(设计文档 §4.3):
- **`Subagent` 特例**:`fulfill_with_scope` 对它返回 `None`,改由 `resolve_requirement`(`drive.rs:600`)
  串行 + `ScopePop` 处理。宏需支持给某 effect 打 `needs_outer` 标记,让 `fulfill_with_scope` 对它生成
  `None` 分支而非 handler 调用。
- **`Interaction` 的 accepts 校验特例**:`accepts`(`requirement.rs:454-460`)对 `NeedInteraction` 额外
  调 `request.accepts_response(response)`。宏需允许某 effect 声明一个自定义 accepts 后置校验。
- **半区 derive 差异**:`RequirementKind`(+`Requirement`)derive `serde`,`RequirementResult` 不 derive。
- **payload 形状差异**:各变体字段不同(`NeedLlm { request, mode }`、`NeedTool { call_id, call }`、
  `ExternalSession` 的 result 是 `Box<ExternalSessionResult>`),清单需能表达任意字段与结果类型。

**做什么**:

- 参照 `src/agent/id.rs:22` 的 `macro_rules! define_id!` 风格,设计 `define_effects!` 的清单语法(先写在
  设计注释/doc 里,再落宏骨架)。建议每个 effect 段声明:tag 名、`snake_case` 序列化名、请求变体名 +
  字段、结果类型、handler trait 名与访问器名、可选 `needs_outer`、可选自定义 accepts 校验钩子。
- 先实现宏生成**第 1–3 处**(三个 enum + `RequirementKindTag::Display` + `tag()`)。宏产物先以**新名字**
  (如 `RequirementKindGen` 等)与手写版**并存**,不替换,便于 M3-2 做等价性断言。
- 若 `macro_rules!` 表达力不足以处理「半区 derive 差异 + 自定义 accepts + `Box` 结果 + 可选标记」,按
  设计文档 §4.4 退化为独立 proc-macro crate(在 workspace 新增 `crates/agent-effect-macros`),并在完成
  记录说明选型理由。

**验证条件**:

- 宏(或 proc-macro crate)能生成与手写 `RequirementKind`/`RequirementResult`/`RequirementKindTag`
  **结构等价**的类型(字段、变体、`snake_case` 名一致);`cargo build` 通过。
- 现有手写版仍在、现有测试全绿(本任务不删旧码):`cargo test -p agent-lib agent::requirement`。
- `cargo fmt` / `clippy` / `cargo doc`(宏生成项 rustdoc 可编译)通过;`git diff --check` 干净。

**完成记录**:

（待补充）

### [TODO] M3-2 宏覆盖 `accepts` 与 `drive.rs` 扇出(第 4–7 处),并加等价性断言

**前置依赖**:M3-1。

**上下文**:

设计文档 §4.4:**先验证等价性再删旧码**。做法是先让宏与手写版并存,用 `#[test]` 断言两者的 serde 输出
与 `accepts` 行为一致,再(M3-3)删手写版。

**做什么**:

- 扩展宏生成**第 4 处**(`accepts`:遍历 tag 对齐 + 对声明了自定义校验的 effect 生成后置校验分支)与
  **第 5–7 处**(`HandlerScope` 访问器默认 `None`、`scope_handles`、`fulfill_with_scope`,其中带
  `needs_outer` 标记的 effect 在 `fulfill_with_scope` 生成 `None` 分支)。产物仍与手写版并存。
- 新增等价性测试(放 `requirement.rs` 的 `#[cfg(test)]` 或独立测试模块):
  - 对 `ALL_TAGS`(`requirement.rs:757`)的每个 tag,断言宏版与手写版的 `RequirementKind` serde JSON
    逐字节相等、`tag()` 相等。
  - 对 `ALL_TAGS × ALL_TAGS` 的 `accepts` 矩阵,断言宏版与手写版结果完全一致(复用
    `accepts_matrix_pairs_each_kind_with_its_result_only`@825 的思路,含 `NeedInteraction` 的
    `accepts_delegates_permission_action_id_check`@851 场景)。
  - 对 `scope_handles` / `fulfill_with_scope`:构造一个覆盖各 family 的 `HandlerScope` 测试替身,断言
    宏版与手写版对每个 tag 的 handles/fulfill 行为一致(`NeedSubagent` 两版都应在 `fulfill_with_scope`
    返回 `None`)。
- `drive.rs:683` 的 `expect("scope_handles confirmed a handler for this family")` 依赖 `scope_handles`
  与 `fulfill_with_scope` 对同一 tag 的一致性——等价性测试须覆盖这条不变量。

**验证条件**:

- 等价性测试全部通过,证明宏版与手写版在 serde、`accepts`、`scope_handles`、`fulfill_with_scope` 上行为
  完全一致。
- 现有 `requirement.rs` / `drive.rs` 测试仍全绿(旧码未删):`cargo test -p agent-lib agent::requirement
  agent::drive`。
- 完整验证序列 1–6 全过。

**完成记录**:

（待补充）

### [TODO] M3-3 切换到宏产物、删除手写版、更新 external-agent 接入示例

**前置依赖**:M3-2(等价性已证)。

**上下文**:

等价性已由 M3-2 证明,现在把生产代码切到宏产物,删除手写的三个 enum + `accepts` + 三处 drive 扇出。第 8
处(机器内 resume 分派,`mod.rs:468` 的 `resume` + `tools.rs` 的 `resume_tool`/`resume_approval`)**保持
手写不动**。

**做什么**:

- 把 `RequirementKind`/`RequirementResult`/`RequirementKindTag`/`accepts`/`HandlerScope` 访问器/
  `scope_handles`/`fulfill_with_scope` 的**手写定义删除**,让宏产物接管原有名字(把 M3-1 的
  `*Gen` 临时名去掉,或直接让宏输出正式名)。删除 M3-2 的等价性测试中「对比手写版」的部分(手写版已不
  存在),保留对宏产物本身的行为测试(serde 往返、accepts 矩阵)。
- 全库编译:所有引用 `RequirementKind::NeedLlm { .. }` 等的地方(machine、drive、testkit、tests)应无需
  改动即通过(宏产物变体名/字段与手写版一致)。若有差异,说明宏产物未完全等价,回到 M3-1/M3-2 修正。
- 在宏清单处或 `docs/effect-refine.md` 补一段注释/附录,演示「新增一个 effect = 清单里加一段」的完整
  diff(即设计文档 §4.2 承诺的收益),作为将来加 effect 的操作指南。

**验证条件**:

- 手写的三个 enum + `accepts` + 三处 drive 扇出已删除,由宏产物取代;全库 `cargo build` 通过。
- 全量测试 `cargo test --all --all-targets` 全绿,**断言未修改**(证明对外类型形状与行为不变)。
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` 通过(宏生成项 rustdoc 可编译且有文档)。
- 完整验证序列 1–6 全过。

**完成记录**:

（待补充）

### [TODO] M3-4 Milestone 3 review:刀 (A) 正确性、等价性与可维护性

**前置依赖**:M3-1 ~ M3-3。

**上下文**:

刀 (A) 的验收核心:对外类型形状与运行时行为逐字节不变、加 effect 的成本从 8 处降到 1 处、宏可读且
rustdoc 完整。

**做什么**:

- 确认第 1–7 处已全部由宏生成、第 8 处(机器 resume 分派)仍手写且未被误宏化。
- 复核两处特例在宏产物里正确成立:`NeedSubagent` 在 `fulfill_with_scope` 返回 `None`(仍走
  `resolve_requirement` + `ScopePop` 串行路径);`NeedInteraction` 的 accepts 后置校验仍生效
  (`accepts_delegates_permission_action_id_check` 场景通过)。
- 验证「加 effect 成本」:按 M3-3 的操作指南,在一个临时分支/草稿里试加一个虚构 effect,确认只需改清单
  一处即可编译通过(验证后回退,不提交该虚构 effect)。在完成记录中描述该验证。
- 跑完整验证序列 1–6 + `cargo test --all --all-targets`;确认 `git diff --stat` 触及范围合理
  (`requirement.rs`、`drive.rs`、可选 `macros.rs`/新 proc-macro crate、宏操作指南文档)。
- 确认三刀合计:`docs/effect-refine.md` 的 §5 落地矩阵三行全部兑现;对外运行时语义自始至终未变。

**验证条件**:

- 完整验证序列 1–6 全过;全量测试全绿。
- 完成记录含:第 1–7 处宏覆盖确认、两处特例正确性、加 effect 成本验证、`git diff --stat` 范围。
- 三刀全部完成后,`docs/effect-refine.md` §5 矩阵的三行(语义变化=无、序列化风险=无)得到验证。

**完成记录**:

（待补充）
