# 执行计划 — M5-1：嵌套机器状态与 `AgentPath` 落位

## 选中的任务
`TODO.md` 第一个未完成任务 = **M5-1**（M1..M4-R 全 `[DONE]`；M5-1 起为 TODO）。
前置 M4-R 已完成。

## 任务目标（TODO.md M5-1）
1. 扩展机器状态：一个节点可持有零或多个子机器（`BTreeMap<AgentSlot, ChildMachineState>`），
   整棵树 serde；live handle 仍在 driver 侧。
2. `step` 递归推进整棵树到静止；树上任意位置 outstanding requirement 聚合进
   `StepOutcome.requirements`，每个带真实 `origin: AgentPath`。
3. requirement 兑现结果 `Resume` 按 `id`(+`origin`) 精确路由回对应子机器。
4. `LoopCursor` 各 cursor 的 `AgentPath` 字段（M2-2 已留）填真实路径。

## 关键设计决策（最终实现）
- 新建 `src/agent/machine/nested.rs`，定义 live `NestedMachine`（实现 `AgentMachine`）与
  可序列化快照 `MachineTreeState`。
- `NestedMachine { own: DefaultAgentMachine, children: BTreeMap<AgentSlot, ChildNode>,
  path: AgentPath }`，`ChildNode { machine: NestedMachine, pending_start: Option<AgentInput> }`
  （递归树）。`path` = 该节点在树中的**绝对路径**，不入 serde（由结构重建）。
- **绝对路径打戳**（统一机制，满足 bullet 4）：单机 `DefaultAgentMachine` 恒把 cursor 绑定与
  emitted `Requirement.origin` 打在 root；每个节点在自己 own 机步进后，用 `step_own` 把 own
  刚产出的 requirement 与 own cursor 绑定重打成本节点的 `path`。子节点递归自打，故冒泡=纯 append。
- delta 语义（与 `AgentMachine` “本步新增 requirement” 契约一致）：
  - `step(External)`：`step_own` 喂 own，再 `start_pending_children` 启动所有 `pending_start`
    子机器（一次 feed 推进整树），子机器自打绝对路径。
  - `step(Resume)`/`step(Abandon)`：`route_by_id` 按 `id` 定位命中的节点（扫描各 cursor 的
    `pending_requirement_ids`，递归 `subtree_contains`），投递给该节点；命中 own 走 `step_own`，
    命中子树直接 append（子已自打）。无节点等待该 id → 投给 own 让其分类报错。
  - `cursor()` 返回 root 的 `own.cursor()`；`quiescent = !has_pending_starts()`。
- cursor 打戳 API：`LoopCursor::rebase_origin(&AgentPath)`（同模块直改私有 origin 字段）→
  `AgentState::rebase_cursor_origin`（`pub(crate)`）→ `DefaultAgentMachine::rebase_cursor_origin`
  （`pub(crate)`）。只改寻址元数据，不过 transition 校验。
- serde：`impl Serialize for NestedMachine`（借用 `own.state()`，递归子树，含 `pending_start`）；
  `MachineTreeState: Deserialize` + `NestedMachine::from_state(state, make_fn)` 递归 `from_state_at`
  按结构重建每节点 `path`，重注入 handle。
- **序列化边界**：单机 parked（NeedLlm 卡住）时 Conversation 有 pending turn，Conversation 核心
  拒绝快照（既有不变量），故整树只能在 committed 边界（Idle/Done）序列化。round-trip 测试用
  parent=Done + child=Idle（保留 pending_start）两个不同 cursor 独立恢复。

## 进度
- [x] 选中 M5-1，读 TODO/PLAN/迁移文档 §7/§9
- [x] cursor 打戳链：LoopCursor::rebase_origin / AgentState / DefaultAgentMachine
- [x] 实现 NestedMachine（绝对路径打戳）+ serde + from_state
- [x] 导出 + 聚焦测试（4 个全绿：聚合真实路径 / 按 id 路由 / 整树 round-trip / slot 占用报错）
- [x] 全套验证：fmt clean、clippy 0 warning、`cargo test --all` lib 427 passed/0 failed、
      `RUSTDOCFLAGS=-D warnings cargo doc` clean、`git diff --check` clean
- [x] TODO.md [DONE] + 提交
