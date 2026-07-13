# 执行计划 — M2-3 抽出 LLM step:`NeedLlm` 与 text-only turn 折叠

## 选中的任务

`TODO.md` 第一个未完成任务是 **M2-3**(M2-1/M2-2 已 `[DONE]`)。

**结构性缺陷(需先修复)**:M2-3 的标题行 `### [TODO] M2-3 …` 在 `TODO.md`(含 HEAD)中丢失,
任务正文(前置依赖 M2-2、上下文 §2/§3、NeedLlm text-only turn)完好地夹在 M2-2 完成记录与
M2-4 标题之间。按"任务条目本身错误须结构性修复"的例外,先补回标题,再实现该任务。

## 目标(迁移文档 §2/§3/§5)

把"要一次 LLM 调用"从 legacy `DefaultAgentLoop` 的"内部 await client"抽成纯 sans-io
`AgentMachine::step`:`step` 吐 `NeedLlm { request, mode }`,driver `Resume(Llm(Response))`
回灌,text-only turn 完整推进(begin_turn → NeedLlm → 折叠 Response → commit → 静止)。

## 设计

### 1. 复用的请求构造(class-wide 重构)
- 新文件 `src/agent/request.rs`:`pub(crate) fn build_chat_request(state, tools: Vec<Tool>, stream)`
  + `combine_system_prompt`,从 `default.rs` 抽出;签名由 `&dyn ToolRegistry` 改为数据 `Vec<Tool>`,
  使 sans-io 机器可直接用 `state.current_tool_set().tools()` 构造请求(不持 live registry)。
- `default.rs::prepare_assistant_call` 改调 `crate::agent::request::build_chat_request(&state,
  tool_registry.declarations(), stream)`;删除 default.rs 内的两个本地函数。
- `LlmStepMode::request_stream_flag` 提升为 `pub(crate)`。
- `agent/mod.rs` 加 `mod request;`(private,后代模块可见)。

### 2. 具体机器
- `machine.rs` → 目录模块:`git mv src/agent/machine.rs src/agent/machine/mod.rs`
  (trait `AgentMachine`/`StepInput`/`StepOutcome` + 既有 FakeMachine 测试留在 mod.rs)。
- 新文件 `src/agent/machine/default.rs`:`DefaultAgentMachine`。
  - 字段:`state: AgentState`、`mode: LlmStepMode`、`requirement_ids: Arc<dyn RequirementIds>`、
    `in_flight: Option<InFlightStep{ step_id, assistant_message_id }>`(mirror 现有
    `PreparedAssistantCall`;从 External 携到 Resume 用于 `finish_assistant`;cursor 只记 RequirementId)。
  - `new(state, mode, requirement_ids)`、`state()`/`into_state()`/`mode()` 访问器。
  - `impl AgentMachine`:`cursor()`=`state.loop_cursor()`;`step` 分派:
    - `External(UserMessage)`:allocate `RequirementId`(Llm tag)→ `begin_turn` →
      `build_chat_request` → cursor `StreamingStep(step_id, Some(CursorRequirement::root(id)))` →
      记 in_flight → 吐 `NeedLlm`,`quiescent=true`,无通知。
    - `Resume(Llm(Ok(resp)))`:校验 cursor=StreamingStep 且 id 匹配 → `start_assistant_response` +
      `finish_assistant(assistant_message_id)`;`ReadyToCommit` → `commit_pending(TurnMeta::default())`
      → boundary=`head()` → cursor `Done(Completed)` → 吐 `Notification::StepBoundary`,quiescent。
      `RequiresToolCallMappings` → 明确"未实现(M2-4)"错误(cursor→Error,discard pending)。
    - `Resume(Llm(Err(e)))`:cursor→Error(discard pending),quiescent。
    - `External(Pivot/deprecated)`、`Abandon`:M4 范畴,先分类 Error(诚实,不静默跳过)。
  - `fail(msg)`:discard pending(`cancel_pending(DiscardTurn)`)→ cursor→Error(best-effort)→
    清 in_flight → quiescent 空 outcome。
- `machine/mod.rs` 追加 `mod default; pub use default::DefaultAgentMachine;`。
- `agent/mod.rs` 的 `pub use machine::{...}` 追加 `DefaultAgentMachine`。

### 3. 聚焦测试(纯,无 tokio)——在 machine/default.rs
- `External(UserMessage)` → 吐 1 个 `NeedLlm`(request.model/messages 正确、mode 匹配)、cursor
  `StreamingStep`、`pending_requirement_ids` 有该 id、quiescent。
- `Resume(Llm(Ok(text)))` → cursor `Done`、committed history 追加 assistant message、吐
  `StepBoundary`(step_id 正确)、quiescent 无 requirement。
- `Resume(Llm(Err))` → cursor `Error`、pending 被 discard。
- `Resume` id 不匹配 → Error。
- tool-use response → Error(未实现,pending discard)。
- streaming mode:mode 透传为 Streaming、request.stream=true。

## 验证顺序
`cargo fmt --all` → `cargo clippy --all-targets -- -D warnings` → `cargo test --lib agent::machine`
→ `cargo test --all --all-targets`(≤30min) → `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`
→ `git diff --check`。

## 完成后
- TODO.md M2-3 标题加 `[DONE]` + 填完成记录。PLAN.md 不改(无阶段级变更)。
- 提交 `[M2-3] ...`,然后停止。
