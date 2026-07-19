# 实施计划：mag 缺口收口（委派交互路由 / facade reconfigure / cancel 强化）

> **唯一设计输入**：[`docs/mag-gaps.md`](docs/mag-gaps.md)（mag 对 agent-lib 的缺口需求，
> 含现状锚点与验收方向）。上游需求方：mag 仓库 [`docs/CLI.md`](../mag/docs/CLI.md)（mag 的
> CLI 里程碑设计，§5A 是本轮评估结论）。
>
> 旧版计划和任务单已归档（最近一轮）：
>
> - [docs/archive/2026-07-19-review-fixes/PLAN.md](docs/archive/2026-07-19-review-fixes/PLAN.md)
> - [docs/archive/2026-07-19-review-fixes/TODO.md](docs/archive/2026-07-19-review-fixes/TODO.md)
>
> 缺口按编号引用（A1–A4、C1–C6），定义见 [`docs/mag-gaps.md`](docs/mag-gaps.md)。
> 逐任务清单见 [`TODO.md`](TODO.md)。

## 目标

1. **委派交互路由（A1）**：委派链中子 agent（local LLM subagent + external ACP agent）暂停的
   交互路由到父级注入的异步 `InteractionHandler`，携带 delegate 归因；父级未注入时保持现状。
   external ACP agent 的 `session/request_permission` 端到端打通（不再 `UnhandledRequirement`
   失败整个委派）。
2. **facade reconfigure（A2）**：把 agent 层已齐备的 `ReconfigRequest` 机制透出到 facade，
   turn 边界生效；tool set 替换时声明与执行闭包一致性有明确保证。
3. **cancel 强化（A3/A4）**：ACP read loop 取消响应（不再等满 120s IO 超时）；cancel/abandon
   时 external 子进程不泄漏；被阻塞的 tool/interaction 批可被 cancel 抢占，语义文档化。
4. 每个行为变更同步更新拥有该行为的文档；默认测试保持离线可跑。

## 非目标

1. 不做 C 组可选项（C1 专用 `Cancelled` 错误变体、C2 DelegationProgress/Message 发射、
   C3 local subagent 可执行工具、C4 pivot 窗口可见性、C5 external-start 异步化、C6 trace 字段
   增补）——登记在 `docs/mag-gaps.md` C 组，本计划不覆盖；A1 关联的 external-start approval
   允许按「文档承诺 + 推荐工具门」降级（M1 内评估定夺）。
2. 不重写 Conversation、AgentMachine、external runtime 的核心架构（sans-io、committed log +
   pending + projection、requirement/handler 模型保持不变）。A1/A4 是在既有 scope/handler 与
   abandon 机制上接线，不是新机制。
3. 不引入新的默认依赖；`external-*` feature 默认关闭的现状不变。
4. 不改变 secret 处理策略；归因信息只含 delegate 名/深度等结构化元数据，不带任务内容。
5. 1.0 前的 API 稳定性不作为约束，但 breaking change 必须在任务完成记录中显式注明。
   优先选择向后兼容的形状（`Interaction` 加可选字段而非改签名、新增 facade 方法而非改既有
   方法语义）。

## 排序原则

1. **A1 最先（M1）**：它是 mag 里程碑最大的硬阻塞（子 agent 权限请求今天直接崩委派），且
   涉及 local/external 两条驱动路径，越早定型归因模型越好。
2. **A2 次之（M2）**：facade reconfigure 依赖 agent 层既有机制，主要是透出与一致性校验，
   与 M1 无代码交叠，但 mag 的配置系统（`apply_config`）等它。
3. **cancel 强化最后（M3）**：A3/A4 都在取消路径上（external read loop、session 清理、批
   抢占），语义相互关联，合并为一个里程碑统一收口；它对 mag 是体验级而非阻塞级，放最后。
4. **先行为后文档**：每个里程碑的 review 任务核对行为与文档一致后才许勾销。

## 里程碑

### M1：委派交互路由（A1）

落地归因模型，打通 local subagent 与 external ACP 两条委派路径的交互上抛。

- 归因：`Interaction` 增加可选 delegate 归因（serde 向后兼容），或等价的包装/携带机制；
  `PermissionRequest.actor` 语义保持。
- local：`FacadeSubagentSpawner` / `ChildAgentScope` 的应答路由到父级注入 handler
  （policy 仍 gate，answer 上抛）；无父 handler 时保持同步 policy 现状。
- external：`drive_external` 的外层 `EmptyExternalScope` 替换为可服务 `NeedInteraction` 的
  路由层；`ExternalPermissionMode::Prompt` 端到端可用。
- external-start approval 去留在本里程碑内评估：异步化，或文档承诺保持 sync + 推荐
  `ask_<name>` 工具门。

重点文件：

- `src/agent/interaction.rs`、`src/agent/permission.rs`
- `src/facade/delegate.rs`（`FacadeSubagentSpawner`、`ChildAgentScope`、`DelegationToolHandler`）
- `src/facade/external.rs`（`drive_external`、`ExternalChildScope`、`EmptyExternalScope`）
- `src/facade/agent.rs`、`src/facade/agent/stream.rs`（supervisor 作用域装配）
- `src/agent/external/machine.rs`（`NeedInteraction` 具现点）
- `docs/facade-api.md`、`docs/managed-external-agent.md`、`docs/agent-layer.md`

### M2：facade reconfigure（A2）

把 agent 层 `ReconfigRequest` 机制透出到 facade，turn 边界生效。

- facade reconfig 入口（`Agent` 方法或 builder），至少覆盖 `SetModel` / `ReplaceToolSet` /
  `SetSystemPromptOverlay`；skill 类请求透出与否在本里程碑定夺并文档化。
- `ReconfigRegistryHandler` / `ToolRegistryResolver` 接线到 facade 同步与流式两条驱动路径。
- `ReplaceToolSet` 的声明/闭包一致性保证（一并替换注册表，或校验名字集合、不一致报错）。
- reconfig 与 snapshot/restore 的交互确认（reconfig 落进 `AgentState` 先于快照点）。
- 2026-07-20 review 追加（TODO M2-3/M2-4）：委派工具（`ask_<name>`）移除后对路由层同样生效
  （不允许绕过过滤 registry 继续驱动委派）；restore 校验快照工具集 ⊆ 重注入面（不允许
  永久锁死状态）；`SetModel` 准入校验对齐 builder；facade re-export 补齐。

重点文件：

- `src/agent/state.rs`、`src/agent/state/queue.rs`（`ReconfigRequest`、`queue_reconfig`）
- `src/agent/drive/reference.rs`（`ReconfigRegistryHandler`、`ToolRegistryResolver`）
- `src/facade/agent.rs`、`src/facade/agent/stream.rs`、`src/facade/agent/snapshot.rs`
- `src/facade/agent/reconfig.rs`、`src/facade/delegate.rs`（委派路由与过滤 registry 的次序）
- `docs/facade-api.md`、`docs/agent-layer.md` §4.2

### M3：cancel 强化（A3 + A4）

统一收口取消路径的三处缺陷。

- ACP read loop 对 cancellation `select!`（子进程静默时 cancel 秒级生效）；其余 CLI adapter
  读循环按现状评估。
- cancel/流 drop 致 external drive abandon 时的 session 清理：facade 路径自动触发，或暴露
  一等清理入口——以「宿主不做额外动作也不泄漏子进程」为验收。
- tool/interaction 批的 cancel 抢占：批级等待可被打断，在途 requirement 按 never-resume
  settle；被丢弃 future 的语义（drop + 长工具自行 select `ToolContext::cancel`）文档化；
  流式与非流式一致。

重点文件：

- `src/agent/external/acp/adapter.rs`、`src/agent/external/acp/connection.rs`
- `src/agent/external/registry.rs`、`src/facade/external.rs`
- `src/agent/drive.rs`（`fulfill_batch`）、`src/agent/drive/reference.rs`
- `src/facade/agent/stream.rs`、`src/facade/agent.rs`（非流式路径）
- `docs/managed-external-agent.md`、`docs/agent-layer.md`、`docs/agent-effect-model.md`

## 完成定义

每个里程碑的 review 任务必须确认：

1. 该里程碑覆盖的缺口条目（`docs/mag-gaps.md`）逐条核实已落地或明确降级（降级 = 文档与
   实现一致地承认现状）。
2. 全部门禁通过：

```bash
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo clippy --all-targets \
  --features "external-claude-code external-codex external-opencode external-acp" -- -D warnings
cargo test --all --all-targets
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
```

3. 拥有该行为的文档已同步（至少检查 `README.md`、`AGENTS.md`、`docs/facade-api.md`、
   `docs/managed-external-agent.md`、`docs/capability-matrix.md`、`docs/agent-layer.md`、
   `docs/agent-effect-model.md`）。
4. `docs/mag-gaps.md` 中对应条目已标注修复状态（`✅ 已修复（M*-*）` 或
   `📄 已降级（文档承认现状，M*-*）`）。
5. mag 侧验收线索：M1 完成后 mag 可用「ACP 子 agent 权限请求 → root handler 应答 → 委派
   继续」的离线 e2e 验证；M2 完成后 `apply_config` 可在 turn 边界换 model/tools/system；
   M3 完成后 cancel 一个静默 ACP 子进程秒级返回且无子进程泄漏。
