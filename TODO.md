# TODO：Refine 修正任务单

本任务单对应 [PLAN.md](PLAN.md) 和 [docs/refine.md](docs/refine.md)。旧任务单已归档到 [docs/archive/2026-07-18-facade-api/TODO.md](docs/archive/2026-07-18-facade-api/TODO.md)。

执行规则：

- 严格按编号顺序实现，除非当前任务明确要求先补充前置信息。
- 每个标题中的 `[TODO]` 表示尚未完成。完成后把 `[TODO]` 改成 `[DONE]`，并在任务下方记录关键实现和验证结果。
- 不要跳过每个 milestone 末尾的 review 任务。
- 修改行为时同步修改拥有该行为的文档，至少检查 `README.md`、`docs/facade-api.md`、`docs/managed-external-agent.md`、`docs/capability-matrix.md` 和 `docs/refine.md` 中是否需要更新。
- 默认测试必须离线可跑，不依赖真实 provider、真实 CLI login、网络或用户本机配置。

## M1：流式生命周期恢复

### M1-1 [DONE] 修复 `ChatSession::stream` 提前 drop 后遗留 pending turn

上下文：

- `ChatSession::stream` 在返回 `RunStream` 前已经打开 pending turn，入口在 `src/facade/chat.rs`。
- `RunStream` 的错误路径已有 rollback 逻辑，实现在 `src/facade/chat/stream.rs`，但结构体没有 `Drop`，调用方提前丢弃 stream 时不会回滚。
- 直接后果是后续 `send`、`stream` 或 `snapshot` 可能遇到仍未完成的 pending turn。

实现要求：

- 在 `src/facade/chat/stream.rs` 中为 `RunStream` 增加 drop-time cleanup。未完成状态被 drop 时必须回滚当前 pending turn。
- 把现有错误路径 rollback 和 drop rollback 收敛到同一个小 helper，避免两条路径行为分叉。
- 正常完成的 stream 必须标记为 terminal，drop 时不能再次 rollback。
- 发生流式错误时，现有错误语义保持不变，同时不能因为随后 drop 再次改变 conversation。
- 确认 `ChatSession::snapshot`、`ChatSession::send` 和下一次 `ChatSession::stream` 在提前 drop 后都能继续工作。

验证条件：

- 增加单元测试覆盖：stream 创建后未 poll 就 drop，随后 `send` 成功。
- 增加单元测试覆盖：stream 读到至少一个 delta 后 drop，随后 `snapshot` 成功，下一次 `send` 不包含未提交的半截 assistant turn。
- 增加单元测试覆盖：stream 正常读完后 drop，不回滚已经提交的 assistant turn。
- 运行：

```bash
cargo test -p agent-lib --lib facade::chat::
```

完成记录：

- 实现（`src/facade/chat/stream.rs`）：
  - 把原来的 `rollback()` 收敛为单一幂等 helper `abandon()`：仅当 `state != Done`
    时调用 `conversation.cancel_pending(CancelDisposition::DiscardTurn)` 并把 `state`
    置为 `Done`。错误路径（`absorb`/`finish`/流式传输错误）与 drop 路径都走这一 helper，
    避免行为分叉。
  - 新增 `impl Drop for RunStream`，drop 时调用 `abandon()`：未完成状态回滚 pending
    turn；正常完成（已提交 `Done`）或已出错状态因 `state == Done` 而是 no-op，不会二次
    回滚已提交的 assistant turn。
  - 更新 `RunStream`/`ChatSession::stream` 文档以匹配“提前 drop 自动 discard 半截
    turn，session 回到上一 committed 一致点”的承诺（此前文档已这样描述，但实现缺 `Drop`）。
- 测试（`src/facade/chat/tests.rs`）：新增 `DualFakeClient`（同时脚本化 `chat` 与
  `chat_stream`）与三条离线回归：
  - `stream_dropped_before_polling_leaves_session_usable`：未 poll 就 drop，随后 `send` 成功。
  - `stream_dropped_after_delta_does_not_commit_partial_turn`：收到 text delta 后 drop，
    `snapshot` 成功且未提交半截 assistant turn，下一次 `send` 只带 1 条消息。
  - `stream_dropped_after_completion_keeps_committed_turn`：正常读完 `Done` 后 drop，
    已提交的 [user, assistant] 保留，后续 `send` 回放 3 条消息。
- 文档：同步 `docs/refine.md` 问题 #1 的修复状态（ChatSession 侧 M1-1 已修，
  `AgentRunStream` 留待 M1-2）。
- 验证：`cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`（clean）、
  `cargo test -p agent-lib --lib facade::chat::`（19 passed）、
  `cargo test --all --all-targets`（全绿，0 failed）、
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`（clean）。

### M1-2 [DONE] 修复 `AgentRunStream` 提前 drop 后遗留未完成 run

上下文：

- `Agent::stream` 的 facade 入口在 `src/facade/agent.rs`。
- `AgentRunStream` 和流式驱动逻辑在 `src/facade/agent/stream.rs`。
- 当前实现把 `drain(machine, ...)` 包进 opaque future，结构体本身没有 drop-time abandon 或 rollback 能力。
- 风险场景包括：模型还在产出、等待工具结果、等待审批、或 run 已打开 pending conversation 但 stream 被调用方丢弃。

实现要求：

- 重构 `AgentRunStream` 的内部状态，让 drop 路径能识别 run 是否已经 terminal。
- 未完成状态被 drop 时，必须让 agent 回到下一次 `run` 或 `stream` 可继续执行的状态。
- 如果当前 `AgentMachine` 已经暴露 pending requirement，drop 路径应使用现有 sans-io 输入执行 abandon 或等价清理，而不是直接篡改底层 conversation。
- 如果没有 pending requirement 但 conversation 已经处于 pending turn，也必须通过 machine 或 facade 提供的受控路径清理。
- 正常完成、错误完成和显式 close 后的 drop 都必须是幂等的。
- 如果需要新增 `AgentRunStream::close` 或类似 API，只能作为显式收尾能力；drop 本身仍必须保证不会留下不可继续运行的 agent。
- 不改变默认非流式 `Agent::run`、`Agent::run_full` 的外部行为。

验证条件：

- 增加测试覆盖：stream 创建后未 poll 就 drop，随后同一个 `Agent` 可以成功 `run`。
- 增加测试覆盖：stream 读到部分事件后 drop，随后同一个 `Agent` 可以成功 `run`，且前一次半成品 run 没有进入 committed history。
- 增加测试覆盖：stream 等待审批时 drop，随后同一个 `Agent` 可以成功 `run`。
- 如果存在工具等待场景的 testkit 支持，增加测试覆盖：stream 等待工具结果时 drop，随后同一个 `Agent` 可以成功 `run`。
- 运行：

```bash
cargo test -p agent-lib --lib facade::agent::stream
cargo test -p agent-lib --lib facade::agent::
```

完成记录：

- 实现（`src/facade/agent/stream.rs`）：
  - 新增共享 machine 句柄 `type MachineCell<'a> = Rc<RefCell<&'a mut DefaultAgentMachine>>`：
    drive future 与 `AgentRunStream` 各持一份 `Rc` clone，使 drop 路径不再把
    `&mut agent.machine` 埋进 opaque future，而能同步触达 machine。
  - 把原来 `drain(machine, ...)` 的驱动逻辑复刻为 `async fn drive_streamed(...)`：
    与 `drain` 的循环逐字段等价（`fulfill_batch` → 按 resolution `Resume` → 记录
    trace → 直到 terminal cursor），但只在同步 `step` 前后借用 `MachineCell`，每次
    `await` 前释放借用，因此 park 时不持有任何 `RefCell` 借用，drop 的
    `try_borrow_mut` 必成功。三个 `start*` 入口改为构造并保存 `MachineCell`。
  - `AgentRunStream` 结构体新增 `machine: MachineCell<'a>` 字段与幂等
    `abandon(&mut self)`：仅当 `state != Done` 时，`try_borrow_mut` machine 后取
    `cursor().pending_requirement_ids()` 的首个 id 喂 `StepInput::Abandon(id)`——
    这是 machine 现有的 sans-io never-resume 输入，不直接篡改底层 conversation。
    LLM 步（`StreamingStep`）丢弃 pending turn；tool / approval 阶段
    （`AwaitingTool` / `AwaitingApproval`）对未决调用折叠 `Cancelled` 结果；两者都
    把 cursor 归位到可继续的 `Idle`。
  - 新增 `impl Drop for AgentRunStream` 调用 `abandon()`：正常读完 `Done`、错误
    完成、或已 abandon（`state == Done`）时为 no-op，不回滚已提交 turn；非流式
    `Agent::run` / `run_full` 外部行为不变。
  - 更新 `AgentRunStream` / `Agent::stream` 文档以匹配“提前 drop 自动 abandon 在途
    turn，agent 回到下一次 `run`/`stream` 可继续的一致点”。
- 支撑改动（`src/agent/drive.rs`）：把 `fulfill_batch`、`Resolved`（含
  `resolution` / `resolved_at_scope` 两字段）、`record_requirement`、
  `record_requirement_resolution`、`is_terminal` 提升为 `pub(crate)`，供
  `drive_streamed` 复用，保证与 `run_full` 的逐字段等价。
- 测试（`src/facade/agent/tests.rs`）：新增 `DropTestClient`（同时脚本化 `chat`
  恢复回合与 `chat_stream` 流式回合，并记录每次 `chat` 请求的消息条数）、
  `partial_text_head`、`ParkingInteractionHandler`、`parking_weather_tool`，及四条
  离线回归：
  - `dropping_never_polled_stream_leaves_agent_runnable`：未 poll 就 drop，随后同一
    `Agent` `run` 成功，恢复回合仅 1 条消息。
  - `dropping_partially_streamed_run_discards_it_and_leaves_agent_runnable`：收到部分
    text delta 后 drop（drive park 在 LLM fold），随后 `run` 成功且半成品 turn 未进入
    committed history（恢复回合仅 1 条消息）。
  - `dropping_approval_gated_stream_leaves_agent_runnable`：等待审批时 drop，随后
    `run` 成功，被 gate 的工具未执行，无残留。
  - `dropping_tool_awaiting_stream_leaves_agent_runnable`：等待工具结果时 drop（park 在
    永不返回的工具里），随后 `run` 成功，无残留。
- 文档：同步 `docs/refine.md` 问题 #1 的修复状态（`AgentRunStream` 侧 M1-2 已修）。
- 验证：`cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`（clean）、
  `cargo clippy --all-targets --features "external-claude-code external-codex external-opencode" -- -D warnings`（clean）、
  `cargo test -p agent-lib --lib facade::agent::`（30 passed，含 4 条新回归）、
  `cargo test --all --all-targets`（全绿，841 lib + 集成全部通过，0 failed）、
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`（clean）。

### M1-3 [DONE] Review：流式生命周期恢复

检查范围：

- `ChatSession::stream`、`Agent::stream`、`RunStream`、`AgentRunStream` 的正常完成、错误完成和提前 drop 路径。
- 是否还有其他 facade stream 类型打开 pending state 但没有 drop cleanup。
- 新增测试是否只依赖 fake client、scripted handler 或 testkit。
- 文档是否需要说明 drop/close 行为。

验证条件：

- 运行：

```bash
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test -p agent-lib --lib facade::chat::
cargo test -p agent-lib --lib facade::agent::
```

- 手工复核 `docs/refine.md` 中 “stream 提前 drop 遗留 pending turn” 条目的状态，必要时补充当前修复说明。

完成记录：

- 复核 `ChatSession::stream` / `RunStream`（`src/facade/chat.rs`、
  `src/facade/chat/stream.rs`）：正常完成经 `State::Finishing` → `finish()` 提交后置
  `State::Done`；错误路径（`absorb` 累加错误、流式传输错误、`finish` 失败、tool-use
  拒绝）与 `Drop` 都收敛到同一幂等 `abandon()`（`state != Done` 时
  `cancel_pending(DiscardTurn)` + 置 `Done`）。已提交 / 已出错的 stream 因 `state == Done`
  在 drop 时为 no-op，不回滚已提交 turn。逻辑正确、无分叉。
- 复核 `Agent::stream` / `AgentRunStream`（`src/facade/agent.rs`、
  `src/facade/agent/stream.rs`）：drive future 与 `Drop` 共享
  `Rc<RefCell<&mut DefaultAgentMachine>>`，`drive_streamed` 仅在同步 `step` 前后借用、
  每次 `await` 前释放，故 park 时 drop 的 `try_borrow_mut` 必成功。`abandon()` 仅在
  `state != Done` 且 cursor 存在 outstanding requirement 时喂 `StepInput::Abandon(首个 id)`，
  LLM 步丢弃 pending turn、tool/approval 阶段折叠 `Cancelled`，均归位到可继续的 `Idle`；
  正常 / 错误 / 已 abandon 状态均为 no-op。rules-routed、dispatcher-routed 起步路径不 step
  machine（cursor 恒 `Idle`），drop 找不到在途 turn，仅保持结构形状统一——已核对无遗漏。
- 其他 facade stream 类型排查：全仓仅 `RunStream` 与 `AgentRunStream` 两个 facade 层
  stream 会打开 conversation / machine 的 pending state，二者均已有 `Drop`。adapter /
  client 层的 `chat_stream` 只是纯 wire 事件流，不打开 facade pending state，无需 cleanup。
- 测试离线性核对：chat 侧新增回归依赖 `DualFakeClient`（脚本化 `chat` + `chat_stream`）；
  agent 侧依赖 `DropTestClient`（可 `park_stream`）、`ParkingInteractionHandler`、
  `parking_weather_tool`，全部为 fake client / scripted handler，无真实 provider、CLI、
  网络或本机配置依赖。
- 文档核对：`RunStream`、`AgentRunStream`、`ChatSession::stream`、`Agent::stream` 的
  doc 均已明确“提前 drop 自动 discard / abandon 在途 turn，session/agent 回到上一
  committed 一致点”。`docs/refine.md` 问题 #1 的“修复状态（更新）”已覆盖 M1-1 与 M1-2，
  R-1 草案与优先级一致，无需再补写。
- 验证：`cargo fmt --all`（无源码改动）、`cargo clippy --all-targets -- -D warnings`（clean）、
  `cargo test -p agent-lib --lib facade::chat::`（19 passed）、
  `cargo test -p agent-lib --lib facade::agent::`（30 passed，含 4 条 drop 回归）。
  本次仅改动 TODO.md / 计划文档，未改动编译产物，复用 M1-2 的全量绿测结果，未重跑
  `cargo test --all --all-targets`。

## M2：非流式事件一致性

### M2-1 [DONE] 在 `Agent::run_full` 中记录 `ApprovalRequested` 事件

上下文：

- `Agent::run_full` 的主体在 `src/facade/agent.rs`。
- 当前 `RunOutput.events` 来自 `collect_traces(done.notifications(), &recorder)`，只覆盖 tool started、tool finished、delegation 等 notification。
- 流式路径通过 `TapInteractionHandler` 在 `src/facade/agent/stream.rs` 中发出 `RunEvent::ApprovalRequested`。
- 非流式路径缺少对应 recorder，导致调用方无法从 `RunOutput.events` 观察审批请求。

实现要求：

- 为非流式路径增加审批事件 recorder。可以复用流式路径中的事件构造逻辑，也可以抽取共享 helper，避免 `FacadeApproval` 字段映射重复。
- 在 `Agent::run_full` 中包装当前 interaction handler，使任何审批请求在传给真实 handler 前或同时被记录。
- 保持现有 interaction handler 优先级：调用方注入的 handler 仍然决定 approve、deny 或 fallback 行为。
- `RunEvent::ApprovalRequested` 必须包含 call id、tool name、reason、policy action、input 摘要等现有流式事件中可见的字段。
- 审批被拒绝、headless fallback、或审批后工具未真正启动时，也必须保留审批请求事件。
- 不要把 secret 或完整大输入无控制地塞进事件；沿用现有 facade approval 的 redaction 和 preview 策略。

验证条件：

- 增加测试覆盖：`Agent::run_full` 触发 ask approval 并 approve，`RunOutput.events` 中出现 `ApprovalRequested`，随后出现对应 tool lifecycle 事件。
- 增加测试覆盖：调用方注入 interaction handler 并 deny，`RunOutput.events` 仍出现 `ApprovalRequested`，错误或输出行为与现有语义一致。
- 增加测试覆盖：headless fallback 或无 handler 场景仍记录审批请求。
- 运行：

```bash
cargo test -p agent-lib --lib facade::agent::
```

完成记录：

- 抽取共享 helper `enriched_approval_request(approval, call_id, requirement)`
  到 `src/facade/approval.rs`：peek `FacadeApproval::pending_request`（不消费，
  fallback handler 仍可 remove）取 tool name + 脱敏 input 摘要，再用机器携带的
  interaction 重绑 `call_id` 与 `reason`。流式路径 `TapInteractionHandler`
  （`src/facade/agent/stream.rs`）改为复用该 helper，消除 `FacadeApproval`
  字段映射重复。
- 非流式路径新增 `RecordingInteractionHandler`（`src/facade/agent.rs`）：包裹
  `interaction_handler()` 解析出的 handler（注入的 handler 或 `FacadeApproval`
  fallback），在 approval interaction 传给真实 handler *之前* 按 fulfill 顺序把
  `ApprovalRequest` 记录进 `Arc<Mutex<Vec<..>>>`。仅观察不决策，approve / deny /
  fallback 优先级完全不变。
- 新增 `weave_approval_events(events, approvals)`：把记录的审批按 `call_id`
  编织进 `collect_traces` 产出的事件流——审批落在其 gated 调用的首个
  `ToolStarted`/`ToolFinished`（approved 走 `ToolStarted`，denied 只有
  `ToolFinished`）之前；无任何工具事件的审批（headless deny 等）按记录顺序在下一
  个锚点前或队尾 flush，保证每个暂停审批可见。流式路径不受影响（审批实时 emit，
  `collect_traces` 仍不产审批事件）。
- `run_full` 主 supervisor drive 装配 recorder 并包裹 `scope.interaction`，
  drain 后 `events: weave_approval_events(collected.events, recorded_approvals)`。
- 新增 3 条离线回归（`src/facade/agent/tests.rs`）：`ask` approve 经 fallback →
  `ApprovalRequested` 先于 `ToolStarted`/`ToolFinished` 且带 call id / reason /
  脱敏 input；注入 handler deny → 仍记录 `ApprovalRequested` 且先于 denied
  `ToolFinished`、无 `ToolStarted`；headless `ask`（`ApprovalPolicy::ask_tool`
  无 handler）→ 仍记录 `ApprovalRequested`。
- 验证：`cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`（clean）、
  `cargo test -p agent-lib --lib facade::agent::`（33 passed，含 3 条新回归）、
  `cargo test -p agent-lib --lib`（844 passed）、
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`（clean）。

### M2-2 [DONE] 对齐非流式和流式事件契约文档与回归测试

上下文：

- 流式路径天然可以产生 token 级 `TextDelta`。
- 非流式 `run_full` 不应该伪造 token delta，但必须记录 approval、tool、delegation 等结构化生命周期事件。
- 事件类型定义和输出结构位于 `src/facade/run.rs` 及相关 facade 模块。

实现要求：

- 增加一组对比测试：同一个 scripted 场景分别通过 `Agent::stream` 和 `Agent::run_full` 执行，比较 approval、tool、delegation 事件的规范化序列。
- 文档说明非流式和流式路径的事件一致性边界：生命周期事件一致，token delta 只属于流式路径。
- 检查 `README.md` 和 `docs/facade-api.md` 中对 `RunOutput.events` 的描述是否需要更新。
- 如果新增 helper 或 recorder 类型，补充 rustdoc，说明它是 facade 内部的事件采集机制。

验证条件：

- 对比测试必须稳定，不依赖真实 provider。
- 运行：

```bash
cargo test -p agent-lib --lib facade::agent::
cargo test -p agent-lib --lib facade::run
```

- 运行：

```bash
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
```

完成记录：

- 发现并修复一处真实的事件契约分歧（M2-2 的核心）：非流式 `collect_traces`
  （`src/facade/agent.rs`）对**被拒工具**的 `Notification::ToolCallFinished`
  （无对应 `ToolCallStarted`，name 查不到）也会投出一个 name 为空的幽灵
  `ToolFinished`，而流式路径（`TapToolHandler` 仅在工具真正执行时 emit）不产任何
  tool 生命周期事件。对齐决策：被拒工具从未执行 → 两条路径都只保留
  `ApprovalRequested`，不产 `ToolStarted`/`ToolFinished`。修正 `collect_traces` 在
  `names` 无该 call id（且非 delegation）时跳过 `ToolFinished`；`weave_approval_events`
  的尾部 flush 已保证被拒审批仍可见。同步更新 `collect_traces` /
  `weave_approval_events` / `tool_event_call_id` 的 rustdoc（删除"denied → ToolFinished"
  旧说法），并收紧 M2-1 denied 回归 `run_full_records_approval_when_injected_handler_denies`
  为断言"被拒工具既无 `ToolStarted` 也无 `ToolFinished`"。
- 新增 parity 回归（`src/facade/agent/tests.rs`）：`lifecycle_signature` /
  `canonical_lifecycle_event` helper 把一次 run 的事件归一化为可比较的生命周期子序列
  （丢弃流式独有的 `TextDelta`、终态 `Done` 与 raw 逃生舱）。四条对比测试对同一 scripted
  场景分别走 `run_full` 与 `stream` 并断言归一化序列**逐项相等**：
  - `stream_and_run_full_agree_on_plain_tool_lifecycle`（auto_allow）；
  - `stream_and_run_full_agree_on_approved_tool_lifecycle`（ask approve，含富化审批的
    tool/call_id/reason/脱敏 input 全字段一致）；
  - `stream_and_run_full_agree_on_denied_tool_lifecycle`（auto_deny，验证对齐后两路都只
    剩 `ApprovalRequested`）；
  - `stream_and_run_full_agree_on_delegation_lifecycle`（新增 dual-mode `MarkerRoutingClient`：
    流式 supervisor + 非流式 child，断言两路 `DelegationStarted`/`DelegationFinished` 一致）。
  每条测试均额外断言 token-delta 边界：`stream` 含 `TextDelta`，`run_full` 绝不含。
- 文档：`docs/facade-api.md` 新增 §6.2.1「事件一致性边界」明确生命周期事件一致、token
  delta 只属流式、`Done` 只由 stream yield、两路共享 `collect_traces` +
  `weave_approval_events` / `TapToolHandler` + `TapInteractionHandler` 采集机制；
  `README.md` §3 补一句同义说明；`src/facade/run.rs` 的 `RunEvent` 枚举、`TextDelta`
  变体、`RunOutput::events` 字段 rustdoc 补充边界说明。
- 验证：`cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`（clean）、
  `cargo test -p agent-lib --lib facade::agent::`（37 passed，含 4 条新 parity）、
  `cargo test -p agent-lib --lib facade::run`（9 passed）、
  `cargo test -p agent-lib --lib`（848 passed）、`cargo test --all --all-targets`（全绿，
  0 failed）、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`（clean）。

### M2-3 [DONE] Review：非流式事件一致性

检查范围：

- `run_full`、`run`、`stream` 的事件语义是否清楚。
- approval 被 approve、deny、fallback 的路径是否都有事件。
- 文档是否明确非流式不产生 token delta。
- 新增 recorder 是否不会改变真实 handler 的执行顺序。

验证条件：

- 运行：

```bash
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test -p agent-lib --lib facade::agent::
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
```

- 手工复核 `docs/refine.md` 中 “非流式 RunOutput.events 缺少审批请求” 条目的状态，必要时补充当前修复说明。

完成记录：

- 代码复核（无需改动，行为符合规范）：
  - 事件语义清晰：`src/facade/run.rs` 的 `RunEvent` 枚举与 `RunOutput::events`
    rustdoc 明确「生命周期变体两路一致；`TextDelta` / `Done` 只属流式」；非流式
    `run_full`（`src/facade/agent.rs`）经 `collect_traces` + `weave_approval_events`
    产出生命周期事件，`run` 返回精简 `Reply`（不承诺事件面），`stream` 通过
    `TapToolHandler` / `TapInteractionHandler` 实时 emit。三者语义与文档一致。
  - approve / deny / fallback 三条路径均有审批事件：`RecordingInteractionHandler`
    包裹 `interaction_handler()` 解析出的真实 handler（注入 handler 或 `FacadeApproval`
    fallback），在委派前记录 `ApprovalRequest`；`weave_approval_events` 对 approved
    审批锚定在其 `ToolStarted` 前，对 denied / headless（无工具锚点）在尾部或下一锚点前
    flush，保证每个暂停审批均可见。
  - 文档明确非流式不产 token delta：`run.rs`、`docs/facade-api.md` §6.2.1、`README.md`
    §3 均写明 `TextDelta` 为流式独有；4 条 parity 回归额外断言 `run_full` 绝不含
    `TextDelta`。
  - recorder 不改变执行顺序：`RecordingInteractionHandler::fulfill` 先记录再
    `self.inner.fulfill(...).await`，**仅观察不决策**，approve / deny / fallback
    优先级完全由真实 handler 决定；富化字段与流式共用 `enriched_approval_request`
    helper（`src/facade/approval.rs`，peek 不消费 pending map）。
- 文档复核并补充：`docs/refine.md` §3「非流式 `Agent::run_full` 的 `RunOutput.events`
  不包含审批请求」补入「修复状态（更新）」块，记录 M2-1（run-scoped recorder +
  weave）、M2-2（对齐被拒工具幽灵 `ToolFinished` + parity 回归 + 文档边界）与 M2-3
  复核结论，标注该问题已解决（与 §1 的 M1 修复状态注记格式一致）。
- 验证：`cargo fmt --all`（clean）、`cargo clippy --all-targets -- -D warnings`（clean）、
  `cargo test -p agent-lib --lib facade::agent::`（37 passed，含 4 条 parity 回归）、
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`（clean）。本任务仅改动
  `docs/refine.md`（文档），代码自 M2-2 全量 `cargo test --all --all-targets` 全绿以来
  未变更，故复用该绿测结果、未重跑全量套件。

## M3：协作状态 snapshot 和 restore

### M3-1 [TODO] 为 mailbox、blackboard、plan 补齐 data-only snapshot API

上下文：

- `Mailbox` 在 `src/agent/collab/mailbox.rs`，内部状态包含 `next_seq` 和按 recipient 组织的 inbox。
- `Blackboard` 在 `src/agent/collab/blackboard.rs`，已有按 channel 读取 snapshot 的能力，但缺少一次性保存和精确恢复全部 channel 的 API。
- `Plan` 在 `src/agent/collab/plan.rs`，已有或接近已有可序列化 snapshot，需要确认是否能被 facade restore 直接使用。
- `src/facade/agent/snapshot.rs` 中已有 `MailboxSnapshot`、`BlackboardSnapshot`、`PlanSnapshot` 字段概念，但当前 capture 写入的是空值。

实现要求：

- 为 mailbox 增加完整 snapshot 和 restore API，保留 message seq 的单调性，恢复后发送新消息不能复用旧 seq。
- 为 blackboard 增加完整 snapshot 和 restore API，保留 board id、channel 列表、每个 channel 的消息顺序和 offset 语义。
- 确认 plan 的 snapshot 能覆盖当前 plan state；如果不能，补齐缺失字段。
- snapshot 类型必须是 data-only，支持 serde，不暴露内部锁或运行时句柄。
- 旧 snapshot 兼容性要通过 `#[serde(default)]` 或等价方式保证。

验证条件：

- 增加 mailbox round-trip 测试：发送多 recipient 消息，snapshot 后 restore，`read_from` 返回一致内容，新发送消息 seq 继续递增。
- 增加 blackboard round-trip 测试：多个 channel、多条消息，snapshot 后 restore，channel 列表和每个 channel snapshot 一致。
- 增加 plan round-trip 测试或确认已有测试覆盖；如果已有测试覆盖，在任务完成记录中写明测试名。
- 运行：

```bash
cargo test -p agent-lib --lib agent::collab
```

### M3-2 [TODO] 让 `AgentSnapshot::capture` 保存 live 协作内容

上下文：

- `AgentSnapshot::capture` 位于 `src/facade/agent/snapshot.rs`。
- 当前实现把 `mailbox`、`blackboard`、`plan` 写成空值或默认值，只保存了 topology。
- `Agent` 持有的协作底座和配置在 `src/facade/agent.rs`、`src/facade/collab.rs` 中。

实现要求：

- 修改 `AgentSnapshot::capture` 的调用链，让 capture 能访问 live `CollabState`。
- 当 mailbox、blackboard、plan 已启用时，写入对应 data-only snapshot。
- 当某个协作组件未启用时，snapshot 字段保持 `None` 或明确的空状态，restore 时不能意外启用未配置组件。
- 保持旧 snapshot 可读。旧 snapshot 没有协作内容时，restore 应按 topology 创建空协作底座。
- 不改变普通 conversation snapshot、delegate snapshot 和 retained external session snapshot 的现有语义。

验证条件：

- 增加 facade snapshot 测试：agent 运行或手工写入 mailbox 后 snapshot，restore 后 mailbox 内容仍可读。
- 增加 facade snapshot 测试：blackboard 多 channel 内容在 restore 后保留。
- 增加 facade snapshot 测试：未启用协作组件时 snapshot 和 restore 不创建额外组件。
- 运行：

```bash
cargo test -p agent-lib --lib facade::agent::snapshot
cargo test -p agent-lib --lib facade::collab
```

### M3-3 [TODO] 让 restore 优先使用 snapshot 中的协作内容

上下文：

- restore builder 在 `src/facade/agent/snapshot.rs`。
- 当前 restore 会根据 topology 重新 provision 空 mailbox、blackboard、plan。
- 修复 capture 后，如果 restore 仍忽略内容，round-trip 仍然不完整。

实现要求：

- 更新 `AgentRestoreBuilder::build` 或相关 helper：当 snapshot 中存在 mailbox、blackboard、plan 内容时，优先从 snapshot 恢复。
- 当 snapshot 中缺失内容但 topology 要求启用组件时，才创建空组件。
- 当 snapshot 内容和 topology 明显冲突时，选择一个可解释策略并写入文档。推荐策略是 snapshot 内容为准，topology 只作为兼容旧 snapshot 的 provision hint。
- 确认恢复后的 agent 能继续执行协作相关工具或 delegate workflow。

验证条件：

- 增加 round-trip 测试：snapshot 前已有 mailbox seq，restore 后继续发送消息，seq 不冲突。
- 增加 round-trip 测试：snapshot 前已有 blackboard channel，restore 后追加消息，旧消息仍在且新 offset 正确。
- 增加兼容性测试：构造没有协作内容字段的旧格式 snapshot，restore 成功并得到空协作底座。
- 运行：

```bash
cargo test -p agent-lib --lib facade::agent::snapshot
```

### M3-4 [TODO] 明确并实现顶层 artifact snapshot 策略

上下文：

- `AgentSnapshot` 顶层存在 `artifacts` 字段，但当前 capture 写入空数组。
- external delegate 或 retained session 可能已经有自己的 artifact snapshot。
- 如果顶层字段长期为空但文档暗示可用，会误导调用方。

实现要求：

- 在 `src/facade/agent/snapshot.rs` 中明确顶层 `artifacts` 的语义。
- 二选一实现：
  - 保存聚合后的 facade-level artifact view，并保证 restore 后可查询。
  - 或将字段标记为保留兼容字段，并在文档中说明 artifact 当前由 external session snapshot 持有，顶层字段不作为行为来源。
- 推荐优先选择最小可维护策略。如果没有稳定 artifact store，就不要伪造聚合语义。
- 更新 `docs/refine.md`、`docs/facade-api.md` 或 `docs/managed-external-agent.md` 中相关说明。

验证条件：

- 若实现聚合保存：增加 snapshot round-trip 测试，验证 artifact view 恢复一致。
- 若选择保留字段策略：增加序列化兼容测试，验证空字段存在且 restore 不依赖它。
- 文档必须明确调用方应从哪里读取 restored artifacts。
- 运行：

```bash
cargo test -p agent-lib --lib facade::agent::snapshot
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
```

### M3-5 [TODO] Review：协作状态 snapshot 和 restore

检查范围：

- mailbox、blackboard、plan 的 snapshot 类型是否 data-only、可 serde、可兼容旧格式。
- `AgentSnapshot::capture` 是否真的读取 live 状态，而不是 topology 默认值。
- `AgentRestoreBuilder` 是否优先使用 snapshot 内容。
- artifact 策略是否在代码和文档中一致。
- retained external session snapshot 是否没有被本阶段改坏。

验证条件：

- 运行：

```bash
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test -p agent-lib --lib agent::collab
cargo test -p agent-lib --lib facade::agent::snapshot
cargo test -p agent-lib --lib facade::collab
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
```

- 手工复核 `docs/refine.md` 中 “协作状态 snapshot/restore 缺失” 条目的状态，必要时补充当前修复说明。

## M4：managed external 可用性和 capability 来源

### M4-1 [TODO] 提供可直接装配默认 session handler 的 builder API

上下文：

- `ManagedExternalAgent` 和 builder 在 `src/facade/external.rs`。
- 当前 README quick start 展示 `ManagedExternalAgent::builder(...).build()`，但 managed external run 需要 session handler，否则 `drive_external` 会返回缺少 handler 的错误。
- 现有 `default_external_session_handler` 需要先拿到已构造的 `ManagedExternalAgent`，再回填 builder，调用体验绕。

实现要求：

- 在 `ManagedExternalAgentBuilder` 上增加清晰 API，用于构造 agent 并自动装配默认 session handler。
- API 名称应直接表达语义，例如 `build_with_default_session_handler` 或等价命名。
- 默认 feature 下不能引入 CLI adapter 依赖；未启用相关 feature 时，错误行为应与现有 default handler 保持一致。
- 启用对应 external feature 时，该 API 应执行现有 probe 和 registry-backed handler 装配。
- 保持现有手工 `.session_handler(...)` 路径可用，不能破坏调用方自定义 handler。

验证条件：

- 增加测试覆盖：默认 feature 下调用新 API 得到明确错误，且错误不包含 secret。
- 增加测试覆盖：手工 `.session_handler(...)` 路径仍可 build。
- 如果可以用 fake registry 或 fake handler 覆盖启用 feature 路径，增加测试；否则在任务记录中说明为什么只能通过 feature clippy 验证。
- 运行：

```bash
cargo test -p agent-lib --lib facade::external
cargo clippy --all-targets -- -D warnings
```

### M4-2 [TODO] 修正 README managed external quick start

上下文：

- `README.md` 的 external quick start 当前示例容易构造出没有 session handler 的 `ManagedExternalAgent`。
- `docs/managed-external-agent.md` 是 managed external 设计说明来源之一。

实现要求：

- 更新 `README.md` 示例，使用 M4-1 中新增的默认 handler builder API。
- 在示例旁说明：默认 crate build 不启用 CLI adapter；运行 managed external 示例需要对应 feature 和本机 CLI login。
- 更新 `docs/managed-external-agent.md` 中的 quick start 或构造说明，确保它和 README 一致。
- 检查 `examples/` 中 managed examples 是否仍是推荐路径；如有重复描述，保持术语一致。

验证条件：

- 运行 README 中示例对应的编译检查。如果示例不是 doc test，至少运行：

```bash
cargo check --examples
cargo clippy --all-targets \
  --features "external-claude-code external-codex external-opencode external-acp" -- -D warnings
```

- 文档中不能出现“build 后即可 run external agent”但没有 handler 装配的示例。

### M4-3 [TODO] 为 external capability 增加来源模型

上下文：

- `ExternalAgentCapabilities` 和 preset builder 在 `src/facade/external.rs`。
- 当前 preset 构造时使用 `declared_capabilities`，builder 校验和 `Agent` 后续能力判断也读取这个 capability view。
- declared capability 是静态声明，不等同于真实 CLI probe 或 ACP negotiation 结果。

实现要求：

- 增加 capability source 模型，至少覆盖：
  - `Declared`：adapter 或 preset 的静态声明。
  - `Supplied`：调用方手工提供。
  - `Probed`：通过 CLI probe 或 registry handler 得到。
  - `Negotiated`：通过 ACP negotiation 得到。
- 为 `ExternalAgentCapabilities` 或等价 wrapper 提供 `source()` accessor。
- 更新 builder 的 capability 校验和错误信息，使调用方能看出当前判断来自哪个 source。
- 保持常见现有调用可编译。若必须调整构造函数，提供兼容 helper 或清晰迁移路径。
- 确认 serde、Debug、Clone、PartialEq 等 trait 行为符合现有测试期望。

验证条件：

- 增加测试覆盖：preset capability source 为 `Declared`。
- 增加测试覆盖：调用方手工 `.capabilities(...)` 的 source 为 `Supplied`。
- 增加测试覆盖：ACP negotiation 结果 source 为 `Negotiated`。
- 若 M4-1 能拿到 probe capability，增加测试覆盖：默认 handler API 得到的 source 为 `Probed`。
- 运行：

```bash
cargo test -p agent-lib --lib facade::external
```

### M4-4 [TODO] 让 probed capability 成为 managed external agent 的真实能力视图

上下文：

- `drive_external` 和 unsupported capability fallback 依赖 agent 持有的 capability view。
- 如果 agent 只持有 declared capability，即使 default handler probe 已经发现缺失能力，后续判断也可能误导。

实现要求：

- 更新 M4-1 的默认 handler builder API：probe 成功后，返回的 agent 必须持有 source 为 `Probed` 的 capability view。
- 如果现有 `default_external_session_handler` 只返回 handler，增加一个不会破坏旧 API 的 helper，返回 handler 和 probed capabilities，或在 builder 内部完成等价逻辑。
- `UnsupportedCapability` 判断必须基于 agent 当前持有的 capability view。
- probe 失败仍然走现有非 secret skip 或错误路径，不把命令行、环境变量 secret 或 provider token 写入错误。
- 更新 `docs/capability-matrix.md`，明确 declared 和 probed 的区别。

验证条件：

- 增加测试覆盖：当 probed capability 缺少某能力时，请求该能力返回 `UnsupportedCapability`，错误信息包含 capability 名称和 source，但不包含 secret。
- 增加测试覆盖：declared capability 支持但 probed capability 不支持时，以 probed 结果为准。
- 运行：

```bash
cargo test -p agent-lib --lib facade::external
cargo clippy --all-targets \
  --features "external-claude-code external-codex external-opencode external-acp" -- -D warnings
```

### M4-5 [TODO] Review：managed external 可用性和 capability 来源

检查范围：

- README 和 managed external docs 是否都给出了可工作的 handler 装配路径。
- 默认 feature 下是否仍不拉入 CLI adapter。
- capability source 是否覆盖 declared、supplied、probed、negotiated。
- unsupported capability fallback 是否基于真实 capability view。
- 错误和测试 fixture 是否不包含 secret。

验证条件：

- 运行：

```bash
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test -p agent-lib --lib facade::external
cargo check --examples
cargo clippy --all-targets \
  --features "external-claude-code external-codex external-opencode external-acp" -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
```

- 手工复核 `docs/refine.md` 中 “managed external capability 混淆声明和验证” 与 “README quick start 缺少 session handler” 两个条目的状态，必要时补充当前修复说明。

## M5：完整逃生出口

### M5-1 [TODO] 扩展 `AgentParts` 覆盖 external、协作和交互状态

上下文：

- `Agent::into_parts` 和 `AgentParts` 在 `src/facade/agent.rs` 附近。
- 当前拆解结果覆盖 LLM、conversation、tools、instructions、policy 等核心字段，但会丢失 external delegates、retained external sessions、协作状态和 interaction handler。
- `src/facade/agent/snapshot.rs` 中已有部分 snapshot/restore 相关状态，需要避免 `into_parts` 和 snapshot 的语义互相矛盾。

实现要求：

- 扩展 `AgentParts`，至少覆盖：
  - external agents 或 delegate registry 配置。
  - retained external sessions 或等价 data-only handle。
  - collaboration 配置和当前 live collab state。
  - interaction handler。
  - 已有 LLM、conversation、tools、instructions、policies、delegates 字段。
- 如果某些内部类型不适合作为 public API 直接暴露，设计封装类型或只读/data-only view，并在 rustdoc 说明限制。
- `Agent::into_parts` 不得静默 drop 仍然有语义价值的状态。
- 检查是否需要提供从 `AgentParts` 重新构造 `Agent` 的 helper。如果不提供，rustdoc 必须说明 `into_parts` 只是拆解出口，不是完整 restore API。

验证条件：

- 增加测试覆盖：构造带 interaction handler 的 agent，`into_parts` 后 handler 仍存在。
- 增加测试覆盖：构造带 collaboration 的 agent，`into_parts` 后 collab 配置和当前状态可见或可继续接管。
- 增加测试覆盖：构造带 external delegate 的 agent，`into_parts` 后 external 配置没有丢失。
- 如果 retained external session 可在单元测试中伪造，增加测试覆盖；如果不可伪造，在任务记录中说明当前验证边界。
- 运行：

```bash
cargo test -p agent-lib --lib facade::agent::
```

### M5-2 [TODO] 对齐 `into_parts`、snapshot 和 builder 文档

上下文：

- `AgentParts` 是高级调用方逃生出口。
- `AgentSnapshot` 是持久化和恢复 API。
- Builder 是常规构造 API。
- 三者都能表达一部分 agent 状态，但用途不同，文档必须避免暗示它们可互相替代。

实现要求：

- 在 `src/facade/agent.rs` 的 rustdoc 中说明 `Agent::into_parts` 拆出的资源范围和不保证事项。
- 在 `docs/facade-api.md` 中说明：
  - 需要持久化恢复时使用 snapshot。
  - 需要接管 live handles 时使用 `into_parts`。
  - 需要常规构造时使用 builder。
- 如果 `AgentParts` 新增 public 字段，检查 README 示例是否需要更新。
- 确认 `docs/refine.md` 中关于逃生出口的条目反映新行为。

验证条件：

- 运行：

```bash
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
cargo test -p agent-lib --lib facade::agent::
```

- 文档中不能继续说 `into_parts` 覆盖完整状态但实际字段缺失。

### M5-3 [TODO] Review：完整逃生出口

检查范围：

- `AgentParts` 是否覆盖当前 `Agent` 中所有有语义的字段。
- 是否有 public API 泄漏了不该稳定承诺的内部实现细节。
- `into_parts`、snapshot、builder 的用途边界是否清楚。
- M3 的协作 snapshot 修复和本阶段 `into_parts` 扩展是否互相一致。

验证条件：

- 运行：

```bash
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test -p agent-lib --lib facade::agent::
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
```

- 手工复核 `docs/refine.md` 中 “Agent::into_parts 状态覆盖不完整” 条目的状态，必要时补充当前修复说明。

## M6：最终收口

### M6-1 [TODO] 同步 `docs/refine.md` 的问题状态和剩余风险

上下文：

- `docs/refine.md` 是本轮 refine 的起点。
- 每个 milestone 完成后，它应该从“问题清单”逐步变成“已修复项和剩余风险记录”。

实现要求：

- 为每个已完成问题补充修复摘要、关键文件和测试命令。
- 对仍未完全修复的问题保留明确状态，不要把风险删除。
- 如果某个问题在实现中被拆成更细问题，把拆分原因写清楚。
- 确认 `docs/refine.md` 不再和 `PLAN.md`、`TODO.md` 的状态冲突。

验证条件：

- 手工检查 `docs/refine.md` 中六类问题都有明确状态。
- 运行：

```bash
git diff --check
```

### M6-2 [TODO] 全量验证默认构建、测试、文档和 external feature clippy

上下文：

- AGENTS.md 要求收尾时按 cheap 到 expensive 顺序运行格式、clippy、test、doc。
- managed external adapter feature 默认关闭，但相关代码被本计划触及后必须单独 clippy。

实现要求：

- 运行完整验证命令。
- 如有失败，回到对应 milestone 修正，不要只记录失败。
- 对 ignored real e2e 不做默认强制运行，但确认它们仍然保持 ignored 或未配置时干净跳过。

验证条件：

- 以下命令全部通过：

```bash
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test --all --all-targets
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
cargo clippy --all-targets \
  --features "external-claude-code external-codex external-opencode external-acp" -- -D warnings
```

- 任务完成记录中写明命令结果。

### M6-3 [TODO] Review：最终正确性和完整性验收

检查范围：

- 所有 milestone 的 review 任务是否已完成。
- `PLAN.md`、`TODO.md`、`docs/refine.md` 是否一致。
- README quick start 是否能让新调用方避开已知坑。
- 默认测试、feature clippy、rustdoc 是否都通过。
- 是否还有必须在本轮修复但未排入任务的设计目标缺口。

验证条件：

- `rg "\[TODO\]" TODO.md` 只应命中本任务本身尚未执行时的标记；当全部完成后不应再有未完成任务标记。
- `git diff --check` 通过。
- 最终完成记录必须列出：
  - 修复的设计目标差距。
  - 仍保留的非阻断风险。
  - 已运行的验证命令。
