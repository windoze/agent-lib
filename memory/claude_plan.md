# Claude 执行计划

## 当前任务:M3-1 定义 `ExternalAgentSpec` / `ExternalAgentState` / cursor 与 runtime handles

**前置依赖**:M2-5(DONE)。

### 目标(TODO M3-1)
在 `src/agent/external/` 定义 external agent machine 的可序列化 spec/state/cursor 与非 serde runtime
handle holder,参照 `AgentSpec`/`AgentState`/`LoopCursor`/`AgentRuntimeHandles` 形态。state 只存可恢复事实,
进程/task/watcher 一律进 handle。`WorkerProfileRef`(§4.1)属 M6,本任务放占位类型 + `Option` 避免调度耦合。

### 设计文档依据
- docs/external-agent.md §4.1(spec)/§4.2(state+cursor)/§4.3(runtime handle)。

### 实施步骤
1. 新增 `src/agent/external/spec.rs`:
   - `WorkerProfileRef`(占位 newtype，transparent serde，M6 展开)。
   - `ExternalAgentSpec { id, runtime, worktree, profile: Option<WorkerProfileRef>, initial_tools,
     session_policy }`,私有字段 + `new` + 访问器(仿 `AgentSpec`)。
2. 新增 `src/agent/external/state.rs`:
   - `ExternalAgentCursor { Idle, AwaitingSession { requirement }, AwaitingInteraction { requirement,
     pending_action }, Done, Error { message } }`,`#[serde(tag="state",content="data",snake_case)]`,`Default=Idle`。
   - `ExternalAgentState { spec, conversation, session: Option<ExternalSessionRef>, cursor, active_tools }`,
     私有字段 + 访问器 + 转换器;自定义 Serialize/Deserialize 走 `ConversationSnapshot`(仿 `AgentState`),
     record 结构 `deny_unknown_fields`。
3. 新增 `src/agent/external/runtime.rs`:
   - `ExternalRuntimeHandles<Runtime, InteractionHandle=(), ToolRegistryHandle=(), SessionTasks=()>`(非 serde),
     `runtime` 必填,`interaction`/`tool_registry` 可选,`session_tasks` 泛型;`new`/`with_handles`/访问器
     (仿 `AgentRuntimeHandles`)。
4. `mod.rs`:声明 `mod spec; mod state; mod runtime;` 并 re-export;更新模块 doc。
5. `src/agent/mod.rs`:re-export 新公开类型。
6. 单测:
   - `state.rs` 内 `ExternalAgentState` serde round-trip(不含 handle,断言无 handle key)。
   - `ExternalAgentCursor` 各变体 round-trip。
   - `runtime.rs`:handles 构造/访问器 smoke(非 serde)。
   过滤名:`cargo test --lib external_agent_state`。

### 验证门(完整序列)
1. `cargo fmt --all`
2. `cargo clippy --all-targets -- -D warnings`
3. 聚焦:`cargo test --lib external_agent_state` + `cargo test --lib external`
4. `cargo test --all --all-targets`(≤30min)
5. `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`

### 进度
- (完成)spec.rs / state.rs / runtime.rs 落地并接线;3 个 `external_agent_state` 测 + 2 个 runtime handles smoke 全过。
- 完整验证门全绿:fmt 无差异;clippy 0 告警(移除未用 `conversation_mut`);`--lib external` 18 passed;
  full suite 30×`test result: ok` 0 failed;doc 0 告警。
- TODO.md 已标 [DONE] 并填完成记录(修复误删的 M3-2 heading)。PLAN.md 无需改(routine 拆分)。待提交。
