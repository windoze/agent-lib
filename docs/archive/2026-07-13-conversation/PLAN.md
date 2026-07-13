# 实施计划：Conversation Core

> 本计划以 [`docs/conversation-core.md`](../../conversation-core.md) 为规范性设计输入，
> 在已完成的 provider-neutral Client 公共类型之上实现 Conversation Core。
> 总体架构边界见 [`DESIGN.md`](../../../DESIGN.md) §1.2；逐任务要求与完成记录见
> [`TODO.md`](TODO.md)。已完成的 Client 层计划和实施记录归档于
> [`docs/archive/2026-07-13-client-layer/`](../2026-07-13-client-layer/)。

## 范围与非目标

**范围**：Conversation 的不可变消息身份、完整 Turn 与 I1--I4 校验、事务性
`pending`、cancel 闭合、受检 `Boundary`、逻辑 `head`、非破坏性 revert、结构共享的
O(1) fork、`Projection`/compaction overlay、有效视图，以及一致点上的 serde 与
DB-neutral 持久化映射。Conversation 复用 Client 层的 `Message`、`ContentBlock`、
`StreamEvent`、`Accumulator` 与 `Usage`，不复制 provider wire 模型。

**非目标**：本计划不实现 Agent loop、tool registry 或工具执行器、approval/pivot、
具体 LLM summarizer、自动预算调度、数据库驱动、网络服务和多 agent 编排。Conversation
只提供这些上层功能所需的受检数据结构、状态转换、compaction 扩展点和持久化记录。
turn 内 hard-limit 应急仍属于 Agent loop；Conversation 只允许在完整 Turn 边界应用
compaction。

## 规范优先级与已定关键决策

`docs/conversation-core.md` 是本阶段的权威规范。若 `DESIGN.md` 中较早的方向性文字与它
冲突，以前者为准：fork 不重分配共享历史的 message id、不复制历史；revert 不物理
truncate，而只移动逻辑 `head`。

1. **Client payload 与 Conversation identity 分层**：Client `Message` 继续不含 id；
   Conversation 新增字段私有的 `ConversationMessage { id, payload }`。已冻结消息和
   `Turn` 不提供可变引用，历史通过共享所有权暴露只读视图。
2. **id 与时间全部外部注入**：`ConversationId`、`TurnId`、`MessageId`、
   `ToolCallId`、`ArtifactId` 使用互不混淆的强类型 id；数据模型内部不调用随机源或
   `now()`。调用方负责提供 UUIDv7/等价全局稳定值，便于确定性测试和回放。
3. **system prompt 单列**：Conversation 级配置单独保存 system prompt；已提交 Turn
   禁止 `Role::System`，从而与两家 adapter 的 `ChatRequest.system` 边界一致。
4. **Turn 是唯一公开切割单位**：canonical role 状态机为外部 `User` 输入开始，随后
   是 `Assistant`；带 tool use 的 assistant 后必须由一个或多个 `Tool` message 完整
   回答全部并行调用，再进入下一条 assistant；Turn 只能以不含 tool use 的最终
   assistant 结束。tool use/result 只能出现在各自合法 role 中。
5. **I1--I4 只有一个提交门**：raw history 不能直接 push。`commit` 原子校验 tool
   配对完整、role 序列合法、无 partial、id 唯一后才能生成 closed `Turn` 并推进历史；
   失败不改变 Conversation。
6. **pending 是唯一可变区**：同一时间最多一个 `PendingTurn`，其中最多一条
   `PendingMessage` 持有 Client `Accumulator`。只有成功完成的 stream/non-stream
   response 才能以外部提供的 `MessageId` 冻结；partial JSON 或未闭合 block 永不进入
   `Message`/`Turn`。
7. **cancel 只碰 pending**：丢弃活跃 partial；所有已冻结但仍 open 的 tool call 必须
   追加带 `ToolStatus::Cancelled` 的合成结果后再继续同一 Turn，或原子丢弃整个 pending
   回到最近 committed boundary。committed history 永不被改写，cancel 后必须可再次
   feed。为此先修复当前 `ContentBlock::ToolResult` 只有 `is_error`、无法无损保存
   `Denied/Cancelled` 的共享模型缺口；不得把状态塞进 `extra`。
8. **历史结构共享**：Conversation 内部使用持久化/结构共享表示保存 active lineage 与
   所有 raw turn。fork 只共享 immutable 节点并创建新的 conversation 元数据，复杂度为
   O(1) 且不 clone/re-id message；从已 revert 的 head 追加新 Turn 会形成新分支，旧 raw
   节点仍保留用于调试/回放。
9. **Boundary 受检且防 ABA**：字段私有并绑定来源 `ConversationId`、Turn 位置和生成时
   version，只能由 Conversation 产生。跨 conversation、越界、过期或 pending 状态下的
   boundary 操作返回分类错误。revert/fork 后需重新获取 boundary。
10. **head 是逻辑有效末端**：revert/redo 只移动 `head`；raw turn、message id 与
    compaction artifact 不删除。派生的 `ToolCallIndex` 在 head/fork/restore 后重建或更新，
    但从来不是事实来源。
11. **Projection 是非破坏性 overlay**：span 端点复用受检 Turn boundary，只能覆盖
    `[0, head)` 内完整 closed Turn，绝不覆盖 pending。有效视图按 head 截断；若 revert
    落入已有 compacted cover，不得泄漏 head 之后的摘要内容，应回退为该可见前缀的 raw
    turns，直到投影再次完整适用。
12. **数据与行为分离**：Turn、projection、artifact provenance、snapshot/row records
    可 serde；`Accumulator`、派生 index、client/registry handle 与 compaction strategy
    实例不 serde。compaction plan 使用可序列化 `StrategyRef` 指向外部运行时实现。
13. **持久化只在一致点发生**：存在 pending 时拒绝 snapshot。restore 必须重新运行
    I1--I4、parent/head/boundary/projection 校验并重建派生 index，不能让反序列化绕过
    commit 门。message/turn 行是 insert-only；fork 只新增 conversation/branch 元数据并
    引用共享父链。

## 里程碑总览

| 里程碑 | 目标 | 主要产出 |
|---|---|---|
| **M1 不可变已提交核心** | identity、完整 Turn、唯一 commit 校验门 | `ConversationMessage`、强类型 id、`Turn`、`ToolPairing`、I1--I4 validator |
| **M2 Pending 与 Cancel** | stream/non-stream 冻结、tool 往返、cancel 闭合 | `PendingMessage`、`PendingTurn`、open-call 记账、`CancelDisposition` |
| **M3 Boundary 与分支历史** | 受检切割、逻辑 head、revert/redo、O(1) fork | `Boundary`、结构共享 history、`ForkOrigin`、派生 `ToolCallIndex` |
| **M4 Projection 与 Compaction** | 非破坏性有效视图和 boundary-aligned overlay | `Projection`、`Span`、`Artifact`、`CompactionPlan`、策略/trigger 扩展点 |
| **M5 Serde 与持久化** | 一致点 snapshot、恢复校验、DB-neutral parent-tree records | `ConversationSnapshot`、row records、restore/index rebuild |
| **M6 跨功能验收与文档** | 状态机组合验收、公共 API/示例和总 Review | 端到端回归、Conversation 示例、README/crate docs |

依赖顺序固定为：M1 → M2 → M3 → M4 → M5 → M6。后续里程碑可以依赖前序公共 API，
不得通过先暴露裸容器或临时无校验构造器来并行绕开依赖。

## 建议目录与公共 API 边界

```text
src/
  conversation/
    mod.rs                 # Conversation、配置、状态转换入口与统一错误
    id.rs                  # 强类型、外部注入的稳定 id
    message.rs             # ConversationMessage immutable envelope
    turn.rs                # closed Turn、TurnMeta、ToolPairing
    validation.rs          # I1--I4 与 canonical role/tool 状态机
    pending/
      mod.rs               # PendingTurn 生命周期
      message.rs           # PendingMessage + Client Accumulator
      cancel.rs            # cancel 闭合与 disposition
    history.rs             # 结构共享 raw history、active lineage、派生 index
    boundary.rs            # Boundary 校验、head、revert/fork
    projection/
      mod.rs               # Projection、Span、effective_view
      artifact.rs          # Artifact/provenance 与 compaction plan
      strategy.rs          # dyn-safe strategy/trigger 扩展点
    persistence/
      mod.rs               # snapshot/restore 一致点
      rows.rs              # DB-neutral parent-pointer records
tests/
  conversation_*.rs        # 跨模块状态机与持久化验收
```

公共 API 只暴露受检操作和只读查询：`Conversation::begin_turn`、pending stream/result
推进、`commit`/`cancel`、`valid_boundaries`、`revert_to`、`fork_at`、
`effective_view`、`apply_compaction`、`snapshot`/`restore`。`Turn`、
`ConversationMessage`、`Boundary` 与 projection span 的不变量字段保持私有；不提供
`turns_mut`、裸 `Vec` 注入、unchecked boundary 或跳过 restore 校验的公共入口。

`effective_view` 返回 Client-ready 的 system 与完整 `Message` 序列；默认只含 head 以内
的 committed projection。pending 中已冻结的消息可通过单独只读接口供当前 agent step
拼接，活跃 `PendingMessage` 的 partial block 永不伪装成完整 Client `Message`。

## 测试策略与完成门

- **模型/serde 单测**：强类型 id、closed data、artifact、snapshot/row record 往返；
  runtime-only 状态不会被意外序列化。
- **不变量状态机测试**：逐项覆盖 I1--I4、parallel tool calls、重复/孤儿/悬空配对、
  非法 role/block、partial stream、原子失败和 commit 后不可变。
- **边界与分支测试**：zero/after-turn boundary、cross-conversation/stale/ABA、
  revert→redo、revert 后新分支、fork pointer sharing/no clone/no re-id 和原分支隔离。
- **projection 测试**：raw/compacted 混排、tiered/consolidated、head clipping、跨压缩点
  revert/redo、pending 排除、非法/重叠/越界 cover 拒绝及 raw 永不改变。
- **持久化测试**：包含 tool round-trip、fork、revert 和 compaction 的会话执行
  存盘→恢复→`effective_view` 全结构一致；损坏 parent/id/head/projection 数据拒绝；
  pending snapshot 拒绝；派生 index 恢复后等价。
- **跨 provider 回归**：Conversation 输出只含 Client 中立类型，并能被 Anthropic 与
  OpenAI Responses 请求 mapper 接受；不在 Conversation 断言层引入 provider 特判。
- **命令顺序**：每个任务先运行 `cargo fmt --all`，再运行
  `cargo clippy --all-targets -- -D warnings`，随后运行聚焦测试和
  `cargo test --all --all-targets`，最后运行
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` 与 `git diff --check`。完整测试最长
  30 分钟，单个测试必须少于 1 分钟。Conversation 单测默认离线，不要求真实 endpoint。

## Serde / 持久化边界

持久化事实包括 conversation 配置与 identity、全部 raw closed turns/messages、active
lineage、逻辑 head、version、fork origin、projection spans、artifact 与 provenance。
共享历史以 `Turn.parent`/稳定 id 表达，snapshot 和 row restore 都必须检测环、缺失父节点、
重复 id 与不属于当前 lineage 的 head。

不持久化 `PendingTurn`/`PendingMessage`、Client `Accumulator`、`ToolCallIndex`、锁、
`Arc<dyn ...>`、client/registry handle 或策略实例。`snapshot()` 只在 `pending == None` 的
Boundary 一致点成功；restore 通过受检构造路径创建 Conversation，随后重建派生 index。
DB-neutral row 映射明确 message/turn insert-only，annotation 另表引用 `MessageId`，不得
通过 UPDATE 修改已冻结 payload。

## 每阶段结束的 Review

每个里程碑末尾必须有独立 `Mx-R` Review，核对本阶段的设计约束、公共 API 封装、错误
边界、测试与 rustdoc，并确认下一阶段的前置条件真实满足。Review 不能代替实现任务，
也不能以完成记录或注释掩盖未闭合的不变量。M6-R 额外回溯
`docs/conversation-core.md` 全文，确认 Message immutability、Turn/tool 配对、pending
cancel、Boundary/head/fork、Projection 和持久化约束均有实现与可判定测试。
