# agent-lib 设计文档

> 本文档记录框架的架构分层与已确定的设计取向,作为后续实现的基线。
> Agent orchestration 层为方向性设计(未细化实现),重点在于它对下层提出的约束。

## 1. 架构分层

框架分为三层,自底向上:

### 1.1 LLM API Client 层
统一各家 LLM 的 API 接口,抽象 message / response / streaming,并提供 **capability** 概念描述 API 能力(如是否支持多模态)。

支持的 provider 协议(按 **wire format** 划分,而非厂商):
- **OpenAI Response**:OpenAI、Microsoft Foundry、Grok 等
- **Anthropic**:Anthropic、Grok、DeepSeek、ollama、vllm 等

不支持:
- **OpenAI chat/completion**:新服务端普遍支持 response 或 anthropic;且 chat API 被各家定制过多,兼容性成本高。
- **Gemini**:当前阶段无需求、无测试条件,暂缓。

> **协议 vs 厂商方言**:协议是稳定的,厂商是易变的。同一协议下存在厂商方言差异(缺失字段、不支持 `cache_control`、content block 类型差异、system prompt 处理不同等)。在 `Protocol`(wire format)之外,应引入轻量的 **endpoint config**(base_url、auth 方式、方言开关/quirks),不让方言差异污染核心 message 模型。

### 1.2 Conversation 层
负责消息流的维护和 tool 支持:tool schema、tool call/response id 的对应关系。

要点:
- 屏蔽两家 provider 的模型差异(Anthropic 的 `tool_use`/`tool_result` 是嵌在 message 里的 block;OpenAI response 是独立 item),对外用中立模型。
- **System prompt 归一化**(Anthropic 单独字段 vs OpenAI message)。
- 预留 **context management** hook(truncation / summarization),即使先不实现。
- Conversation 可序列化(存的是 message 列表),便于中断续跑与调试回放;其引用的 registry 不序列化。

#### Revert / Fork
属于 Conversation 层(操作本层维护的消息流与 id 映射)。难点不在 truncate/clone 本身,而在**边界合法性**。

**核心洞察:边界不是 message,是轮次(turn)。**
单个 message 不是安全切割点,因为一次 assistant 轮次可能横跨多个逻辑单元,它们之间存在不可断开的引用关系。最典型的是 **tool_use ↔ tool_result 配对**:

```
[user]        "查一下天气"
[assistant]   text + tool_use(id=call_1)      ← 在这里切会留下悬空 tool_use
[tool]        tool_result(id=call_1)
[assistant]   "今天晴"
```
在 tool_use 之后、tool_result 之前 truncate,会留下有 call 无 result 的悬空 tool_use,发回任何一家 provider 都会报错(两家都强制配对)。**合法切割点只能落在完整轮次的边界上。**

**边界检测(`can_revert_to` 类判定)要检查:**
1. **tool 配对完整性(硬约束)**:切割点不能把配对的 tool_use / tool_result 分开。
2. **不能切在流式进行中**:某 message 仍是 streaming 未 `collect` 完成的 partial 状态时,不是合法边界。
3. **role 序列合法性**:truncate 后结尾要能接上下一步(如结尾不能是孤立的 tool_result)。
4. **(fork 专有)** 起点之前的历史须自身自洽。

**Revert(原地截断回退)**:本质是 truncate。
- id 维护:被砍掉 message 携带的 `tool_call_id`,若框架维护了 `call_id → 状态` 映射表,truncate 时须**同步清理**,否则悬空。
- Conversation 若有版本号 / 修改计数,递增。

**Fork(从某点分叉新对话)**:本质是 clone 前缀 + 新建分支。
- **id 冲突是关键**:provider 分配的 `tool_call_id` 一般不冲突;但框架**内部**生成的 id(message id、分支内序号)在 clone 时必须**重新分配**,否则两分支撞 id,持久化/回放无法区分。
- **fork 需要 identity**:每条分支有自己的 `ConversationId` / `BranchId`,可选记录 `parent_id + fork_point`,以重建分支树(调试、A/B、agent 多路径探索)。

**设计落点:**
- Conversation 内部不只是 `Vec<Message>`,而要能识别**轮次边界**(或提供"index 归属哪个 turn、turn 是否完整"的查询)。边界检测建立在轮次之上。
- 把边界合法性编码进类型(`Boundary` 只能指向合法轮次边界),而非裸 `usize`:
  ```rust
  fn revert_to(&mut self, boundary: Boundary) -> Result<Truncated, RevertError>;
  fn fork_at(&self, boundary: Boundary) -> Result<Conversation, ForkError>;  // 返回新实例,不改自身
  fn valid_boundaries(&self) -> Vec<Boundary>;  // 可切割点,给 UI/调用方选
  ```

#### Compaction
长对话压缩,同样属于 Conversation 层。

**前置原则:compaction 必须非破坏性(overlay,不是 truncate)。**
若破坏性地删原始 message 换成 summary,就无法 revert 到压缩点之前,调试回放也丢失原始信息(与上面的 revert/fork 直接冲突)。因此:
- Conversation 保存**完整原始 message log(raw log)不变**。
- compaction 产出 **summary artifact + 一层投影(projection)**,决定"实际渲染进 context window 的内容"。
- 发给 LLM 的是 raw log 经 projection 后的**有效视图**,不是 raw log 本身。

这样 compaction 与 revert 不打架:raw 还在,revert 可穿过压缩点;compaction 只是换了"如何把 raw 投影成 context"的策略。

**统一模型:段(span)。**
一条会话是一串 span,每个要么 `Raw(turns)`,要么 `Compacted { artifact, covers: range, produced_by: strategy }`。compaction = 取连续若干 span,叠加产出一个新的 Compacted span。压缩点**复用 `Boundary`(轮次边界)**——同一条硬约束:不能切开 tool_use/tool_result,必须落在完整轮次边界上。

**三个关注点解耦:**

*策略(strategy)—— trait 化,可组合:*
```rust
#[async_trait]
trait CompactionStrategy {
    async fn compact(&self, spans: &[Span], ctx: &CompactCtx) -> Result<Artifact>;
}
```
实现:`Summarize`(整段 LLM 压缩)、`Truncate`/`Drop`(直接丢弃,零 LLM 成本,如丢早期无关的 tool 大输出)、`SlidingWindow`(保留最近 N 轮)。

*时机(trigger)—— 与策略解耦,在轮次边界求值:*
```rust
trait CompactionTrigger {
    fn should_compact(&self, conv: &Conversation, usage: &Usage) -> Option<CompactionPlan>;
}
```
- **手动**:调用方直接调 `compact(plan)`。
- **自动**:在**一个轮次完成后**求值(天然 checkpoint,不会切在流式中途或 tool 配对中间),如"window 使用超过 soft limit"。依赖横切层 usage accounting。
- 两级阈值:soft limit 触发 compaction,hard limit 是硬上限。
- trigger 只决定"要不要压、压哪段",产出 `CompactionPlan` 交策略执行,不决定"怎么压"。

*分阶段/分层压缩(rolling / tiered summary):*
再次触发压缩时,如何处理"已有旧 summary span"和"新 raw 尾巴"——两种都合法,框架都支持:
- **(a) 分层追加(tiered)**:旧 summary 原样保留,只把新 raw 尾巴用"摘要 raw 轮次"的 prompt 压成新一层 → `[Compacted(S1), Compacted(S_tail)]`。省钱、旧摘要稳定。
- **(b) 合并重写(consolidate)**:旧 summary + 新尾巴一起用"整合摘要"的 prompt 重压成一条 → `[Compacted(S2, covers all)]`。更连贯,防止分层越堆越碎。

`CompactionPlan` 能**分别寻址"已有 summary 段"与"raw 尾段",各自指定策略/prompt**:
```rust
struct CompactionPlan { steps: Vec<CompactionStep> }
struct CompactionStep {
    target: SpanRange,                      // 指向已有 summary 段 或 raw 尾段
    strategy: Arc<dyn CompactionStrategy>,  // 各带自己的 prompt/model
}
```
同一套机制覆盖所有场景:"两阶段不同 prompt" = 两个 step 的 plan;"整条会话压缩" = 覆盖全部的单 step plan;"截断早期" = 前段 `Drop` + 尾段 `Summarize` 的 plan。

**Artifact 记 provenance(一等 serializable 对象):**
`covers`(哪些轮次/原 span)、`produced_by`(strategy/prompt/model)、`tokens_before/after`。原因:分层的"summary of summaries"需知下层覆盖了什么;调试可追"摘要怎么来的、丢了什么";成本核算。

#### Cancel 一致性(最高优先级正确性约束)
一个 turn 被 cancel 后,conversation **必须能恢复到可用状态,仍可继续 feed**。OpenCode / Codex 都踩过这个坑:cancel 后会话永久损坏,只能新开。根因是 cancel 停在了破坏消息流不变量的**半完成状态**。

**两处裂缝:**
- **裂缝 A —— 悬空 tool_use**:`tool_use` 已写进 conversation,tool 执行中被 cancel,`tool_result` 还没回。留下有 call 无 result 的悬空 tool_use,下次发给 provider 直接 400。
- **裂缝 B —— partial streaming message**:streaming 到一半被 cancel,只有半截 content block(甚至半截 tool_use JSON)。partial 原样留下会破坏后续。

**核心原则:cancel 落到"最近一致边界",不是"当前状态"。** 两条实现约束:

*(1) Conversation 事务性推进(committed log + pending)。*
turn 的产物先攒在 **pending/staging 区**,到达完整 `Boundary` 时才 commit 进正式 log。不变量:**committed log 任何时刻都满足 tool 配对 + role 序列合法 + 无 partial**。cancel 只影响 pending 区,committed log 永远可用。
> pending/staging 与 compaction 的 raw log 是同一套"分层写入"设施。Conversation 核心数据结构因此确定为 **committed log + pending + projection** 三部分,不是裸 `Vec<Message>`。这是 revert/fork、compaction、cancel 三者共用的地基。

*(2) Cancel 闭合裂缝到一致状态。*
- 裂缝 A 默认:给未应答的 tool_use 补一个**合成 tool_result**(`{ status: cancelled, content: "interrupted by user" }`),配对补齐,会话保持"tool 被取消但已应答"的合法状态,**可继续 feed**。(备选:回滚整个 turn 到上一干净边界,可用但丢这轮。)
- 裂缝 B:丢弃 partial(回滚到 turn 开始),或 `collect` 已到达部分并闭合;partial tool_use JSON 无法 parse 时只能丢。**partial 绝不原样保留。**

**区分两种 cancel:**
- **取消当前 feed 段**(默认):停 loop、闭合裂缝、conversation 回到可用 → 可再 feed。
- 会话对象**永不进入 poisoned 状态**。"cancel 后 conversation 仍可 feed" 是**硬性验收标准**(写成测试钉死)。

### 1.3 Agent Management / Orchestration 层

方向性设计(未细化实现)。这一层大概率**不止一层**,并依赖若干垂直/横切功能。

#### Layer A —— 单 agent 执行(Agent Runtime / Loop)
一个 agent = LLM client + conversation + tool registry + 一个 loop 策略。核心是 agent loop:
```
call LLM → response → 有 tool_use? → 执行 tools → tool_result 回灌 conversation → 再 call
                    → 无 tool_use / stop → 结束这一段
```
要管:终止条件(stop reason / max steps / 预算耗尽 / cancel)、并行 tool call 编排、错误如何回灌模型(失败让模型自愈还是中止)、每步后是否触发 compaction(agent loop 是 compaction trigger 的天然求值点——正好在轮次边界)。

**loop 不是"单个 async fn",而是步进模型。** 抽象:
```rust
#[async_trait]
trait AgentLoop {
    async fn feed(&mut self, input: Input) -> ResultAsyncStream;  // 一次一段,stream 消费完才 feed 下一个
    fn interject(&self, msg: PivotMessage) -> Result<()>;         // 软转向,下个 step 边界生效
    // cancel 走通用 CancellationToken,不单列
}
```
关键点:
- **一次 feed 通常跨多个 LLM 轮次**(call→tool→call…),stream 把整段自主推进作为事件流吐出。它统一了"loop 多步推进"与"LLM streaming"。
- **背压即节奏控制**:"stream 消费完才能 feed 下一个" = 用 stream lifetime 强制"一次只有一段活跃推进",无需额外锁。**保留成硬规则。**
- stream 里流动的是 `AgentEvent`(包住并超出 `StreamEvent`):
```
AgentEvent =
  | Llm(StreamEvent)                              // 透传下层 text/thinking delta
  | StepBoundary(Boundary)                        // 轮次边界:trace/预算/compaction/pivot 生效点
  | ToolCallStarted { call } / ToolCallFinished { result }
  | AwaitingApproval { call, respond: Responder }  // stream 就地挂起等外部回灌
  | Done(Outcome)                                  // 这一段为何结束
```

**三种外部干预的分工**(扩展 `feed→stream` 的双向性):
- **审批(human-in-loop)**:loop 主动停 → `AwaitingApproval` + responder,**stream 挂起而不结束**,回灌后继续。
- **pivot(用户中途改向)**:外部主动插 → `interject`,**不立即打断当前 LLM 调用**,在下一个 `StepBoundary` 并入 conversation 生效(软转向,不违反 tool 配对)。
- **cancel**:外部主动停 → `CancellationToken`(硬,立即;闭合裂缝见 Cancel 一致性)。

#### Layer B —— 多 agent 编排(Orchestration)
**不发明大而全的编排引擎**,只提供最小原语:"把 agent 当可 await 的任务 spawn / 传消息 / 收结果",让编排用普通 Rust(tokio task / channel / join)写。多 agent 拓扑发散(委派 sub-agent、pipeline、group/swarm),过早抽象必选错。**先把 Layer A 做扎实,B 只给原语。**

#### 垂直/横切功能(穿透 A/B)
| 功能 | 关注点 |
|---|---|
| 预算/配额 | token / 成本 / 步数 / wall-clock 上限;每步检查,超限中止 |
| Cancellation | `CancellationToken` 贯穿 loop / streaming / tool / 子 agent |
| Observability / trace | 一次 run 是一棵树:run→step→llm call / tool call / sub-agent,要能重建 |
| State 持久化 & 恢复 | run 可中断续跑,存 conversation + loop 游标 + 预算余量 |
| Human-in-the-loop / 审批 | 某些 tool 需人工确认;loop 一等状态,不是事后加 |
| Hook / 中间件 | before-llm / after-response / before-tool / after-tool 等点插逻辑 |
| 完成判定 | 何为"任务完成":stop reason / 特定 tool(如 `finish`)/ schema 校验 |

两个必须让 loop 从一开始支持的:**(1) 可暂停/可恢复**(审批、外部等待、中断续跑本质都是"loop 停在某点、序列化、之后恢复");**(2) 统一 step 边界 hook 点**(预算/compaction/审批/trace/注入都发生在同样几个点,别各挖各的洞)。

#### 对下层的约束(downward constraints — 本层最重要的产出)
指导下层现在就预留对的钩子:
1. **Client**:`stop_reason` 归一化要足够细(至少 `tool_use`/`end_turn`/`max_tokens`/`refusal`),agent loop 的终止与分支完全依赖它。
2. **Client**:usage 每步可得(含流式最终 usage),否则预算无法逐步核算。
3. **Conversation**:必须暴露**"轮次完成"边界事件**——它同时是 compaction trigger / 预算检查 / trace step 边界 / loop 可暂停点 / pivot 生效点。`Boundary` 概念要能被 agent 层复用(不只服务 revert/fork)。**revert/fork、compaction、cancel、agent loop 四者共用同一地基。**
4. **Conversation + Tool**:tool **发起在 agent 层、配对记账在 conversation 层**。agent loop 决定"执行哪些 call / 并行还是串行 / 失败怎么办",conversation 负责"call_id ↔ result 记账"。接口划清。
5. **Tool**:ToolResponse 要能表达"需审批 / 被拒绝 / 执行出错"等**非正常结果**,不只成功值(human-in-loop 与错误自愈都读它)。
6. **全局**:`CancellationToken` + 预算等 **run 级上下文**要有能贯穿三层往下传的载体(类似 `RunContext`)。现在不实现,但下层关键 async 接口签名要**预留接收位置**,否则以后到处改签名。
7. **Conversation 事务性推进**:committed log 永远满足不变量;turn 产物先入 pending,仅在 `Boundary` commit;cancel 只影响 pending(见 Cancel 一致性)。
8. **Cancel 闭合裂缝**:默认给未应答 tool_use 补 `cancelled` 合成 tool_result;partial message 丢弃或闭合。**"cancel 后 conversation 仍可 feed" 是硬性验收标准。**
9. **`feed` 的 stream 能"挂起而不结束"**:审批点要求 stream 停在中途等回灌;tool 执行编排要支持"某 tool call 获批前不启动"。
10. **`interject` 要求 conversation 支持"在边界注入 message"**:注入后仍满足 role 序列合法与 tool 配对。conversation 要预留此写入口。

## 2. 垂直组件

### 2.1 Tool Execution
区分两类性质完全不同的工具,**不塞进同一个执行回调抽象**:

- **ProviderTool(声明式)**:web search 等由 API 端执行的工具。用户只声明启用,结果随 response 返回。本质是 provider capability。
- **LocalTool(执行式)**:由本框架执行,提供执行回调。需考虑:异步执行、并行 tool call、执行超时、错误如何回传给模型、取消/中断。

统一的 `ToolCall` / `ToolResponse` data model:
- ToolResponse 也是多模态的(可返回图片),需支持 content block。

### 2.2 Tool Registry
负责 tool 注册与 name 映射,并管理:
- JSON schema 生成(最好能从 Rust 类型派生,类似 `schemars`)
- name 冲突 / 命名空间(multi-agent 下多个 agent 各带工具)
- 按 provider 方言序列化 schema(格式有差异)

## 3. 基础技术取向

| 取向 | 决定 | 理由 |
|---|---|---|
| async 运行时 | **tokio** | 最流行,生态最好 |
| async trait | **一律 `#[async_trait]`** | 全 trait dyn-safe,不与 AFIT 的 object safety / Send 边界较劲。LLM 调用是数百 ms~秒级网络往返,boxing 开销可忽略;程序好写、provider 可运行时切换更重要。不做混合方案。 |
| 序列化 | **数据 serde,运行时资源不 serde** | 见下 |

### 序列化边界
分界线:**"数据"序列化,"行为/运行时资源"不序列化。**
- **必须 serde**:所有 wire data model(message / content block / tool call / response / usage)、所有 config。理由:持久化、调试回放、与 provider JSON 互转。
- **不需要 serde**:registry、name mapping、client handle、tool 执行回调等运行时对象(持有闭包、连接、`Arc`)。

## 4. 逃生舱(Escape Hatch)

逃生舱要有,但不能是 `serde_json::Value` 一把梭(等于放弃归一化)。分成三种性质不同的机制,各管一段:

| | 方向 | 谁填 | 约束 | 优先级 |
|---|---|---|---|---|
| (A) `ProviderExtras` | 请求 | 用户 | 绑定 `ProviderId` | 按需再加 |
| (B) flatten `extra` | 响应 | serde 自动 | 归一化字段优先,其余兜底 | 先做 |
| (C) `Normalized<T>` | 响应 | 框架映射 | value 是枚举,raw 留证据 | 先做 |

### (A) ProviderExtras —— 入站方言(请求侧)
调用方想塞框架未建模的请求参数(某厂商特有的 `top_k`、`safety_settings` 等)。

```rust
pub struct ProviderExtras {
    provider: ProviderId,          // 约束:声明给谁的,发错 provider 时可丢弃/报错
    fields: Map<String, Value>,    // 只在序列化最后一步 merge,不进核心模型
}
```
**约束**:必须绑定 `ProviderId`,方言被隔离在明确标注归属的口袋里,核心模型保持干净。先不做,等真有调用方需要时再开口子,避免过早开洞。

### (B) flatten extra —— 出站未知字段(响应侧)
provider 返回了未建模的字段。**框架自动兜底**,不让用户手填:

```rust
#[serde(flatten)]
extra: Map<String, Value>,
```
用户想读就读,不读不影响归一化字段。零心智负担、纯防御性。

### (C) Normalized\<T\> —— 带出处的归一化值(响应侧)
归一化字段旁保留原始值,用于枚举归一化场景(`stop_reason`、`role`、tool 状态等):

```rust
pub struct Normalized<T> {
    pub value: T,              // 归一化后的枚举/值
    pub raw: Option<String>,   // provider 原始值;映射不上时 value=Unknown 也保留证据
}
```
示例:`stop_reason: Normalized<StopReason>`,`value = ToolUse`、`raw = "tool_use"`;遇到没见过的 `raw = "some_new_reason"` 时 `value = Other`,不丢信息也不崩。

## 5. Client 层已定约束

### Capability(结构化,非布尔标志)
`supports_multimodal: bool` 这类很快不够用。改为结构化描述:
- `max_context_tokens`
- 支持的 input / output modality 集合
- 是否支持:streaming、tool calling、并行 tool call、prompt caching、reasoning/thinking、结构化输出(JSON schema)
- 支持的 stop reason 类型集合

**来源**:默认表(硬编码) + 可覆盖(用户 config)。运行时探测按需。

### 统一 Error Model
分类:限流(429 + retry-after)、超时、context 超限、内容被拒、网络错误、协议解析错误。retry/backoff 策略依赖此分类。

### Usage / Token Accounting
每次响应的 input / output / cached / reasoning token,是 Response 的**一等公民**(成本控制与 context 管理的基础)。

### ContentBlock
抄成熟设计(以 Anthropic 分类为参考,最干净):`text` / `image` / `tool_use` / `tool_result` / `thinking`。多模态承载需明确:图片 URL vs base64、audio、文件/PDF、citation。message content 是 `Vec<ContentBlock>` 而非 `String`。

不自己发明轮子,唯一要额外用心的是 streaming(见下)。

### Streaming
核心矛盾:`ContentBlock` 是**完整态**,streaming 是**增量态**(block start → 一串 delta → block stop)。需三个层次,不要混:

```
1. ContentBlock        完整态(存进 conversation、序列化、给 tool 用)
2. ContentBlockDelta   增量态(流式事件传输的碎片)
3. StreamEvent         归一化的流事件流(统一两家原始事件)
```

**三条纪律:**
1. **delta 必须带 `index`**:多个 block 可能交替推进(尤其并行 tool call),`{ index, delta }` 才能把碎片拼回正确的块(照抄 Anthropic 的 `content_block_delta`)。
2. **tool_use 参数是流式 JSON 字符串,累积完再 parse**:`input` 流式时是 JSON 文本片段(Anthropic `input_json_delta`,OpenAI response 的 `arguments` delta)。**不能边流边 parse**,须累积完整字符串,`block stop` 后一次性 parse。ContentBlock 的 tool_use input 需区分"未完成 partial string"与"完成后 `Value`"。
3. **流可折叠回完整 Response**:提供 `Accumulator`,内部维护 `HashMap<index, PartialBlock>`,吃 `StreamEvent` 吐完整 `Response`。两家适配器各自把原始 SSE 翻译成统一 `StreamEvent`,Accumulator 逻辑只写一份。

**归一化 StreamEvent 形态**(两家的最小公共上界):
```
StreamEvent =
  | MessageStart { role, ... }
  | BlockStart   { index, block_type }
  | BlockDelta   { index, delta }        // delta = Text | InputJson | Thinking | ...
  | BlockStop    { index }
  | Usage        { ... }                 // 通常末尾,也可能中途
  | MessageStop  { stop_reason }
  | Error        { ... }
```
OpenAI response 的 `response.output_item.added` / `.delta` / `.done` 可映射到 BlockStart/Delta/Stop,仅字段名不同(适配器负责)。

**两种消费姿势:**
```rust
// A:实时增量(TUI 打字机、边想边显示)
stream: impl Stream<Item = Result<StreamEvent>>
// B:只要最终结果(框架内部跑 Accumulator 折叠)
async fn collect(stream) -> Result<Response>   // 含完整 Vec<ContentBlock> + usage + stop_reason
```

## 6. 横切层(纳入框架)

| 关注点 | 说明 |
|---|---|
| **Observability / tracing** | 从第一天起就要有结构化 trace:每次 LLM 调用的 request / response / usage / 延迟。agent loop 调试依赖它。 |
| **Retry / 限流 / backoff** | 生产必备,依赖统一 error model。 |
| **Cancellation** | `CancellationToken`;agent loop、streaming、tool 执行都要能被打断。 |
| **Cost / token 预算** | 依赖 usage accounting。 |
