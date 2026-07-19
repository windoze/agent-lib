# TODO：mag 缺口收口任务单（委派交互路由 / facade reconfigure / cancel 强化）

本任务单对应 [PLAN.md](PLAN.md) 和 [docs/mag-gaps.md](docs/mag-gaps.md)（唯一设计输入）。
旧任务单已归档（最近一轮）：[docs/archive/2026-07-19-review-fixes/TODO.md](docs/archive/2026-07-19-review-fixes/TODO.md)。

执行规则：

- 严格按编号顺序实现，除非当前任务明确要求先补充前置信息。
- 每个标题中的 `[TODO]` 表示尚未完成。完成后把 `[TODO]` 改成 `[DONE]`，并在任务下方追加
  "完成记录"，写明关键实现决策、验证结果和（如有）breaking change。
- 不要跳过每个 milestone 末尾的 review 任务。
- 缺口编号（A1–A4、C1–C6）定义见 [docs/mag-gaps.md](docs/mag-gaps.md)；修复后在该文档对应
  条目上标注 `✅ 已修复（M*-*）` 或 `📄 已降级（文档承认现状，M*-*）`。
- 修改行为时同步修改拥有该行为的文档，至少检查 `README.md`、`AGENTS.md`、
  `docs/facade-api.md`、`docs/managed-external-agent.md`、`docs/capability-matrix.md`、
  `docs/agent-layer.md`、`docs/agent-effect-model.md`。
- 默认测试必须离线可跑，不依赖真实 provider、真实 CLI login、网络或用户本机配置。
  真实 ACP agent 联调一律 `#[ignore]`，缺环境干净跳过。
- 行号引用自评估时点（2026-07-20，`@0add094`），随后续修复可能漂移，以符号名为准。
- 1.0 前 API 稳定性不作为约束，但优先向后兼容形状（加可选字段/新增方法），breaking
  change 必须在完成记录显式注明。

全量门禁命令（每个 milestone review 必跑）：

```bash
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo clippy --all-targets \
  --features "external-claude-code external-codex external-opencode external-acp" -- -D warnings
cargo test --all --all-targets
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
```

---

## M1：委派交互路由（A1）

### M1-1 [DONE] 交互归因模型：`Interaction` 增加可选 delegate 归因

上下文：

- `Interaction { step_id, kind }`（`src/agent/interaction.rs:46-52`）无任何 delegate 归属
  信息；`InteractionKind`（同文件 179-210）四个变体 serde 齐备（`snake_case`）。
- `PermissionRequest.actor: AgentId`（`src/agent/permission.rs:98`）是权限语义主体，ACP
  adapter 已绑定（`src/agent/external/acp/adapter.rs:122-124`）——与归因互补，不归并。
- mag 侧消费形态：`ServiceEvent::InteractionRequested` 带 `origin { agent, delegate, depth }`
  （mag `docs/CLI.md` §3.3），渲染 `[approval · codex(sub, depth 1)]`。
- `RunContext` 已有 `depth`（`src/agent/context.rs:50-56, 95-149`），委派链上可用。

实现要求：

- 给 `Interaction` 增加可选归因字段（建议 `#[serde(default, skip_serializing_if = "Option::is_none")]
  pub origin: Option<InteractionOrigin>`），`InteractionOrigin` 至少含 `delegate: String` 与
  `depth: u32`；serde 向后兼容（旧数据无该字段可反序列化）。
- root agent 自己发起的交互 `origin = None`；委派链第 n 层子 agent 发起的交互携带其
  delegate 名与深度。
- 归因的**填入点**在 M1-2/M1-3 的路由层（本任务只定类型与构造/标注 API）；类型放
  `agent/interaction.rs`，导出路径与 `Interaction` 一致。
- rustdoc 明确归因语义：它是**渲染归属**（谁替谁问），不是权限主体（那是
  `PermissionRequest.actor`）。

验证条件：

- 单元测试：带/不带 `origin` 的 `Interaction` serde round-trip；无 `origin` 字段的旧 JSON
  可反序列化为 `None`。
- `cargo test -p agent-lib --lib agent::interaction` 通过。

完成记录（2026-07-20）：

- 在 `src/agent/interaction.rs` 新增 `InteractionOrigin { delegate: String, depth: u32 }`，并给
  `Interaction` 增加可选 `origin` 归因字段；root 构造路径保持 `origin = None`，委派路由层可通过
  `Interaction::with_origin` 标注归因，`Interaction::origin` 读取归因。
- `origin` 的 wire 形状保持为可选对象字段，使用 `serde(default, skip_serializing_if = "Option::is_none")`；内部存储为
  `Option<Box<InteractionOrigin>>`，避免新增字段放大 `Interaction` 后触发 feature-gated external 状态枚举的
  `clippy::large_enum_variant`。
- rustdoc 明确该归因仅用于渲染「谁通过委派链发问」，不改变权限主体；权限主体仍由
  `PermissionRequest::actor` 表示。
- 新增 serde 测试覆盖：无 `origin` 时省略并 round-trip、带 `origin` 时 round-trip、旧 JSON 缺失
  `origin` 时反序列化为 `None`。
- Breaking change：`Interaction` 是公开字段结构体，本次新增公开字段会影响外部直接使用结构体字面量构造
  `Interaction` 的源码；既有 serde 数据向后兼容。
- 验证通过：`cargo fmt --all`；`cargo clippy --all-targets -- -D warnings`；
  `cargo clippy --all-targets --features "external-claude-code external-codex external-opencode external-acp" -- -D warnings`；
  `cargo test -p agent-lib --lib agent::interaction`；`cargo test --all --all-targets`；
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`。

### M1-2 [DONE] local subagent 路径：子 agent 交互应答路由到父级注入 handler

上下文：

- `FacadeSubagentSpawner::spawn`（`src/facade/delegate.rs:1465`）为子 agent 新建同步
  `FacadeApproval`（worker 自带 `ApprovalPolicy`），接线为 `ChildAgentScope` 的
  `InteractionHandler`（`delegate.rs:1374-1392`）；外层 pop 目标是无父级的 `EmptyScope`
  （`delegate.rs:1738-1739`）。
- supervisor 语义先例：注入 `interaction_handler` 是被暂停交互的唯一**应答方**，但哪些
  调用暂停仍由 `ApprovalPolicy` 控制（`src/facade/agent.rs:1265-1276`）。
- 父级 handler 的可得性：supervisor 作用域装配在 `agent.rs:504-508`（流式
  `src/facade/agent/stream.rs:441`），委派驱动在同 run 内，父 handler 句柄可沿
  `DelegationToolHandler::fulfill`（`delegate.rs:1992-2031`）/ spawner 传递。

实现要求：

- 父级注入了异步 `InteractionHandler` 时：子 agent 的 gate 仍用 worker 自己的
  `ApprovalPolicy`（不变），但被暂停交互的**应答**路由到父级 handler，并按 M1-1 标注
  归因（delegate 名 + `ctx.depth`）。
- 父级未注入时保持现状（worker 同步 policy 应答，headless ask 即 deny）——逐测试钉住
  两种模式。
- 异步语义不丢：路由全程保持 `InteractionHandler::fulfill` 的 async 暂停点；cancel 经
  `RunContext` 传播（父 handler 内 `ctx.cancellation()` 可用）。
- 子 agent 的 `Question`/`Choice`（若 worker 侧未来产生）走同一路由，不在本任务新增
  发射器。

验证条件：

- 单元测试（testkit scripted LLM）：supervisor 注入 recording handler + delegate 一个
  worker policy 为 ask 的 subagent → 子 agent 工具审批到达**父级** handler，归因字段正确，
  应答后子 agent 继续。
- 对照测试：supervisor 不注入 handler → 子 agent ask 走 worker 同步 policy（与现状一致）。
- cancel 测试：父 handler 挂起期间 cancel → 子 agent 交互以 deny/abandon 收尾，委派以
  cancelled/failed 结束，不挂死。
- `cargo test -p agent-lib --lib facade::delegate` 通过。

完成记录（2026-07-20）：

- `DelegationToolHandler` 现在携带可选的父级注入 `InteractionHandler`，并沿 local subagent
  spawner 传入子 drive；非流式、流式、rules/dispatcher 复用的委派 handler 构造路径均接入。
- 子 agent 仍用 worker 自己的 `ApprovalPolicy` / `FacadeApproval` 作为工具审批 gate；父级注入
  handler 存在时，由 `ChildInteractionRouter` 仅接管已暂停交互的 answer 路径，并用
  `InteractionOrigin { delegate, depth: ctx.depth() }` 标注渲染归因。
- 父级未注入 handler 时保持旧行为：子 `ChildAgentScope` 继续使用子 `FacadeApproval` fallback，
  worker 同步 ask handler/headless deny 语义不变。
- 子 interaction 转发保持 async 暂停点；路由层对 cancellation 做 `select!`，父级 handler 挂起且
  run cancel 时返回同 family 的取消/deny 结果，避免本地委派卡死。
- 新增/保留离线测试覆盖：父级 recording handler 收到带 origin 的子工具审批且 worker 同步 ask
  handler 未消费；无父级 handler 时 worker 同步 ask policy 被调用；父级 handler 永久挂起时 cancel
  在 2s 测试时限内收尾。
- 文档同步：`docs/facade-api.md` 说明 local subagent interaction routing 与 gate/answer 分工；
  `docs/mag-gaps.md` 标注 A1 的 M1-1/M1-2 已修复部分并保留 M1-3/M1-4 后续项。
- Breaking change：无公开 API 破坏；`DelegationToolHandler::new` 是 crate-private 构造签名调整。
- 验证通过：`cargo fmt --all`；`cargo test -p agent-lib --lib facade::delegate`；
  `cargo clippy --all-targets -- -D warnings`；
  `cargo clippy --all-targets --features "external-claude-code external-codex external-opencode external-acp" -- -D warnings`；
  `cargo test --all --all-targets`；`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`。

### M1-3 [DONE] external 委派路径：`NeedInteraction` 路由到父级 handler（替换 `EmptyExternalScope`）

上下文：

- `drive_external`（`src/facade/external.rs:1681`）接线 `ExternalChildScope`（只服务
  `external()`，1551-1559）+ 外层 `EmptyExternalScope`（1563）；注释自述 "The richer
  external approval wiring lands in M4-3"（1544-1550）未落地。
- ACP adapter 已把 `session/request_permission` 桥接为 `PausedForInteraction`（携带
  `Interaction::permission`，`src/agent/external/acp/adapter.rs:446-465, 500-511`），
  machine 具现 `NeedInteraction`（`src/agent/external/machine.rs:765-806`）——今天 pop 到
  空作用域即 `UnhandledRequirement`，**整个委派失败**。
- outcome 回灌方向已有 adapter 机制（permission response 经 ACP transport 返回），缺的
  只是 facade 层的路由。

实现要求：

- 替换/扩展 `EmptyExternalScope` 为可服务 `NeedInteraction` 的路由层：父级注入 handler
  时，把 external machine 具现的交互（含归因：delegate 名 + depth）转发父级 handler，
  应答映射回 `RequirementResult::Interaction` 喂给 machine。
- 父级未注入时保持现状失败语义（`UnhandledRequirement`），并在错误消息中明确原因
  （external agent 请求了权限但无 handler 可应答）。
- `ExternalPermissionMode::Prompt` 端到端打通后更新 `docs/managed-external-agent.md` 与
  `docs/capability-matrix.md` 对应描述。
- 其余 CLI adapter（claude_code/codex/opencode）的 permission 通道按各自 adapter 现状核查：
  若同样具现 `NeedInteraction`，同一路由层应覆盖；差异在完成任务记录中说明。

验证条件：

- 离线 e2e（`external-acp` feature，内存管道 / scripted ACP server 或 testkit fake）：
  ACP 子 agent 发起 `session/request_permission` → 父级 recording handler 收到带归因的
  Permission 交互 → 应答 allow → 子 agent 继续 → 委派成功。
- 对照：父级无 handler → 委派以明确错误失败（不是 hang）。
- `cargo test --features external-acp -p agent-lib --lib facade::external` 通过。

完成记录（2026-07-20）：

- `drive_external` 现在接收可选父级注入 `InteractionHandler`，并用 `ExternalInteractionScope` / `ExternalInteractionRouter`
  替换旧外层空 scope：external child 的 `NeedExternalSession` 仍由自身 session handler 服务，`NeedInteraction` pop 到外层路由。
- 父级 handler 存在时，external runtime 具现的 permission/question/choice/approval interaction 会带
  `InteractionOrigin { delegate, depth: ctx.depth() }` 转发给父级 handler，应答以
  `RequirementResult::Interaction` 回灌 external machine，再由 machine 发送 `RespondInteraction` 给 runtime。
- 父级 handler 不存在时保持 external headless 失败语义，但 `drive_external` 把 interaction family 的
  `UnhandledRequirement` 转成明确错误：external agent 请求了权限但没有 interaction handler 可应答，避免静默 hang。
- CLI adapter 核查：Claude Code 与 ACP 均可产生 `PausedForInteraction`，因此自动走同一路由层；Codex/OpenCode 当前自主运行，
  无 host-answerable permission pause，未来若具现 `NeedInteraction` 也会被该路由覆盖。
- 新增 `external-acp` gated 离线测试：scripted ACP-like handler 先返回 `PausedForInteraction`，父级 recording handler 收到带
  `origin { delegate: "coder", depth: 1 }` 的 permission interaction 并 approve，第二次 session request 为
  `RespondInteraction` 且委派成功；无父级 handler 的对照测试断言明确失败且只推进一次 session。
- 为保证 `external-acp` 单 feature 编译无 warning，`agent/external/process` 中 CLI-only transport/deadline/autonomous helper
  改为仅在 CLI adapter feature 下编译；ACP 仍保留共享 read/close/capability helper。
- 文档同步：`docs/facade-api.md`、`docs/managed-external-agent.md`、`docs/capability-matrix.md` 说明 external permission bridge
  的父级 handler 路由与 no-handler 语义；`docs/mag-gaps.md` 标注 A1 的 M1-3 已修复。
- Breaking change：无公开 API 破坏；`drive_external` 是 crate-private helper，签名调整只影响内部委派调用点。
- 验证通过：`cargo fmt --all`；`cargo test --features external-acp -p agent-lib --lib facade::external`；
  `cargo clippy --all-targets -- -D warnings`；
  `cargo clippy --all-targets --features "external-claude-code external-codex external-opencode external-acp" -- -D warnings`；
  `cargo test --all --all-targets`；`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`。

### M1-4 [DONE] external-start approval 去留评估与收口（A1 关联，可降级）

上下文：

- `FacadeApproval::resolve_external_start`（`src/facade/approval.rs:573`，调用点
  `delegate.rs:1825`）是 sync-only；`ApprovalPolicy::ask_external_agents()` 在无同步 ask
  handler 时 headless deny（测试 `delegate.rs:3362-3380`），表面为
  `FacadeError::ApprovalDenied`。
- mag 的替代模式：`ApprovalPolicy::ask_tool("ask_<name>")` 把「是否启动委派」经父级异步
  handler 在工具门审批——已可满足 mag 需求（mag-gaps A1 关联节允许本项降级）。

实现要求：

- 评估两条路：(a) external-start 决策也路由到注入的异步 handler（与 M1-2/M1-3 同通道，
  归因标注为启动决策）；(b) 保持 sync-only，文档明确推荐工具门模式。
- 选型标准：改动爆炸半径 vs 语义一致性。选 (b) 则本任务为纯文档任务（`facade-api.md`、
  `managed-external-agent.md` 写明 sync 语义 + 工具门推荐 + headless deny 陷阱）；
  选 (a) 则随 M1-2/M1-3 的路由层一并实现。
- 无论选哪条，`docs/mag-gaps.md` A1 关联节与 C5 的状态要同步标注。

验证条件：

- 选 (a)：异步审批 external-start 的测试（allow/deny 两条路径）。
- 选 (b)：文档落位；现有 sync 行为测试不变。
- 决策与理由写入完成记录。

完成记录（2026-07-20）：

- 评估结论选择方案 (a)。初始评估发现方案 (b) 的前提不成立：per-delegate external start 工具会被机器
  tool gate 豁免以避免双重审批，因此仅文档推荐 `ask_tool("ask_<name>")` 不能让启动决策进入父级异步
  `InteractionHandler`；该问题直接影响 M1-4 目标，故本任务改为实现异步路由。
- `FacadeApproval` 新增 crate-private `external_start_requires_ask`，用于 drive layer 在不触发同步 handler
  的情况下判断 external-start effective tier 是否为 ask；auto allow/deny 仍保持同步快速路径。
- `DelegationToolHandler` 在 managed external delegate 启动前，如策略为 ask 且 supervisor 注入了父级
  `InteractionHandler`，会构造 `InteractionKind::Approval` 并标注
  `InteractionOrigin { delegate, depth: ctx.depth() + 1 }` 转发父级 handler；只有 `Approve` 继续启动，
  `Deny` / `Timeout` / `Cancel` 或 family mismatch 均按启动被拒处理并表面为 `FacadeError::ApprovalDenied`。
- 父级未注入 handler 时保持既有同步 fallback：同步 `Approval::ask` handler 仍可批准/拒绝；无同步 handler
  的 headless `ask_external_agents()` 仍 deny，不挂起。
- 新增离线测试覆盖：`ask_external_agents()` + 父级 recording handler approve 时 external delegate 继续执行且
  start approval 带 `origin { delegate: "coder", depth: 1 }`；`ask_tool("ask_coder")` + 父级 handler deny
  时表面为 `ApprovalDenied` 且 external runtime 未被驱动。既有 auto_deny、headless deny、同步 ask handler
  测试保持通过。
- 文档同步：`docs/mag-gaps.md` 将 A1 关联节与 C5 标注为 M1-4 已修复；`docs/facade-api.md` 与
  `docs/managed-external-agent.md` 明确 external-start ask 的异步路由、同步 fallback/headless deny 语义，并修正
  无 handler 的 `.ask_external_agents()` 示例陷阱。
- Breaking change：无公开 API 破坏；新增 helper 为 crate-private，行为仅扩展为父级异步 handler 存在时可应答
  external-start ask。
- 验证通过：`cargo fmt --all`；`cargo test -p agent-lib --lib facade::delegate`；
  `cargo clippy --all-targets -- -D warnings`；`cargo test --all --all-targets`；
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`。

### M1-R [DONE] M1 review：委派交互路由收口

- 逐条核对 `docs/mag-gaps.md` A1（含关联节）的落地状态，标注 `✅`/`📄`。
- 核对归因语义一致性：local 与 external 两条路径的归因形状、`origin=None` 的 root 语义、
  `PermissionRequest.actor` 未受污染。
- 核对向后兼容：未注入 handler 的现有委派测试全部原样通过；`Interaction` serde 兼容。
- 全量门禁通过（含 `external-acp` feature 的 clippy 与测试）。
- 文档同步检查：`docs/facade-api.md`（interaction_handler 注入语义更新——从「supervisor
  唯一应答方」扩为「委派链统一应答方」）、`docs/managed-external-agent.md`（Prompt 模式
  端到端）、`docs/agent-layer.md`（如涉 scope 描述）。

完成记录（2026-07-20）：

- 已逐条核对 `docs/mag-gaps.md` A1 与关联 external-start approval：M1-1/M1-2/M1-3/M1-4 均已标注为
  `✅ 已修复`；C5 也已标注为 M1-4 已修复，无需新增降级项。
- 已核对归因语义：root `Interaction` 构造路径保持 `origin = None`；local subagent 与 external runtime
  permission/question/choice/approval 路由均使用 `InteractionOrigin { delegate, depth }`；external-start
  drive-layer approval 同样带 delegate/depth 归因；`PermissionRequest.actor` 仍仅表示权限主体，不被归因字段污染。
- 已核对向后兼容：`Interaction` 的 `origin` 字段为 serde 默认/省略字段，旧 JSON 仍反序列化为 `None`；未注入
  handler 的 local 委派仍走 worker 同步 policy/headless deny，external permission prompt 仍以明确错误失败，external-start
  仍保留同步 fallback/headless deny。
- 已核对文档同步：`docs/facade-api.md` 将 `interaction_handler` 语义扩展到 local 与 managed external 委派链，并说明
  gate/answer 分工；`docs/managed-external-agent.md` 记录 `ExternalPermissionMode::Prompt` 的 parent handler 端到端路由；
  `docs/agent-layer.md` 保持 scope pop/向上路由语义一致。
- Breaking change：无，本 review 未改变公开 API 或行为。
- 验证通过：`cargo fmt --all`；`cargo clippy --all-targets -- -D warnings`；
  `cargo clippy --all-targets --features "external-claude-code external-codex external-opencode external-acp" -- -D warnings`；
  `cargo test --all --all-targets`；`cargo test --features external-acp -p agent-lib --lib facade::external`；
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`。

---

## M2：facade reconfigure（A2）

### M2-1 [DONE] facade reconfig 入口 API 与校验

上下文：

- agent 层机制齐备：`AgentState::queue_reconfig`（`src/agent/state.rs:182-188`）、
  `ReconfigRequest` 八变体（`src/agent/state/queue.rs:186-229`）、turn 边界应用经
  `NeedReconfigRegistry` + `ReconfigRegistryHandler` / `ToolRegistryResolver`
  （`src/agent/drive/reference.rs:191-244`）。
- facade 从未接线（`src/facade/agent.rs:1679-1681`）；`docs/agent-layer.md` §4.2 已定
  语义：reconfig 只在 turn 边界，turn 中到达排队到 turn 结束。
- mag 的消费形态：`apply_config` 在 run 之间置 pending，turn 边界应用 `SetModel` /
  `ReplaceToolSet` / `SetSystemPromptOverlay`（mag `docs/CLI.md` §4.4）。

实现要求：

- facade 暴露 reconfig 入口（建议 `Agent::reconfigure(&mut self, request: ReconfigRequest)
  -> Result<(), FacadeError>`；若需批量可用 `Vec<ReconfigRequest>`）。
- 至少支持 `SetModel` / `ReplaceToolSet` / `PatchToolSet` / `SetSystemPromptOverlay`；
  skill 类三变体（Activate/Deactivate/ReplaceActiveSkills）与 `SetLoopPolicy`：评估透出
  成本，不透出则入口显式拒绝并文档化（不允许静默忽略）。
- 时机校验：run 进行中（stream 存活 / cursor 非 Idle）调用返回 `FacadeError::InvalidState`
  或按 agent-layer §4.2 语义排队——二选一，文档钉死；不得 turn 中直接生效。
- `ReconfigRequest` 的可见性：确认对下游可构造（目前在 `agent::state::queue`，评估
  re-export 路径）。

验证条件：

- 单元测试：Idle 时 `reconfigure(SetModel)` 被接受，下一 turn 的 LLM 请求用新 model
  （scripted client 断言 model 字段）。
- 单元测试：turn 中调用的行为与文档一致（InvalidState 或排队后 turn 末生效）。
- `cargo test -p agent-lib --lib facade::agent` 通过。

完成记录（2026-07-20）：

- 新增 `Agent::reconfigure(&mut self, request: ReconfigRequest) -> Result<(), FacadeError>`，通过底层
  `DefaultAgentMachine::reconfigure` 做准入校验并把请求排进 turn-boundary reconfig queue；`SetModel` 与
  `SetSystemPromptOverlay` 会在下一 turn 起点进入 LLM request 渲染。
- `facade` 现在重导 `ReconfigRequest`、`ToolSetPatch`、`ModelRef`、`ToolSetRef`、`LoopPolicy` 与
  `ToolFailurePolicy`，下游可从 facade 公共路径构造 reconfig 请求，无需依赖 `agent::state::queue` 内部路径。
- 支持 `SetModel` / `SetSystemPromptOverlay` / `ReplaceToolSet` / `PatchToolSet` / `SetLoopPolicy` 的入口准入；
  skill 三变体因 facade 尚无 skill registry 与 skill-to-prompt/tool 展开层，显式返回
  `FacadeError::InvalidState`，不静默忽略。
- facade 时机语义选择 `InvalidState`：只在 between-run/rest cursor（`Idle` / `Done` / `Error` /
  `CancelRecovery`）接受 reconfig；active/parked turn 拒绝，且 `AgentRunStream` 存活时的 `&mut Agent`
  借用从类型上阻止并发 reconfigure。
- 为让 tool-set declaration reconfig 通过入口准入，facade machine 装配声明型 resolver；live registry
  handler、流式/非流式接线和声明/执行闭包一致性仍按 M2-2 任务收口，本任务不把该后续项标为完成。
- 新增离线单元测试覆盖：Idle 时 `SetModel` + system overlay 下一 turn 生效并由 scripted client 断言请求；
  in-flight cursor 调用返回 `InvalidState`；skill 变体显式拒绝；`ReplaceToolSet` / `PatchToolSet` 入口准入。
- 文档同步：`docs/facade-api.md` 记录入口 API、支持/拒绝变体和时机语义；`docs/agent-layer.md` 区分底层
  machine 排队能力与 facade 的保守准入策略；`docs/mag-gaps.md` 更新 A2 现状并保留 M2-2/M2-3 待办。
- Breaking change：无，新增公开方法和重导均为 additive。
- 验证通过：`cargo fmt --all`；`cargo test -p agent-lib --lib facade::agent`；
  `cargo clippy --all-targets -- -D warnings`；`cargo test --all --all-targets`；
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`。

### M2-2 [DONE] reconfig handler 接线（流式 + 非流式）与 `ReplaceToolSet` 一致性

上下文：

- facade 两条驱动路径：非流式 collector（`src/facade/agent.rs` run/run_full）与流式
  （`src/facade/agent/stream.rs`）；reconfig requirement 需要 `ReconfigRegistryHandler` +
  `ToolRegistryResolver` 服务（`src/agent/drive/reference.rs:191-244`）。
- 一致性陷阱（snapshot 路径同款教训，`src/facade/agent/snapshot.rs:656-664`）：执行闭包
  按名字解析，声明与注册表不一致是**静默** mismatch。

实现要求：

- 两条驱动路径都接线 reconfig handler；`ReplaceToolSet` / `PatchToolSet` 到达时，
  执行侧注册表同步替换（推荐 facade 持 `Arc<dyn ToolRegistry>` 整体换），或对声明集合
  与注册表名字集合做校验、不一致即 fail 该 reconfig（`RequirementResult::Reconfig(Err)`）
  并表面为 run 错误——**不允许静默 mismatch**。
- reconfig 应用后 `state.current_tool_set()` / `current_model` 与后续 LLM 请求渲染一致
  （`src/agent/machine/default/mod.rs:689,717`）。
- reconfig 结果要有 run 事件或返回值层面的可观测性（至少 trace；若新增 `RunEvent`
  变体，`WireRunEvent` 同步投影——评估后定，文档说明）。

验证条件：

- 单元测试：流式与非流式各一条——`ReplaceToolSet` 后模型看到的工具声明更新、新工具可
  执行、被移除的工具调用得到明确错误。
- 单元测试：声明与注册表名字不一致时按设计失败（非静默）。
- `cargo test -p agent-lib --lib facade::` 通过。

完成记录（2026-07-20）：

- 非流式 `FacadeAgentScope` 与流式 `FacadeStreamScope` 均已接线 `ReconfigRegistryHandler`；两条路径的
  `ToolRegistryHandler` 与 reconfig handler 共享同一个 active registry slot，`NeedReconfigRegistry` 会在
  turn 边界换入新 registry 后再恢复 machine。
- 新增 facade 专用 `FacadeToolRegistryResolver` / active filtered registry：queue-time validation 与
  apply-time swap 使用同一 resolver；`ReplaceToolSet` / `PatchToolSet` 的目标声明名字必须来自该 agent
  已注册的 facade tool surface。声明/执行集合不一致会在 `Agent::reconfigure` 准入时显式
  `FacadeError::Agent(AgentError::Tool(..))` 失败，不再排队成静默 mismatch。
- 每个 run 的初始 tool registry 也按 `state.current_tool_set()` 过滤，避免上一轮移除的工具在下一轮因
  registry 重新构造而恢复执行能力；被移除工具若仍被模型调用，会得到明确 `UnknownTool` tool result，
  不执行旧闭包。
- reconfig 可观测性选择不新增 `RunEvent` 变体：完成后可通过 `AgentState::current_tool_set`、后续 LLM
  request 的 `tools` 字段以及 removed-tool 的错误 tool result 观察；文档已在 `docs/facade-api.md` 与
  `docs/agent-layer.md` 说明该口径。
- 新增/更新离线测试覆盖：非流式 `ReplaceToolSet` 后只声明/执行新 active 工具且旧工具返回错误；非流式
  `PatchToolSet` 后同步过滤 registry；流式 `ReplaceToolSet` 后 request tools 与执行路径一致；未注册工具声明
  显式失败而非静默 mismatch。
- 文档同步：`docs/facade-api.md` 记录 reconfig tool-set resolver、active registry slot 与可观测性；
  `docs/agent-layer.md` 记录 facade 两条 drive path 的 handler 接线；`docs/mag-gaps.md` 将 A2 的
  live registry 一致性标注为 `✅ 已修复（M2-2）`，保留 M2-3 snapshot/restore 后续项。
- Breaking change：无公开 API 破坏；行为变化是更严格地拒绝 facade registry 未注册的 tool-set
  reconfig 声明，避免之前 M2-1 中 declared-only resolver 允许的不可执行声明入队。
- 验证通过：`cargo fmt --all`；`cargo test -p agent-lib --lib facade::agent`；
  `cargo test -p agent-lib --lib facade::`；`cargo clippy --all-targets -- -D warnings`；
  `cargo test --all --all-targets`；`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`；
  `cargo clippy --all-targets --features "external-claude-code external-codex external-opencode external-acp" -- -D warnings`。
- 后续修正（2026-07-20 review）：委派工具（`ask_<name>`）的移除绕过了过滤 registry——
  `DelegationToolHandler` 的固定 route 先于 `allowed_names` 检查解析，被 reconfig 移除的委派
  工具仍可驱动委派。该需求违反由新增任务 **M2-3** 收口，本任务的「完成」不含委派工具路径。

### M2-3 [DONE] 委派工具移除一致性修复

上下文：

- `DelegationToolHandler::fulfill`（`src/facade/delegate.rs:2136`）经 `self.route.resolve(call)`
  ——per-run 固定的 `DelegationRoute`（按注册 delegates 构建，`src/facade/agent.rs:709-712`）——
  **先于** `ActiveFacadeToolRegistry` 的 `allowed_names` 检查
  （`src/facade/agent/reconfig.rs:180-183`）解析委派调用。
- 后果（review probe 实测确认）：`ReplaceToolSet`/`PatchToolSet` 移除 `ask_reviewer` 后，模型调它
  仍驱动完整委派（`DelegationTrace{status: Completed}`），而非 UnknownTool。违反 M2-2「被移除的
  工具若仍被模型调用，会得到明确 `UnknownTool` tool result」的硬性要求；`docs/facade-api.md` 与
  `docs/agent-layer.md` 的对应描述当前与实现不符（文档承诺先行）。
- mid-run swap 路径同洞：registry slot 换入新 registry 时 route 不重建。

实现要求：

- 委派路由解析（`Resolved::Delegate` / `Resolved::External`）以**当前 active tool set / slot
  registry 的声明集**为准：被 reconfig 移除的 `ask_<name>` 调用返回 UnknownTool tool result，
  不驱动委派。
- run 起点与 mid-run swap 两条路径一致（route 按 active tool set 重建，或解析时查 slot
  registry——选型写入完成记录）。
- 未被移除的委派工具行为不变（现有委派测试全绿）。

验证条件：

- 单元测试：`ReplaceToolSet` 移除 `ask_reviewer` → 下一 run 模型调它得到 UnknownTool，无
  `DelegationStarted` 事件；保留的委派工具正常驱动。
- `PatchToolSet` 移除路径同样覆盖（review 指出的测试缺口）。
- `cargo test -p agent-lib --lib facade::` 通过。

完成记录（2026-07-20）：

- 选型：**解析时查 slot registry**（而非按 active tool set 重建 route）。`DelegationToolHandler`
  的 base `ToolRegistryHandler` 本就持有共享 `SharedRegistry` slot——run 起点的过滤 registry 与
  turn 边界 `ReconfigRegistryHandler` 换入的新 registry 是同一个 slot 的不同安装值。在 `fulfill`
  解析前查一次当前 slot registry 的声明集（`ActiveFacadeToolRegistry::declarations()` 与其
  `allowed_names` 同源），一个检查点同时覆盖两条路径，不存在「route 忘了重建」的第二份状态；
  重建 route 方案则需要把 swap 通知从 agent 层 reconfig handler 穿回 facade handler，状态双写，
  被否。为此把 agent 层 `ToolRegistryHandler::current()` 放宽为 `pub(crate)`（仅 crate 内可见，
  公开 API 不变）。
- 行为：`fulfill` 中 `route.is_delegation(&call.name)` 为真但当前 registry 不再声明该名时，直接
  返回 `ToolRuntimeError::UnknownTool` tool result，不记录 trace、不驱动委派——与普通被移除工具
  经过滤 registry 得到的结果逐字一致。流式 `TapToolHandler` 换用同一谓词
  （`DelegationToolHandler::is_active_delegation`），被移除的委派工具落入普通 ToolStarted/
  ToolFinished 括号，不再发出 `DelegationStarted`/`DelegationFinished`；未移除的委派工具路径
  逐字节不变（谓词在声明集包含该名时退化为原 `is_delegation`）。
- 文件：`src/agent/drive/reference.rs`（`current()` 放宽 `pub(crate)`）、
  `src/facade/delegate.rs`（`fulfill` 前置 active-set 检查；`is_delegation` 换成
  `is_active_delegation` + 私有 `active_set_declares`；struct rustdoc 记录该语义）、
  `src/facade/agent/stream.rs`（tap 改用 `is_active_delegation`）、
  `src/facade/agent/tests.rs`（新增两个测试 + `delegating_agent` fixture）、
  `docs/mag-gaps.md`（A2 标注 ✅ 已修复（M2-3），并把 snapshot/restore 后续项指向 M2-4）、
  `docs/facade-api.md`（同上，仅把「M2-3 继续钉住」改为 M2-4；UnknownTool 承诺本就先行，无需改
  行为描述）。
- 测试（全部离线、ScriptedClient）：`reconfigure_replace_tool_set_removing_a_delegate_yields_unknown_tool`
  ——ReplaceToolSet 移除 `ask_reviewer` 后下一 run：首个 LLM request tools 只剩
  `ask_researcher`，模型调 `ask_reviewer` 的 tool result 为 `unknown tool `ask_reviewer``，事件流
  无 reviewer 的 DelegationStarted，保留的 `ask_researcher` 仍完成
  DelegationStarted/Finished 括号；`reconfigure_patch_tool_set_removing_a_delegate_yields_unknown_tool`
  ——PatchToolSet 移除路径同样钉住 UnknownTool 与零委派事件。
- 验证通过：`cargo fmt --all`；`cargo test -p agent-lib --lib facade::`（254 passed）；
  `cargo clippy --all-targets -- -D warnings`；`cargo clippy --all-targets --features
  "external-claude-code external-codex external-opencode external-acp" -- -D warnings`；
  `cargo test --all --all-targets`；`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`。
- Breaking change：无（仅 `pub(crate)` 可见性调整与行为补齐，公开 API 形状不变；行为变化是让
  文档已承诺的 UnknownTool 语义对委派工具成真）。

### M2-4 [DONE] reconfig 与 snapshot/restore 的交互确认 + 准入校验补齐 + 文档收口

上下文：

- `Agent::snapshot()` 捕获 `AgentState`（`src/facade/agent/snapshot.rs:163-199`），只能在
  committed 一致点取（`agent.rs:987-993`）。
- reconfig 排队在 turn 边界应用——与「快照在 committed 点」的时序要保证 reconfig 落进
  state 先于快照读取，否则 snapshot 丢失已应用的 reconfig。
- **review 实测（2026-07-20）**：restore 只查重不校验快照 `current_tool_set` ⊆ 重注入工具面
  （`snapshot.rs:857-864`）；工具面偏小时每次 run 在 drain 开始前即失败
  （`agent.rs:754-757` `tool_handlers_for_run`），排队中的矫正性 `ReplaceToolSet` 永远没机会
  应用——agent 被永久锁死，`into_parts` 之外无解。
- **review 实测**：`SetModel` 准入跳过 builder 校验（`agent.rs:1496-1523` 的空白 model 名 /
  非有限 temperature / provider_extras 匹配检查），`SetModel { model: "" }` 或 NaN temperature
  会被接受并原样渲染进下一个 `ChatRequest`（`src/agent/request.rs:31-43`）。
- **review 实测**：facade re-export 不全——缺 `ToolSetId`（`ToolSetRef::new` /
  `ToolSetPatch::new` 必需）与声明类型 `model::tool::Tool`；crate 自己的测试都得从
  `crate::agent` 内部路径 import（`src/facade/agent/tests.rs:22-27`），facade-only 消费者
  构造不了 `ReplaceToolSet`。

实现要求：

- 测试钉住：reconfig（SetModel + ReplaceToolSet）→ run 完成 → snapshot → restore →
  恢复的 agent 用新 model/工具集（restore 以快照 `AgentState` 为权威，
  `snapshot.rs:864-869`，本就该成立——用测试锁住）。
- **restore 锁死修复**：restore 时校验快照 `current_tool_set`（与 queued reconfigs）⊆
  重注入工具面，不满足即 restore 显式报错；或 run 起点按 post-application 集合解析使排队
  矫正有机会应用——二选一，选型与理由写入完成记录。不允许出现「公开 API 无法恢复」的状态。
- **`SetModel` 准入校验**：对齐 builder 的校验（非空 model、有限 temperature、
  provider_extras 与 provider 匹配），不合法即准入失败、不排队。
- **re-export 补齐**：facade 公共路径可构造全部已支持 reconfig 请求（补 `ToolSetId` 与
  工具声明类型）。
- 边界情形：reconfig 排队未应用时取 snapshot 的语义（丢弃 or 含排队）文档化——
  `Agent::snapshot()` rustdoc 目前未说明 queued reconfig 会被快照捕获并在 restore 后重新
  计划，此语义必须钉死并文档化。
- 文档同步：`docs/facade-api.md`（reconfig 入口、时机语义、与 restore 的分工——restore
  仍是换 provider/client/handler 的路径，reconfig 是换 model/tools/system 的路径）、
  `docs/agent-layer.md` §4.2（更新为「facade 可达」）。

验证条件：

- 上述 snapshot/restore 测试通过；restore 工具面校验的允许/拒绝两路径各有测试。
- `SetModel` 非法输入（空白 model / NaN temperature）准入失败测试。
- facade-only 路径（不 import `agent::` 内部模块）构造 `ReplaceToolSet` 的编译测试或 doc
  示例。
- 文档落位，无「机制齐备但 facade 不可达」的过时描述残留。

完成记录（2026-07-20）：

- **restore 锁死修复选型：方案 (a)——restore 时显式校验、失败即报错。** 在
  `AgentRestoreBuilder::build` 反序列化 `AgentState` 之后、装配 machine 之前，用与 run 路径同一个
  `FacadeToolRegistryResolver` 校验两块：(1) 快照 `current_tool_set` ⊆ 重注入工具面；(2) 排队未应用
  reconfig 队列 plan 出的 post-application tool set ⊆ 重注入工具面。任一不满足即以
  `FacadeError::InvalidState`（带缺失工具名）使 restore 失败。理由：run 起点按 `current_tool_set`
  解析 active registry 先于任何 turn 边界 drain，方案 (b)（run 起点按 post-application 集合解析）会
  让快照权威的当前集合被静默跳过、语义上等于restore 偷偷应用了排队 reconfig，且把宿主少注入工具的
  真实错误掩盖成一次隐式配置变更；fail-fast 更诚实，且与 restore 已有的声明面/查重校验同层同时机。
  排队 reconfig 的考虑：队列随 `AgentState` 序列化（`queued_reconfigs` 字段），restore 时
  `from_record` 会重新 plan；方案 (a) 因此必须连 post-application 集合一起校验，否则排队矫正
  ReplaceToolSet 指向缺失工具时首个 run 仍会在 turn 边界失败——校验已覆盖该路径（测试钉住）。
  修复后不存在「公开 API 无法恢复」的状态：要么 restore 成功且每个 run 起点/边界都可解析，要么
  restore 当场报错、调用方补足工具重试。
- **排队未应用 reconfig 的快照语义（钉死）**：`Agent::snapshot()` **捕获**排队队列——
  `queued_reconfigs` 是 `AgentState` 序列化记录的一部分，restore 后 agent 保留该队列并在下一次
  run 的 turn 边界照常应用（不丢弃、不提前应用），行为与从未取过快照一致。已写入
  `Agent::snapshot` rustdoc、`AgentRestoreBuilder::build` # Errors、snapshot 模块 doc 与
  `docs/facade-api.md` §8.2/§15.2。
- **`SetModel` 准入校验**：`Agent::reconfigure` 在 machine 排队前对 `SetModel` 跑与
  `AgentBuilder::build` 对齐的校验（复用 `ensure_non_blank_model` / `ensure_finite_temperature` /
  `ensure_provider_extras_match_provider`），非法输入以 `FacadeError::Config` 失败、不排队。
  provider_extras 的 provider 比对以**当前 model 的 extras 所指 provider** 为可推断基准（facade 不
  保留 builder 的 `ProviderConfig`，注入 client 无可靠 provider id）；两侧都无 extras 时按 builder
  client-only 逃生口语义放行，由注入 client 决定。
- **re-export 补齐**：`agent_lib::facade` 新增 `ToolSetId`（agent 层重导）与
  `ToolDecl`（`model::tool::Tool` 的 facade 别名，区别于可执行的 typed `Tool`）；facade-only
  消费者可构造全部五种已支持 reconfig 请求。编译级证明双保险：`Agent::reconfigure` rustdoc 新增
  facade-path-only 构造 `ReplaceToolSet` 的 doc 示例（doctest 通过），测试内新增
  `facade_surface` 模块只 import `crate::facade::` 路径构造全部五种请求。
- 文件：`src/facade/mod.rs`（`ToolSetId` / `ToolDecl` re-export）、`src/facade/agent.rs`
  （reconfigure 准入加 `ensure_facade_set_model_valid`；reconfigure/snapshot rustdoc 更新 + doc
  示例）、`src/facade/agent/snapshot.rs`（`ensure_restored_tool_surface`；build 内 resolver 构造
  前移；build # Errors 与模块 doc 更新）、`src/facade/agent/tests.rs`（8 个新测试 +
  `weather_tool_decl` helper）、`docs/facade-api.md`（§8.2 reconfig 准入/分工/时机，§15.2 工具面
  校验与排队捕获语义）、`docs/agent-layer.md`（§4.2 M2-4 段落）、`docs/mag-gaps.md`（A2 状态与
  三条 ✅ 已修复（M2-4）标注）。
- 测试（全部离线、ScriptedClient）：`snapshot_restore_preserves_applied_model_and_tool_set_reconfig`
  （SetModel + ReplaceToolSet → run 应用 → snapshot → restore → 恢复 agent 的 state 与下一 LLM
  request 都用新 model/工具集）；`snapshot_captures_queued_unapplied_reconfigs_for_restore`（排队
  SetModel 经 JSON round-trip 后被快照捕获，restore 后队列仍在、下一 run 边界应用）；
  `restore_rejects_a_surface_missing_current_tool_set_tools`（缺当前集合工具 → InvalidState；
  补足 → 恢复成功，允许/拒绝两路径）；
  `restore_rejects_a_surface_missing_a_queued_reconfig_tool_set`（当前集合 ⊆ 工具面但排队矫正
  ReplaceToolSet 的应用结果 ⊄ → InvalidState；补足 → 成功）；
  `reconfigure_rejects_blank_set_model_at_admission` /
  `reconfigure_rejects_non_finite_set_model_temperature_at_admission` /
  `reconfigure_set_model_provider_extras_must_follow_the_current_provider`（准入失败且不排队 +
  匹配 provider 放行）；`facade_surface::facade_paths_construct_every_supported_reconfig_request`
  （facade-only 编译测试）。
- Breaking change：**restore 对工具面不覆盖快照可激活 tool set 的情形新增强制失败**
  （`FacadeError::InvalidState`）——此前这类 restore 能build 成功但之后每次 run 都失败（agent 锁死），
  现在恢复阶段即报错；任何依赖「少注入工具也能 restore 成功」的调用方会观察到行为变化。其余无公开
  API 形状破坏（只新增 re-export 与准入校验）。
- 验证通过：`cargo fmt --all`；`cargo test -p agent-lib --lib facade::`（262 passed，+8）；
  `cargo clippy --all-targets -- -D warnings`；`cargo clippy --all-targets --features
  "external-claude-code external-codex external-opencode external-acp" -- -D warnings`；
  `cargo test --all --all-targets`（50 套件全绿）；`RUSTDOCFLAGS="-D warnings" cargo doc
  --no-deps --workspace`；`cargo test --doc -p agent-lib`（新增 reconfigure doc 示例通过）。

### M2-R [DONE] M2 review：facade reconfigure 收口

- 逐条核对 `docs/mag-gaps.md` A2 的落地状态并标注。
- 核对 mag 验收线索：turn 边界可换 model/tools/system，会话历史保留，snapshot/restore
  不丢 reconfig。
- 核对「不允许静默 mismatch」在代码与测试中都成立——含 M2-3 的委派工具移除一致性、
  M2-4 的 restore 工具面校验。
- 核对 review 指出的 should-fix 已收口：`SetModel` 准入校验、facade re-export 完整性。
- 核对 review nit 的处置（逐条决定修复或登记）：声明内容仅按名校验（同名不同 schema 被
  接受）、skill 变体拒绝与时机拒绝同为 `InvalidState` 不可区分、`available_names` 在
  build/restore 时冻结、`SetLoopPolicy` 覆盖 facade 派生 budget 的交互未文档化。
- 全量门禁通过。

完成记录（2026-07-20）：

- **A2 状态标注核对**：`docs/mag-gaps.md` A2 全部子条目已带正确任务 id 的 `✅ 已修复`
  标注（M2-1/M2-2/M2-3/M2-4）；本轮把 M2-1 条目从「已接线（M2-1）」补齐为
  「✅ 已修复（M2-1）」统一口径，状态总述补充 skill 拒绝变体为 `Config` 的说明，无
  「未实现」的过时描述残留。
- **mag 验收线索核对（读测试，未重跑全量）**：turn 边界换 model/system 由
  `reconfigure_set_model_and_overlay_apply_at_next_turn_start` 钉住（scripted client 断言下一
  request 的 model/system 字段），换 tools 由非流式 Replace/Patch 与流式 Replace 三个
  registry 测试钉住；reconfig 不触碰 `Conversation`，会话历史保留由 `Conversation` 累积
  committed turn 的既有不变量与 `snapshot_then_restore_continues_history` 覆盖；
  snapshot/restore 不丢已应用 reconfig（`snapshot_restore_preserves_applied_model_and_tool_set_reconfig`）
  且捕获排队未应用队列（`snapshot_captures_queued_unapplied_reconfigs_for_restore`）。
  无静默 mismatch：M2-2 准入校验（`reconfigure_rejects_tool_set_not_backed_by_facade_registry`）、
  M2-3 委派 gating（`reconfigure_replace/patch_tool_set_removing_a_delegate_yields_unknown_tool`）、
  M2-4 restore 工具面校验（`restore_rejects_a_surface_missing_current_tool_set_tools` /
  `restore_rejects_a_surface_missing_a_queued_reconfig_tool_set`）均在测试钉住，无缺口需补。
- **nit 处置逐条**：
  a. 准入仅按名字校验声明——判定 BY DESIGN，已在 `docs/facade-api.md` §8.2 reconfigure
     段新增一段：名字被校验，description/input_schema 是调用方提供的展示元数据（原样渲染进
     LLM request），执行永远使用注册闭包，同名不同 schema 的声明被接受是有意设计。
  b. skill 变体拒绝从 `FacadeError::InvalidState` 改为 `FacadeError::Config`，与 M2-4
     `SetModel` 准入失败同族——请求内容问题报 `Config`，时机问题（active/parked turn）保留
     `InvalidState`，两者可区分。更新 `ensure_facade_reconfig_request_supported`、
     `Agent::reconfigure` rustdoc（含 # Errors）、`docs/facade-api.md` 与测试
     `reconfigure_rejects_skill_requests_explicitly`。**行为变化（minor）**：skill 变体拒绝的
     错误变体改变，按变体判等的下游会观察到差异；消息文本不变。
  c. `available_names` 在 build/restore 时冻结——登记为已知限制，写入
     `docs/facade-api.md` §7.3 与 `AgentBuilder::tool_registry` rustdoc（动态 custom registry
     后续改声明不会更新准入面）。
  d. `SetLoopPolicy` 静默取代派生 budget——已在 `docs/facade-api.md` §8.2 文档化：
     `SetLoopPolicy` 整体替换 `min(max_steps, max_tool_rounds + 1)` 派生 budget，builder 的
     `max_steps` / `max_tool_rounds` 此后不再参与计算。
- **should-fix 收口核对**：`SetModel` 准入校验（三个 M2-4 测试）与 facade re-export 完整性
  （`facade_surface::facade_paths_construct_every_supported_reconfig_request` 编译级测试 +
  reconfigure doctest）均已收口，无遗留。
- Breaking change：无公开 API 形状破坏；唯一行为变化是 nit b 的 skill 变体拒绝错误变体
  （`InvalidState` → `Config`）。
- 验证通过：`cargo fmt --all`；`cargo clippy --all-targets -- -D warnings`；
  `cargo clippy --all-targets --features "external-claude-code external-codex external-opencode external-acp" -- -D warnings`；
  `cargo test --all --all-targets`（50 套件全绿）；`cargo test -p agent-lib --lib facade::`
  （262 passed）；`cargo test --doc -p agent-lib`（12 passed）；
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`。

---

## M3：cancel 强化（A3 + A4）

### M3-1 [DONE] ACP read loop 取消响应（不再等满 120s）

上下文：

- ACP read loop 只在行间隔查 `ctx.is_cancelled()`（`src/agent/external/acp/adapter.rs:431`）；
  读超时 120s（`src/agent/external/acp/connection.rs:157-160`，facade 侧
  `src/facade/external.rs:1006` 的 `DEFAULT_EXTERNAL_IO_TIMEOUT`）。子进程静默时 cancel
  最长阻塞 120s。
- 三个 CLI adapter 的行读带独立 idle 超时（2026-07-19 已修，语义不同），按现状评估
  是否需同样处理。

实现要求：

- ACP 读循环改为 `select!`（read line vs cancellation），cancel 触发后秒级返回；
  取消路径走既有 abandon/cleanup 语义（machine `abandon` 置 `cleanup_required`，
  `src/agent/external/machine.rs:1571-1589`）。
- 不破坏正常慢响应：120s IO 超时仍作为最后的错误路径保留（对端真死）。
- 其余 CLI adapter 读循环核查结论写入完成记录（改或说明不改的理由）。

验证条件：

- 单元测试（`external-acp`，内存管道）：对端静默（永不写行）时发起 cancel，断言驱动在
  秒级（测试用短时限，如 2s）内以 cancelled 收尾，不等读超时。
- 正常路径回归：scripted ACP server 正常应答的测试不受影响。
- `cargo test --features external-acp -p agent-lib --lib agent::external::acp` 通过。

完成记录（2026-07-20）：

- **机制选型：`tokio::select!`（biased）行读 vs `CancellationToken::cancelled()`。** 四个 adapter 的
  `advance` 读循环统一改为：`select! { biased; () = ctx.cancellation().cancelled() => 取消,
  line = self.read_line() => 行 }`；取消分支返回与原行间检查完全相同的
  `ExternalAgentError::SessionLost { detail: "<runtime> session advance was cancelled" }`，
  因此取消仍沿既有链路传导——advance 快速返回 → `fulfill_batch` 落定 → drain 复査
  `is_cancelled()` → `StepInput::Abandon` → machine `abandon` 置 `cleanup_required`（语义不变，
  由 facade 既有测试 `drive_external_marks_cleanup_on_cancel` 钉住）。`biased` 保证已落地 cancel
  优先于同时就绪的行；`cancelled()` 对预取消 token 立即就绪，行间预检随之冗余而移除。读/空闲
  超时原样保留在 `read_line` 内部，仍是对端真死的最后错误路径；慢但存活的对端行为不变。
- **CLI adapter 核查结论：三者存在同一结构性缺陷且更严重，按「clearly warranted」一并修复。**
  claude_code/codex/opencode 的 advance 循环与 ACP 同构（只在行间查 `is_cancelled()`），且其
  `read_idle_timeout` 默认 600s（故意拉长以免把长静默构建误判为死 CLI）——静默 CLI 上 cancel
  最坏阻塞 600s，比 ACP 的 120s 更糟。修复形状与 ACP 完全一致、错误类型与文案不变、空闲超时
  语义不动，故同补丁收口而非留尾巴。begin/prelude 循环未改：它们已受 launch timeout 总
  deadline（默认 30s）约束并逐轮查取消，cancel 延迟有界，不属于本次的稳态读循环缺陷。
- 文件：`src/agent/external/acp/adapter.rs`（`read_to_decision` 改 select! + 测试）、
  `src/agent/external/claude_code/adapter.rs` / `codex/adapter.rs` / `opencode/adapter.rs`
  （advance 读循环改 select! + 各自 fake 增加「脚本耗尽后永久 pend」模式 + 测试）、
  `docs/managed-external-agent.md`（能力矩阵 cancel 行与 §12.1 `read_idle_timeout` 段补 M3-1
  口径）、`docs/mag-gaps.md`（A3 第 1 条标 ✅ 已修复（M3-1），第 2 条留 M3-2）。
- 测试：每 adapter 各 1 个静默对端取消测试（ACP 用 `ScriptedThenSilent` 内存 reader + 60s 读
  超时反衬；三 CLI 用各自 FakeIo/FakeTurn 的 `silent_after_script` 模式）——handshake 正常完成后
  advance 挂起，50ms 后 cancel，断言 2s 内以 `SessionLost`（detail 含 "cancelled"）收尾，证明走的
  是取消路径而非读超时。正常路径回归：ACP 39 个测试、external 全特性 375 个测试全绿。
- 门禁：`cargo fmt --all` ✅；`cargo test --features external-acp -p agent-lib --lib
  agent::external::acp` ✅（39 passed）；`cargo clippy --all-targets -- -D warnings` ✅；
  `cargo clippy --all-targets --features "external-claude-code external-codex external-opencode
  external-acp" -- -D warnings` ✅；`cargo test --all --all-targets` ✅（全 suite 0 失败）；
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` ✅。
- Breaking change：无（公有 API 形状未动，仅 advance 在取消时的返回时机提前；取消时返回的错误
  类型与 detail 文案与改动前一致）。

### M3-2 [DONE] cancel/abandon 时 external session 清理（子进程不泄漏）

上下文：

- facade 从不调 `ExternalSessionRegistry::cleanup_agent/cleanup`
  （`src/agent/external/registry.rs:406/448`）；机器「从不自发 Shutdown——force-close 是
  handle 层职责」（`src/agent/external/handler.rs:39-45`）。
- ACP adapter 的 `shutdown()` 发 best-effort `session/cancel` + 关 transport
  （`src/agent/external/acp/adapter.rs:741-748`）；进程组终止在 process 层
  （`src/agent/external/process/`，SIGTERM→SIGKILL）。
- mag 今天可自己持有 registry sweep（`default_external_session_handler` 经 `.registry()`
  暴露），但目标是「宿主不做额外动作也不泄漏」（mag-gaps A3 验收标准）。

实现要求：

- 在 facade 驱动路径上，drive 以 abandon/cancelled 收尾（`cleanup_required`）时自动触发
  对应 session 的清理（shutdown + 进程终止 + ephemeral worktree 按既有策略处置）；
  或在 facade 暴露一等清理入口（如 `Agent`/`ManagedExternalAgent` 上的显式方法）并在
  cancel 文档中强制要求调用——二选一，优先自动清理，选型理由写入完成记录。
- 正常结束（committed）的 session 语义不变（worktree 干净拆除/脏保留的既有策略不动）。
- 进程退出路径：facade `Agent` drop 时有在管 external session 的处理，文档化（至少
  不静默泄漏；若无法自动清理，rustdoc 明确要求宿主 sweep）。
- 文档同步：`docs/managed-external-agent.md`（清理责任归属从「宿主全责」更新）。

验证条件：

- 单元测试（`external-acp`，fake ACP server 子进程或内存替身）：drive cancel → 断言
  收到 `session/cancel`/transport 关闭，registry 中 session 终态正确，无残留句柄。
- worktree 策略回归测试不受影响。

完成记录（2026-07-20）：

- **选型：自动清理（非一等入口 + 文档强制）。** 机制是在 `ExternalSessionHandler`
  （`src/agent/drive.rs`）新增 trait 级清扫钩子 `cleanup_agent(agent_id) ->
  Vec<ExternalSessionShutdown>`（默认 no-op，向后兼容），facade 驱动路径 `drive_external`
  在 drive **未 committed 收尾**时自动调用它。理由：(a) 验收标准是「宿主不做任何额外动作
  也不泄漏子进程」，文档强制的入口依赖宿主记得调用，达不到该标准；(b) 钩子挂在
  handler 而非 facade 新类型上，是因为 live runtime 的所有权本来就在 handler 层
  （「force-close 是 handle 层职责」，design §16），facade 只持有 `Arc<dyn
  ExternalSessionHandler>`，trait 默认方法让既有自定义 handler 无需改动即可编译；
  (c) sweep 条件取 `!completed` 而非仅 `cleanup_required`——cancel 落在无 outstanding
  requirement 的窗口（session 已 Completed 但 drive 未收尾）时机器不会置标记，而
  never-resume 语义下该 session 同样该杀；drive 失败（如无 interaction handler 应答
  permission）遗留的 live session 因 agent_id 是每次 drive 新铸、宿主无从知晓，本来
  就是不可达的泄漏，一并扫掉。committed drive 完全不触碰（live session 保留供
  Attachable 复用，worktree 干净拆除/脏保留策略不变）。
- **接线**：`RegistryExternalSessionHandler::cleanup_agent` 转发 registry
  `cleanup_agent`（shutdown = best-effort `session/cancel` + transport close + 进程组
  终止 + worktree 按 disposition 处置，全部复用既有 registry 路径）；
  `ExternalUsageChargingHandler` 透明转发 inner（否则包装后清扫被默认 no-op 吞掉）；
  testkit `ScriptedRuntimeExternalSessionHandler` / `CassetteRuntimeExternalSessionHandler`
  同样转发各自 registry，保持「production 形状」。sweep 的每个 disposition best-effort
  记入 run trace（`external-cleanup-sweep/{run_id}/{seq}` 节点），与 design §6.4/§10
  「handle 层记录 disposition」一致。agent_id 每次 drive 新铸，sweep 精确限定本 drive
  的 session。
- **Drop 语义（文档化，非自动）**：facade `Agent` rustdoc 新增「Managed external
  sessions and teardown (M3-2)」一节——drop 时 registry 随之 drop，直接子进程有
  `kill_on_drop` 兜底回收（不静默泄漏直接子进程），但不跑 `session/cancel`/
  disposition 分类/进程组终止（孙进程可能残留）/worktree 清扫；需要分类化 teardown
  的宿主必须在 drop 前经 `RegistryExternalSessionHandler::registry().cleanup_agent(..)`
  显式 sweep。异步 cleanup 无法在 `Drop` 里可靠执行（无 runtime 保证），故按任务书
  允许的「rustdoc 明确要求宿主 sweep」处理。
- **文件**：`src/agent/drive.rs`（trait 钩子 + 默认 no-op 测试）、
  `src/agent/external/handler.rs`（override + 模块/registry() 清理文档更新）、
  `src/agent/external/budget.rs`（charging wrapper 转发）、
  `src/agent/external/budget/tests.rs`（转发测试 + ScriptedHandler 记录 cleanup）、
  `src/agent/external/handler/tests.rs`（trait 钩子转发测试）、
  `src/facade/external.rs`（`drive_external` 自动 sweep + trace 审计 + rustdoc；
  `default_external_session_handler` rustdoc 清理责任更新；新增 ACP 内存替身测试脚手架
  `SilentTurnLauncher`/`ScriptedThenSilent`/`SharedWriter`/`RecordingWorktreeManager`）、
  `src/facade/agent.rs`（`Agent` drop 语义 rustdoc）、
  `crates/agent-testkit/src/external/{runtime,cassette}.rs`（两 handler 转发）、
  `docs/managed-external-agent.md`（§3 cancel 行 + §11.4 清理责任归属段）、
  `docs/facade-api.md`（§11.2 清理责任条目）、`docs/mag-gaps.md`（A3 第 2、3 条标
  ✅ 已修复（M3-2））。
- **测试**：新增 4 个——`drive_external_cancel_sweeps_live_session_and_worktree`
  （`external-acp`，内存 ACP 替身 handshake 后永久静默：50ms 后 cancel，断言 outcome
  `cleanup_required && !completed`、written 帧含 `session/cancel`、registry
  `live_len()==0` 无残留句柄、ephemeral worktree 以 session disposition 清扫一次、
  sweep disposition 记入 trace）；`cleanup_agent_trait_method_sweeps_through_the_registry`
  （handler 层转发）；`external_budget_cleanup_agent_is_forwarded_to_inner`（charging
  wrapper 不吞清扫）；`external_session_handler_default_cleanup_agent_is_a_no_op`
  （默认 no-op 契约）。worktree 策略回归：registry/worktree 全部既有测试不受影响
  （`external_runtime_registry_cleanup_agent_sweeps_each_prepared_worktree` 等全绿）。
- **门禁**：`cargo fmt --all` ✅；`cargo test --features external-acp -p agent-lib --lib`
  ✅（1090 passed）；`cargo clippy --all-targets -- -D warnings` ✅；
  `cargo clippy --all-targets --features "external-claude-code external-codex
  external-opencode external-acp" -- -D warnings` ✅；`cargo test --all --all-targets`
  ✅（50 套件 0 失败）；`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` ✅。
- Breaking change：无公开 API 形状破坏（`ExternalSessionHandler` 新增带默认实现的
  方法，既有实现者无需改动）。行为变化（有意）：facade external drive 未 committed
  收尾时会 force-close 本 drive 的 live session——此前该 session 泄漏至 registry
  drop；committed drive 语义不变。

### M3-3 [DONE] tool/interaction 批的 cancel 抢占

上下文：

- drive 只在批启动前/完成后查 cancel（`src/facade/agent/stream.rs:244-253, 291-305`）；
  `fulfill_batch` 等批内全部 requirement 完成（`src/agent/drive.rs:794-842`）；
  `ToolRegistryHandler::fulfill` 忽略 ctx（`src/agent/drive/reference.rs:179-189`）。
- 先例：LLM handler 已 cancel-selecting（`reference.rs:132-138`）；abandon 语义齐备
  （`stream.rs:270-279`，never-resume settle；cursor 回 Idle）。
- 后果（今天）：阻塞中的 tool handler（如 mag `ask_user`）冻结整个 run，cancel 不生效。

实现要求：

- 批级等待可被 cancel 抢占：cancel 触发时不再等未完成的 fulfill future，在途
  requirement 按 never-resume settle，turn 以 cancelled 收尾。
- 被丢弃的 tool/interaction future 语义：**drop（detached）** + 文档强制「长工具必须
  select `ToolContext::cancel`」；interaction handler 侧同理（`RunContext.cancellation`
  已在手）。若评估发现 drop 有状态风险（如 handler 内部持有 machine 借用），可选
  「标记 abandon 但后台 join」并说明。
- 非流式路径（`run`/`run_full`）与流式行为一致。
- 更新「批完成后才响应 cancel」的现有测试；`docs/agent-effect-model.md` /
  `docs/agent-layer.md` 的取消延迟口径同步。

验证条件：

- 单元测试：阻塞中的 tool handler（永不返回的 future）在 cancel 后 run 秒级以
  cancelled 收尾；会话可继续下一 run（cursor Idle、committed 历史不变）。
- 单元测试：非流式路径同样行为。
- 回归：正常批完成路径测试不受影响。

完成记录（2026-07-20）：

- **抢占机制**：新增 `fulfill_batch_cancellable`（`src/agent/drive.rs`）——把
  `fulfill_batch` 的批 future 与 `RunContext` 取消令牌做 `tokio::select!`(biased)。两条
  驱动路径共用:`drain`（非流式 `run`/`run_full`）与 `drive_streamed`（流式）都改为经它
  等待批；返回 `BatchOutcome::Completed` 走既有 resume/批后复检路径,
  `BatchOutcome::Preempted` 时对批内**全部** outstanding requirement 按既有 never-resume
  语义 settle（逐一 `NeverResumed` 留痕 + `StepInput::Abandon`),turn 以
  `cancelled() == true` 收尾,cursor 回 `Idle`。批前/批后两个既有观测点保持不变,cancel
  观测点从两个变为三个。
- **drop-vs-join 抉择（选 drop + 有界 unwind 宽限）**：评估发现**纯即时 drop 有真实状态
  风险**——`drive_external` 的 external 会话清扫（M3-2,`cleanup_agent` → adapter
  shutdown + 进程组终止）在 `handler.fulfill` 返回**之后**执行;cancel 触发瞬间若外层
  select 抢先 drop 整个批 future 树,清扫永远不跑,协作取消的常规路径也会泄漏子进程。
  「标记 abandon 但后台 join」不可实现:批 future 借用 scope 栈（`&pending`/`&mut dyn
  Pop`/`&RunContext`),非 `'static`,无法 `tokio::spawn`  detach 后继续 join。因此取
  折中:cancel 命中后继续 poll 在途批一个**有界宽限**(`CANCEL_UNWIND_GRACE = 2s`,
  常量带 rustdoc 说明),合作的 handler（LLM cancel-select、M3-1 的 ACP 读循环、自行
  select 令牌的工具）在窗口内收尾、external 清扫照常执行;宽限到期仍阻塞的 fulfill
  future 被 **drop(detached)**。嵌套层不叠加宽限——同一令牌同时唤醒各层,最深层的
  清扫也在同一个 2s 预算内完成。
- **文档强制约定**：被 detach 的 handler 不得依赖「跑到返回」——长工具必须自行 select
  取消令牌。写入 `ToolHandler` / `InteractionHandler` trait rustdoc（agent 层,
  `RunContext::cancellation`)、`ToolContext::cancel` 字段 rustdoc（facade 工具)、
  `ToolRegistryHandler::fulfill`（解释为何刻意不 select ctx)、`drain` # Cancellation、
  `Agent::run_full_with_cancel` / `AgentRunStream::cancel` rustdoc,以及
  `docs/agent-effect-model.md` §6.3（新增「取消延迟（M3-3)」段)、`docs/agent-layer.md`
  §3 cancel 行（两个观测点→三个)、`docs/facade-api.md` 取消段。
- **文件**:`src/agent/drive.rs`（`fulfill_batch_cancellable` / `BatchOutcome` /
  `CANCEL_UNWIND_GRACE`;`drain` 批等待改抢占式 + rustdoc;`ToolHandler` /
  `InteractionHandler` rustdoc;测试 +3 个夹具 +1 测试)、`src/agent/drive/reference.rs`
  （`ToolRegistryHandler::fulfill` rustdoc)、`src/facade/agent/stream.rs`
  （`drive_streamed` 批等待改抢占式;`cancel()` rustdoc)、`src/facade/agent.rs`
  （`run_full_with_cancel` rustdoc)、`src/facade/tool.rs`（`ToolContext::cancel`
  rustdoc)、`src/facade/agent/tests.rs`（`blocking_weather_tool` + `ToolDropProbe`
  夹具 +2 测试)、`docs/agent-effect-model.md`、`docs/agent-layer.md`、
  `docs/facade-api.md`、`docs/mag-gaps.md`（A4 四条 ✅ 已修复（M3-3）标注）。
- **测试（全部离线）**：
  - `agent::drive::tests::drain_preempts_a_blocked_tool_and_interaction_batch_on_cancel`——
    永不返回的 tool + interaction 同批,cancel 在两者都 in-flight 后触发;断言 30s 上限内
    settle(实际 ~2s 宽限)、`cancelled()`、两个 future 的 `DropProbe` 均触发（detach 语义
    钉死)、无 resume、两条 requirement 均 `NeverResumed` 留痕;随后用合作 handler 再
    drive 一轮正常完成（机器可复用）。
  - `facade::agent::tests::cancelling_a_run_blocked_in_a_tool_detaches_it_and_leaves_agent_runnable`
    （非流式）与 `..._stream_blocked_in_a_tool_...`（流式)——阻塞工具 + cancel,秒级以
    `FacadeError::Agent` 取消诊断收尾,工具 future 被 detach;agent 立即可复用,恢复
    run 只携带自己的 user 消息（committed 历史无残留)。注意:tool phase 的 cancel 按
    既有语义（`CancelDisposition::ResumeTurn`）保留一个闭合了悬空 tool_use 的**一致
    pending turn**,cursor 停 `Idle`,下一次 run 将其作为被取代事务丢弃——因此这两个
    测试**不断言 snapshot 可用**(pending turn 存在时 snapshot 拒绝,这是 M4-4 既有
    语义,LLM phase cancel 的 DiscardTurn 路径不受影响)。
  - 既有测试更新:无——排查确认没有任何测试钉住「批完成后才响应 cancel」的旧延迟
    （既有 cancel 测试全部走合作 handler,宽限内完成,断言不变全绿）;新行为由上述
    三个新测试钉住。正常批完成路径回归:`agent::`(515)、`facade::`(264)全绿。
- **Breaking change（行为收紧）**：无公开 API 形状破坏（仅新增 `pub(crate)` 项与
  rustdoc)。行为变化:cancel 不再等阻塞中的 tool/interaction 批自行返回——阻塞
  handler 的 fulfill future 现在会在 cancel + 2s 宽限后被 **drop**,而不是一直 awaited。
  依赖「工具一定跑到返回」做收尾（释放锁、关闭句柄、落盘）的宿主 handler 必须改为
  select `ToolContext::cancel` / `RunContext::cancellation`,或在 `Drop` 中收尾;cancel
  延迟从「最坏 = 批内最慢 handler 的返回时间」收紧为「最坏 ≈ 2s 宽限」。
- 验证通过:`cargo fmt --all`;`cargo test -p agent-lib --lib agent::`(515 passed);
  `cargo test -p agent-lib --lib facade::`(264 passed,+2);
  `cargo clippy --all-targets -- -D warnings`;`cargo clippy --all-targets --features
  "external-claude-code external-codex external-opencode external-acp" -- -D warnings`;
  `cargo test --all --all-targets`(50 套件全绿);`RUSTDOCFLAGS="-D warnings" cargo doc
  --no-deps --workspace`。

### M3-R [TODO] M3 review：cancel 强化收口 + 全计划终检

- 逐条核对 `docs/mag-gaps.md` A3/A4 落地状态并标注；C 组确认保持「不做」。
- 核对 mag 验收线索：cancel 一个静默 ACP 子进程秒级返回、无子进程泄漏；阻塞 tool 不再
  冻结 run。
- 取消语义三处（read loop、session 清理、批抢占）的文档口径一致。
- M1 review（2026-07-20）遗留项逐条评估（登记于 `docs/mag-gaps.md` C7–C9）：委派的审批
  绕过 tap/recorder（无 `RunEvent::ApprovalRequested`、不进 `RunOutput.events`）、
  SingleTool 委派模式 external start 双重 gate、child auto-deny 层可被父 handler 改判
  Approve 的语义文档化、M1 测试缺口（external cancel-while-parked、M1-4 family-mismatch、
  Claude Code 路径覆盖）——逐条决定收口或保持登记，结论写入完成记录。
- 全量门禁通过（含 external features 的 clippy 与测试）。
- 终检：PLAN.md 四个目标逐项核对；本计划与任务单归档到
  `docs/archive/<完成日期>-mag-gaps/`。
