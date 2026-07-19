# Conversation 核心数据结构设计

> 本文档细化 `DESIGN.md` §1.2 Conversation 层的核心数据结构。
> 它是 **revert/fork、compaction、cancel 一致性、agent loop** 四个功能共用的地基,
> 是整个框架里功能依赖最密集、返工代价最高的交汇点,因此单独设计。

## 0. 设计目标(从上层需求倒推)

这个数据结构必须同时满足以下来自 `DESIGN.md` 的硬需求:

| 来源 | 需求 |
|---|---|
| Revert/Fork | 切割点只能落在**完整轮次边界**;以 conversation 自己的单位(Turn)划分 |
| Compaction | **非破坏性**:原始 message 永不丢;压缩是叠加的投影 |
| Cancel 一致性 | **已关闭的 turn 任何时刻满足不变量**;半完成产物只存在于 pending,cancel 只影响 pending |
| Agent loop | 暴露"轮次完成"边界事件;支持"在边界注入 message";tool call_id ↔ result 记账 |
| 持久化 | 会话可存数据库;message 需要稳定 PK |

倒推出的结论,直接决定数据结构形状:

1. **不是裸 `Vec<Message>`**,而是 `turns + pending + head` 三部分。
2. **原始数据(raw)与呈现视图(projection)分离**:compaction/truncation 只动投影,不动 raw。
3. **一切边界与切割都以"轮次(Turn)"为单位**;`Boundary` 是受检类型,不是裸 `usize`。
4. **Message immutable 是基础不变式**(见 §1),它让 fork 从 O(n) 复制退化成 O(1) 共享。

## 1. 基础不变式:Message Immutability

**Message 一旦生成,内容永不改变。** 这是整个 Conversation 层的根基,一连串复杂性由它消除:

- **fork 廉价**:message 不可变 ⇒ 同一 `MessageId` 在多个分支里指向同一实体,无所谓"撞 PK"。fork 不复制 message,只是新建 conversation 让 head 指向共享历史(git / persistent data structure 模型)。
- **可变区唯一**:唯一可变的东西是 `pending`(正在流式生长)。`commit` 是**冻结时刻**——把可变态冻结成 immutable。这与 cancel 一致性天然咬合:cancel 只动没冻结的 pending,已 commit 的历史恒可用。
- **编辑历史 = fork**:ChatGPT 式"改一句旧话重新生成",在 immutable 下就是"新 message + 从编辑点 fork 新分支",原 message/原分支纹丝不动。无需专门设计 edit。

**边界守则**:随消息冻结时一起写入 envelope 的来源 metadata 可以保留在 message
旁边,但真正可变的附加数据(后期给历史消息打标记 / 评分 / 人工反馈)**不能塞进
message**,必须旁路——放到 conversation 级或独立 annotation 表,通过 `message_id`
引用。否则 immutability 被破坏。

## 2. 轮次(Turn):切割与不变量的单位

### 2.1 Turn 的定义(边界哲学)

**合法边界 = "球在用户手上"的点** —— 模型已给出**不含 tool_use 的最终回复**、等待下一个外部输入的时刻。

反例排除:若把 turn 定义成 req+resp,边界会落在 `tool_use` 之后(球在**工具**手上)。从那里 fork,新会话一上来就欠一次 tool execution,不合理。因此 `tool_use → tool_result` 之间**绝不允许边界**。

> **Turn = 从一个外部输入,到 agent loop yield(模型给出无 tool_use 的最终回复)为止的完整交换周期。**

一个 turn 内部可含**任意多轮 tool round-trip**:
```
Turn = [ user input,
         assistant + tool_use, tool_result,   ← round 1(球在工具→模型,非边界)
         user injection,                      ← pivot 可在闭合 tool_result 后进入 pending
         assistant + tool_use, tool_result,   ← round 2
         assistant final ]                    ← loop yield,turn 结束 = 边界
```
tool 配对**必然闭合在 turn 内部** ⇒ **每个 turn 边界都是安全的 fork/revert 点**,无需运行时判定"这个点能不能切"。

### 2.2 两级边界:职责分开

| 边界 | 归属 | 可见性 | 用途 |
|---|---|---|---|
| **Turn 边界** | Conversation 公开 API | 公开 | revert / fork |
| **Step 边界** | Agent loop 内部机制 | 内部 | compaction / pivot / 预算检查 |

- **Step 边界**:每个 tool_result 全部闭合之后、下一次 LLM 调用之前(turn 内部,配对已闭合,对 compaction 安全)。pivot 可在此处向 pending 追加 `user` 消息。
- **Turn 边界**:step 边界的最后一个(最终回复之后)。
- 两者**都绝不落在 tool_use 之后**。

**revert/fork 只以 Turn 为单位**——这是 conversation 层自己的原生概念,库在自己的抽象层级上用自己的单位做操作,内聚而非限制。**用户若需 message 级 revert/fork,下沉到 LLM API Client 层自行拼 message 序列**(那一层暴露原始 message 模型)。高层抽象不迁就低层需求。

### 2.3 Turn 的构成

```rust
/// 一个完整、自洽、已关闭的轮次。immutable。conversation.turns 里只存 Turn。
struct Turn {
    id: TurnId,              // 轮次分组 + 边界/配对的锚点
    messages: Vec<Message>,  // 本轮所有 message(user/assistant/tool/user/assistant…),均 immutable
    pairings: Vec<ToolPairing>,  // 本轮内的 tool 配对(必全部闭合)
    parent: Option<TurnId>,  // git 式父指针(见 §7);构成分支树
    meta: TurnMeta,          // usage、时间戳(外部注入)、来源等
}
```

> Turn 与 provider 的 message 模型是**中立层**。Anthropic 把 tool_result 放进 user message、OpenAI response 用独立 item —— 差异在序列化到 wire 时由 Client 层适配,Turn 只维护中立配对关系。

### 2.4 tool_use / tool_result 配对

```rust
struct ToolPairing {
    call_id: ToolCallId,          // 框架内部 id(记账用,不依赖 provider id)
    provider_call_id: Option<String>,  // provider 原始 id,原样保留用于 wire 序列化
    call_msg: MessageId,          // tool_use 所在 message
    result_msg: Option<MessageId>, // tool_result 所在 message;None = 悬空(只允许在 pending)
}
```

## 3. 不变量(Invariants)—— 已关闭的 Turn 恒真

区分粒度是关键:

- **Immutability = message 粒度**:每条 message 冻结后永不改。
- **不变量 = turn 粒度**:只有**已关闭的 turn** 保证满足;**pending turn 内允许瞬时悬空**。

已关闭 turn(及 `conversation.turns` 整体)在**任何时刻**满足:

1. **I1 tool 配对完整**:无悬空 tool_use(每个 call 有 result_msg),无孤儿 tool_result。
2. **I2 role 序列合法**:message 的 role 序列可被两家 provider 接受。
3. **I3 无 partial**:不含未闭合 content block 或未 parse 的 partial tool_use JSON。
4. **I4 id 唯一**:MessageId / TurnId 在本 conversation 内唯一。

> 这四条由**类型 + 操作**共同保证:turns 只能通过 `commit(pending)` 推进,`commit` 校验后才关闭 turn;没有 API 能把裸 message push 进 turns。

**瞬时悬空的合法性举例**:pending turn 里 assistant message(含 tool_use)已冻结,但 tool_result 未到——此时存在一条带悬空 tool_use 的 immutable message,**不违反不变量**,因为它在 pending turn(未关闭)。cancel 时**追加**一条合成 tool_result(不改已冻结的那条),turn 自洽后才关闭。immutable(message)+ 不变量(turn)+ cancel 闭合(追加)三者无矛盾。

## 4. 四层结构总览

```
Conversation
├── turns:   Vec<Turn>              // 已关闭,immutable,满足不变量
├── pending: Option<PendingTurn>    // 唯一可变区
├── head:    Boundary               // revert 游标(见 §8)
├── id:      ConversationId
└── origin:  Option<ForkOrigin>     // 从哪个父对话/边界 fork 而来

Turn
└── messages: Vec<Message>          // immutable,各有 MessageId

PendingTurn
├── messages: Vec<Message>          // 本轮已冻结的 message(逐条冻结)
└── pending:  PendingMessage        // 正在流式生长的那一条 —— 不是 Message

PendingMessage                       // 缺终态标记、可能含未闭合 block / partial JSON;
                                     // commit 时分配 id、闭合、冻结 → 成为 Message
```

**PendingMessage 不是 Message**:用类型强制 immutability。一次 `feed` 段产出多条 message,它们**逐条冻结**:

```
feed(input) → 创建 PendingTurn
  assistant 流完 → 冻结成 Message,入 pending.messages
  tool 执行完   → tool_result 冻结成 Message,入 pending.messages
  下个 assistant 开始流 → 成为新的 pending.pending(PendingMessage)
  ...(任意多轮 tool round-trip)
  loop yield(无 tool_use 最终回复) → 关闭 PendingTurn → 追加进 conversation.turns → head 前移
```

**同一时刻只有一条 message 可变**(那条 pending),这正是"pending message 单数"的准确含义。

## 5. Pending 区与 Cancel 闭合

```rust
struct PendingTurn {
    messages: Vec<Message>,          // 本轮已冻结
    pending: PendingMessage,         // 正在流的一条
    open_calls: Vec<ToolCallId>,     // 已发出未应答的 tool_use(悬空只在此合法)
}

struct PendingMessage {
    role: Role,
    partial_blocks: HashMap<usize, PartialBlock>,  // Accumulator 折叠(参考 DESIGN.md streaming)
    // ... streaming 状态
}
```

**cancel(仅影响 pending,turns 永不被触碰):**
```
裂缝 A (open_calls 非空): 给每个 open call 追加合成 tool_result(status=cancelled)
                          → PendingTurn 自洽 → 可选择 commit 或整体丢弃
裂缝 B (partial_blocks 未闭合): 丢弃无法 parse 的,闭合能闭合的;partial 绝不原样保留
⇒ committed(turns)自始至终满足不变量 ⇒ "cancel 后仍可 feed" 天然成立
```

**finish 时的块级预检(M3-6):** assistant 响应 freeze 进 pending turn 时即过与 commit
同一来源的块级规则——角色语法 allowlist(`validation::sequence::block_allowed_for_role`)、
tool_use 完整性(空 id/空 name,`incomplete_tool_use_detail`)、provider call id 在消息内与
本轮已注册调用间的唯一性。非法响应在 `finish_assistant` 即报错(`InvalidAssistantBlock`/
`IncompleteToolUse`/`DuplicateProviderCallId`),不会拖到 commit 才暴露——那时 `ReadyToCommit`
只剩 DiscardTurn,整轮已冻结的 tool 往返只能作废。预检失败后 pending turn 保持原状,可正常
DiscardTurn 并继续 feed。

## 6. Projection:compaction / truncation 的家

发给 LLM 的不是 raw turns,而是 raw 经 projection 计算的有效视图。

```rust
/// 把 turns 投影成"实际渲染进 context window"的内容。非破坏性:只引用 turns,不改它。
struct Projection { spans: Vec<Span> }

enum Span {
    Raw { turns: Range<TurnIdx> },                 // 原样透传
    Compacted {
        covers: Range<TurnIdx>,                    // 被压缩的 raw 区间
        artifact: ArtifactId,                      // summary 产物(带 provenance)
        produced_by: StrategyRef,
    },
    // 未来可加 Dropped 等
}
```

- Span 覆盖边界必须对齐 **Turn 边界**(复用 `Boundary`),不切开配对。
- **有效视图** = 遍历 spans(以 `head` 为上界),Raw 段取原 turn、Compacted 段取 artifact。
- compaction = 生成新 Projection(把某段 Raw 换成 Compacted);raw turns 不动 ⇒ revert 可穿过压缩点。
- **spans 以 `head` 为上界**:超过 head 的 turn 不参与投影。

### 6.1 约束:compaction 只覆盖完整 turn(不涵盖 pending)

**compaction 只覆盖已 commit 的完整 turn,绝不涵盖 pending turn。** 三个理由:

1. **契合不变量**:pending 内允许瞬时悬空 / partial JSON(见 §3)。若 compaction 触及 pending,就要去压一个可能违反 I1/I3 的半成品。约束成"只压完整 turn"后,compaction 永远只作用于满足不变量的数据,pending 的可变性被隔离在其视野之外。
2. **投影简化**:spans 的覆盖范围恒为 `[0, head)` 内的完整 turn,**pending 永不在投影里**。有效视图 = compacted spans + 剩余 raw turns + (流式中 pending 的实时增量单独拼),三段互不交叠。§6 "covers 对齐 Turn 边界"自动满足。
3. **语义正确**:compaction 语义是"把已完成历史浓缩"。进行中的 turn 语义上不是历史,是"当前正在发生的事";压它等于事情没结束就写总结,既可能丢掉马上要用的上下文,也无法生成连贯摘要。

**时机推论(soft / hard limit 分工):**
- **soft limit 命中于 turn 中途**:只**标记待压**,不立即动作;推迟到当前 turn 到达边界(loop yield)时才执行 compaction。与"compaction 只在 turn 边界"一致。
- **hard limit 是 turn 内部的事,不由 compaction 兜底**:一个长 turn 反复调 tool、可能在结束前逼近/越过 hard limit,而 compaction 只能等 turn 结束。turn 内撞 hard limit 的保护是 **agent loop 层的责任**(在 step 边界处理:让当前 LLM 调用失败并回灌 loop,或其他应急机制),不是 Conversation compaction 的职责。

> 一句话:compaction 只认 turn 边界,soft limit 触发推迟到下个 turn 边界;turn 内 hard limit 保护归 agent loop 层。

**reverted head 上不可 compaction(M3-1 / H-STATE-1):** `apply_compaction` 要求 head 位于
lineage 末尾(`active_len == lineage_len`),否则返回 `CompactionOnRevertedHead`。
若在 reverted head 上压缩,新投影只覆盖 `[0, head)`,而 redo 不触碰投影——`effective_view()`
会静默丢失 `head..lineage_len` 的 turn,且无法自愈。要压缩 reverted 的状态,先 redo 到
lineage 末尾再应用 plan。

## 7. Identity 与 id 策略

```rust
struct MessageId(Ulid);        // 全局稳定 PK;去中心生成、外部注入
struct TurnId(Ulid);
struct ConversationId(Uuid);   // 每条(含 fork 出的分支)唯一

struct ForkOrigin { parent: ConversationId, fork_point: Boundary }
```

**两级 id 职责:**
| id | 作用 | DB 对应 |
|---|---|---|
| `MessageId` | 每条 message 全局唯一 PK | `messages` 表主键 |
| `TurnId` | 轮次分组 + 边界/配对锚点 | `messages.turn_id` / `turns` 表 |

**生成策略:框架侧生成,不依赖 DB 自增。**
- 用 **ULID(或 UUIDv7)**:去中心生成(fork/离线/多来源不冲突),时间单调 ⇒ 做 DB 主键索引局部性好。
- 在**创建时**分配(内存态就要引用 message:配对、projection.covers、fork),不能等入库。
- 与"数据模型不在内部调用 `now()`/随机源"呼应:id **从外部注入**,保持可测试/可回放。

**fork(因 immutable 而 O(1)):**
- **不重分配 id**、**不复制 message**。新 conversation 的 `head` 指向共享的某个 turn。
- 历史是 git 式 **parent 指针树**(`Turn.parent`);fork = 从某 turn 长出兄弟分支。
- provenance 在 conversation 级(`ForkOrigin`)+ turn 级(`parent`),分支树可重建(调试、A/B、agent 多路径探索白送)。

> `ToolCallIndex`(call_id → 位置的记账加速结构)从 turns + pending **派生**,非事实来源,fork/revert 后重建即可。

## 8. Revert:逻辑游标(已定)

Revert = **移动 `head`**,不物理删除任何 turn。

- committed turns 保留全部 raw(与 compaction 非破坏性、immutability 一致)。
- `head: Boundary` 表示"当前有效末端";所有视图计算以 head 为上界。
- 好处:revert 可 **redo**、被 revert 的内容调试可见、零数据丢失。
- fork 同理:从任一历史 Boundary 派生新 conversation,共享 head 之前的 turn。

## 9. Boundary:受检的切割点

```rust
/// 只能指向合法 Turn 边界的受检类型。由 Conversation 生成并校验。
struct Boundary {
    conversation_id: ConversationId, // token owner;跨 Conversation 不可复用
    turn_count: u64,                  // 此切割点之前有多少个完整 Turn
    after_turn: Option<TurnId>,       // zero 为 None,其余位置的稳定 Turn 锚点
    version: u64,                     // 生成时 structural version;变化即失效(防 ABA)
}

impl Conversation {
    fn valid_boundaries(&self) -> Vec<Boundary>;  // zero + lineage ceiling 内全部 Turn 边界
    fn boundary_after(&self, turn: TurnId) -> Result<Boundary, BoundaryError>;
    fn validate_boundary(&self, boundary: &Boundary) -> Result<(), BoundaryError>;
}
```

`Boundary` 字段私有，只由 Conversation 签发。serde 反序列化只能恢复不可信 token；消费时
统一检查 owner、version、位置/锚点、lineage/fork ceiling 与操作所需的一致点。公开
`validate_boundary`、revert、fork、compaction 等 Turn-boundary 操作继续要求 `pending == None`；
pending 内 step 注入使用单独校验,只接受当前 head token 且 pending 已停在闭合 tool-result
之后。逻辑 revert
后，`valid_boundaries` 仍可为同一 lineage 上 head 之后的 suffix 签发新版本 token 以支持
redo；旧 token 即使再次落在相同位置也必须返回 `StaleBoundary`。

| 功能 | 用法 |
|---|---|
| revert | `revert_to(Boundary)`:移动 head |
| fork | `fork_at(Boundary)`:新 conversation,共享历史 |
| compaction | Span 的 `covers` 两端对齐 Boundary |
| agent loop | `StepBoundary`:pivot 生效 / trace / 预算 / compaction 检查(step 粒度,内部) |

## 10. 序列化 / 持久化

- **可 serde**:`ConversationId` / `ForkOrigin` / `version` / `head` / `turns`(全部 Turn 与 Message) / `Projection` / artifacts。
- **不 serde,恢复时重建**:`ToolCallIndex`(派生);对 registry/client 的引用(运行时资源,见 `DESIGN.md` 序列化边界)。
- **pending 默认不持久化**:存盘只发生在 commit 后的一致点(Boundary 处)。"streaming 中途崩溃恢复"若需要再单议。
- **message 表 immutable**:只 INSERT,不 UPDATE;可变附加数据走独立 annotation 表(见 §1 边界守则)。
- **序列组织 = parent 指针树**:`turns(turn_id PK, conversation_id, parent_turn_id, ...)`、`messages(message_id PK, turn_id FK, seq, payload, meta, ...)`（`meta` 为 envelope 级来源 metadata，随消息冻结写入，行级 round-trip 保留）。查"某分支全部 message" = 沿 parent 链递归 CTE。fork 只写一行新 conversation + 新 head,不复制历史行。
- **行模型(rows)按代次演进,全程 insert-only**:行分两类——**不可变事实行**(turn / message / tool_pairing,稳定 id 主键;raw membership 为 append-only 关联;projection 头)只 INSERT 不 UPDATE,各次导出共享;**会演进的代次版本化行**(conversation 行、lineage 关联、projection span、artifact 成员)带 `generation` 列,其值恒等于导出一致点的 `structural_version`(每次结构性变更——commit / revert / compaction——递增,天然是代次计数器)。同一 Conversation 演进后重导出 = 以新代次**插入**新行,旧代次行原样保留,存储侧永不 UPDATE / DELETE。**当前状态 = 最大代次**:读回时返回全部行,由 `ConversationRowInsertSet::into_snapshot` 选取最大代次重组 snapshot,低代次行作为历史忽略。时序示例:commit → 导出 gen 1 行集(conversation(v1) + gen 1 的 lineage/span/artifact 行);revert → 导出 gen 2 行集(两代次共存,事实行共享);查询当前状态取 gen 2。旧 schema 版本的行集 pre-1.0 不提供迁移路径,直接报错要求用当前 crate 重导出。
- **时间戳/随机 id 外部注入**:数据模型内部不调用 `now()`/随机源(可测试、可回放)。

## 11. 实现顺序建议

1. `Message`(immutable)/ `Role` / `Turn` / `ToolPairing` + `commit` 的不变量校验器 —— **先把 I1..I4 用测试钉死**。
2. `PendingTurn` / `PendingMessage` + Accumulator 折叠 + `cancel` 裂缝闭合 —— **"cancel 后仍可 feed" 测试**。
3. `Boundary` + `valid_boundaries` + revert(head 游标)+ fork(共享历史,O(1))。
4. `Projection` / `Span` + `effective_view` + `apply_compaction`。
5. 持久化(serde + DB 映射 round-trip:存盘→恢复→`effective_view` 一致)。

> 依赖:本结构依赖 Client 层的 `Message` / `ContentBlock` / `StreamEvent` / `Usage` 定义(它们是 Turn 的内容物),建议与 Client 层这几个类型**并行确定**。
