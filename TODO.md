# TODO：Conversation Core 实现任务列表

> 依据 [`PLAN.md`](PLAN.md) 与规范性设计
> [`docs/conversation-core.md`](docs/conversation-core.md)。任务按真实依赖顺序编号；
> coding agent 每次只执行首个标题未带 `[DONE]` 的任务，完成后把该标题的 `[TODO]`
> 改为 `[DONE]` 并补充完成记录。已完成的 Client 层任务与验证记录归档在
> [`docs/archive/2026-07-13-client-layer/TODO.md`](docs/archive/2026-07-13-client-layer/TODO.md)。

通用约束：不得公开能直接修改 closed history 的裸容器或 unchecked 构造器；不得用
`extra`、provider 特判或任务私有状态绕过规范缺口；id/时间由调用方注入；每个测试用例
必须在 1 分钟内完成。每项的完整验证均按“format → 严格 clippy → 聚焦测试 → 全量测试
→ rustdoc → diff check”的顺序执行，全量测试最长 30 分钟。

---

## Milestone 1 — 不可变已提交核心

### M1-1 [DONE] Conversation 模块、强类型 identity 与不可变消息 envelope

**前置依赖**：Client 层 M1--M6 已完成；直接复用现有 `model::message::Message`，不得给
Client `Message` 回填 Conversation id。

**上下文**：`docs/conversation-core.md` §1/§7 要求 message 冻结后永不改变、id 在创建时
由外部注入且可作为稳定 PK。现有 Client `Message` 有完整 payload 但故意不含 id，因此
需要 Conversation 自己的 envelope，而不是修改 provider-neutral wire 模型。

**做什么**：

- 新建 `src/conversation/` 并从 `lib.rs` 导出；按 `PLAN.md` 建立聚焦模块，避免把核心状态机
  堆入一个长文件。
- 引入只提供解析/serde 能力的 UUID 依赖；定义互不混淆的 `ConversationId`、`TurnId`、
  `MessageId`、`ToolCallId`、`ArtifactId` newtype。构造函数只接收外部值，不调用 RNG、
  时钟或数据库自增；文档约定生产调用方提供 UUIDv7/等价稳定 id。
- 定义字段私有的 `ConversationMessage { id, payload: Message }`，只提供构造、只读 getter
  和消费式拆分；不提供 `DerefMut`、`payload_mut` 或用同一实例原地改内容的 API。
- 定义可 serde 的 `ConversationConfig`，至少单列 `system: Option<String>`；system 不得作为
  closed Turn 中的 `Role::System` message 混入历史。
- 所有新增公共类型、模块和函数补齐 rustdoc；serde 表示固定且不混淆不同 id 类型。

**验证**：

- 聚焦测试覆盖全部 id 与 `ConversationMessage`/`ConversationConfig` serde round-trip、不同
  newtype 不能误用的编译期 API 边界，以及构造过程不隐式生成 id/时间。
- 断言冻结 envelope 只暴露 `&Message`，Client `Message` 仍不含 `MessageId`，system 配置
  不进入 payload role 序列。
- 依次通过 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、聚焦测试、
  `cargo test --all --all-targets`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` 和
  `git diff --check`。

**完成记录（2026-07-13）**：

- 新增 `conversation::{id, config, message}` 聚焦模块并从 crate root 导出；五种私有字段
  UUID newtype 只接受调用方提供的值，统一支持 canonical string serde、解析、只读访问、
  比较/哈希与消费式取回，依赖未启用 UUID 生成、RNG 或时钟 feature。
- 新增字段私有的 `ConversationMessage`，公共 API 仅含构造、Copy id getter、
  `payload() -> &Message` 与 `into_parts`；新增独立持有可选 system prompt 的
  `ConversationConfig`，未修改 Client `Message`，也未把 system 合成为历史 payload。
- 补齐模块/API rustdoc、README 当前能力与用法；单元测试覆盖五类 id 和 config/envelope
  serde、外部 id 原样保留、非法 UUID、只读/消费边界、Client Message 无 id 及 system
  分离，两个 compile-fail doctest 验证不同 id 不可互换和 payload 不可经 getter 原地修改。
- 验证通过：`cargo fmt --all`；`cargo clippy --all-targets -- -D warnings`；
  `cargo test conversation`（9 passed）；`cargo test --doc conversation`（2 passed）；
  `cargo test --all --all-targets`（142 passed、7 ignored、0 failed）；
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`；`git diff --check`。

### M1-2 [DONE] Closed `Turn`、`ToolPairing` 与外部元数据

**前置依赖**：M1-1。

**上下文**：Turn 是公开切割、不变量和 parent-tree 的最小单位；`conversation.turns` 只能
保存完整 closed Turn。tool pairing 要同时保留框架 `ToolCallId`、provider call id 及调用/
结果 message id，不能从 provider id 推断内部 identity。

**做什么**：

- 在 `conversation/turn.rs` 定义字段私有、只读的 `Turn`、`TurnMeta`、`ToolPairing`；
  `Turn` 包含 `TurnId`、有序 immutable messages、全部配对、`parent: Option<TurnId>` 与 meta。
- `ToolPairing` 保存 `call_id`、`provider_call_id: Option<String>`、`call_msg` 和
  `result_msg`。pending 阶段可以内部表达 `result_msg == None`，但 closed `Turn` 的公共只读
  视图必须保证每项都有结果。
- `TurnMeta` 至少承载聚合 `Usage`、由外部提供的可选时间戳/来源和可扩展数据字段；模型
  内部不得调用 `now()`。明确 meta/annotation 不能用来覆盖或修改历史 message payload。
- messages 使用共享只读所有权，`Turn` 不提供公共裸构造器或 `messages_mut`；先建立
  crate-private draft/builder 边界，最终 closed 构造留给 M1-3 的唯一校验门。
- 为所有类型定义确定性相等性、只读访问和 serde data shape，避免序列化锁或运行时引用；
  live `Turn` 不得通过 unchecked derive `Deserialize` 绕过 closed 校验，可先使用内部
  `TurnData` DTO，并在 M1-3 通过 validator 实现受检反序列化。

**验证**：

- 聚焦测试覆盖多 message Turn、parallel tool pairing、parent/meta 与 serde round-trip；
  断言共享 message 的 id/payload 在读取 Turn 后不变。
- 编译/API 审查确认外部调用方不能构造“closed 但 `result_msg == None`”的 Turn，也不能在
  Turn 内替换 message。
- 依次通过 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、聚焦测试、
  `cargo test --all --all-targets`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` 和
  `git diff --check`。

**完成记录（2026-07-13）**：

- 新增并导出字段私有的 `Turn`、`ToolPairing` 与 `TurnMeta`；`Turn` 通过
  `Arc<[ConversationMessage]>`/`Arc<[ToolPairing]>` 共享有序只读数据，仅提供 id、message、
  pairing、parent 与 meta getter，不提供 public 构造、裸容器、mutable getter 或替换 API。
- closed `ToolPairing.result_msg` 固定为非可选 `MessageId`，同时独立保留框架
  `ToolCallId`、可选 provider call id 和 call/result message id；`TurnMeta` 承载聚合
  `Usage`、调用方提供的可选时间戳/来源及独立嵌套扩展字段，不读取时钟也不能覆盖历史
  payload。
- 新增 crate-private `TurnData`/`ToolPairingData`，作为 draft 与 serde DTO 可表达 pending
  的 `result_msg: None`；live `Turn` 只单向序列化到同一稳定 data shape，未实现 unchecked
  `Deserialize` 或 DTO→live 构造，受检转换明确留给 M1-3 唯一 validator。
- 聚焦测试覆盖 4-message Turn、两个 parallel tool pairing、parent/meta、稳定 DTO serde
  round-trip、closed pairing 缺失/null result 拒绝、Arc pointer sharing 及读取前后 message
  id/payload 不变；三个 compile-fail doctest 分别钉住消息不可替换、live Turn 不可直接
  deserialize、外部不可 raw construct。
- 验证通过：`cargo fmt --all`；`cargo clippy --all-targets -- -D warnings`；
  `cargo test conversation::turn`（6 passed）；`cargo test --doc`（1 个正向与 5 个
  compile-fail passed）；`cargo test --all --all-targets`（145 个库单测与 3 个离线集成测试
  passed、7 ignored、0 failed）；`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`；
  `git diff --check`。

### M1-3 [DONE] I1--I4 validator 与原子 `commit` 门

**前置依赖**：M1-2。

**上下文**：closed history 的正确性不能依赖调用约定。唯一合法推进路径必须先验证
I1 tool 配对、I2 role 序列、I3 无 partial、I4 id 唯一；失败时不得留下半个 Turn 或污染
索引。

**做什么**：

- 定义分类化 `ConversationError`/`CommitError`，错误至少区分重复 Turn/Message/ToolCall id、
  duplicate/orphan/dangling provider call、非法 role/block、非法首尾状态、未完成 content、
  parent 不匹配和非原子提交。
- 实现 canonical Turn 状态机：恰好从一条外部 `Role::User` message 开始；assistant 只能
  出现在 user 或全部 tool results 之后；只有 assistant 可含 `ToolUse`；只有 tool role 可含
  `ToolResult`；parallel calls 可由多条 tool message 回答，但必须恰好一次全部闭合；最后
  一条必须是不含 tool use 的 assistant。closed Turn 禁止 `Role::System`。
- 逐个对照 content 中的 provider call id 与显式 `ToolPairing`，验证 call/result message id
  指向正确 block，拒绝重复、孤儿、跨 Turn 配对与同一 result 多次消费。
- I3 由完整 Client `Message` 类型和受检冻结路径共同保证；validator 仍须拒绝内部 draft
  中未结束的 pending 标记，不能把 partial JSON 伪装为 `Value::Null`。
- 实现 `Conversation` 的空实例与 crate-private draft commit：在临时状态上完成全部校验后
  一次性生成 closed `Turn`、设置正确 parent 并推进 history/version；任何错误保持原对象
  全结构不变。
- 将 `TurnData`/serde 输入接入同一 validator；无论来自内存 draft 还是反序列化，都不能
  构造违反 I1--I4 的 live `Turn`。

**验证**：

- 正向测试覆盖纯文本、单次 tool、多个串行 round-trip 和 parallel tool calls；每次 commit
  后逐项断言 I1--I4。
- 负向表驱动测试覆盖每类 duplicate/orphan/dangling、wrong role/block、未回答 parallel
  call、带 tool use 的最终 assistant、system role、重复 id 和错误 parent；对失败前后
  Conversation 做全结构相等断言。
- 依次通过 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、聚焦 validator/
  commit 测试、`cargo test --all --all-targets`、
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` 和 `git diff --check`。

**完成记录（2026-07-13）**：

- 新增公开只读 `Conversation`、分类化 `ConversationError`/`CommitError` 与 crate-private
  `commit_draft`；外部 id/config 原样注入，history/version 字段私有。commit 先计算下一
  version 并在临时数据上完成全部校验，成功后才同时追加 live `Turn` 与推进 version；
  所有 validation、parent 及 version exhaustion 错误路径均保持 Conversation 全结构相等。
- 新增唯一 `validation` 门并按 identity/completion、role/content sequence、pairing 三个模块
  拆分。canonical 状态机要求 user 起始、assistant tool-use 后由一条或多条 tool message
  恰好一次闭合全部 parallel calls、允许任意串行 round-trip，并以无 tool-use assistant
  结束；同时拒绝 system、两家 adapter 公共子集之外的 role/block 及非法 tool-result
  nested content。
- validator 对 Turn/Message/ToolCall id 做 conversation-wide I4 校验，并将每个 provider
  tool-use/result block 与显式 pairing 双向核对，分类拒绝 duplicate/orphan/dangling、
  result 重复消费、未知/跨 Turn reference、错误 message anchor 与错误 parent。可选
  `provider_call_id: None` 仅在 call/result anchors 唯一确定同一 content id 时接受并原样
  保留，歧义时拒绝，不靠顺序猜测。
- `TurnData` 新增 closed serde 默认省略的显式 completion marker；pending marker 即使把
  partial tool JSON 写成 `Value::Null` 也会被拒绝，已完成且语义合法的 JSON null 仍可
  提交。内存 draft 与反序列化 DTO 共用同一 validator-issued certificate；`Turn` 只消费
  该不可由 sibling 构造的 certificate，旧 Turn fixture 也移除直接字段构造。
- 测试按 fixture/positive/negative/atomic 及负例子域拆分；正向覆盖纯文本、单次、多个串行、
  多消息 parallel、optional provider id、serde 与连续 parent commit，并从只读 live Turn
  逐项复核 I1--I4。表驱动负例覆盖任务指定的全部错误类，且每例都断言失败前后对象全结构
  相等；compile-fail doctest 钉住外部不能 raw push closed history。
- 验证通过：`cargo fmt --all`；`cargo clippy --all-targets -- -D warnings`；
  `cargo test conversation`（33 passed、0 failed，1 个真实 endpoint 测试 ignored）；
  `cargo test --all --all-targets`（163 个库单测与 3 个离线集成测试 passed、7 ignored、
  0 failed，30 分钟硬上限内完成）；`cargo test --doc`（1 个正向与 6 个 compile-fail
  passed）；`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`；`git diff --check`。

### M1-R [DONE] Milestone 1 Review

**前置依赖**：M1-1 至 M1-3 全部完成。

**上下文**：M1 首次建立后续所有状态转换都依赖的 closed data boundary；若 immutable/API
封装或 validator 留洞，pending、fork 与 restore 都会把同一缺陷放大，因此必须独立审阅。

**做什么**：

- 对照 `docs/conversation-core.md` §1--§4 审查 identity 分层、message immutability、Turn
  边界哲学和 I1--I4；确认 Client `Message` 仍 provider-neutral 且无 id。
- 审查所有 closed 类型的字段可见性，确认不存在 raw push、unchecked closed Turn、可变
  message getter 或绕过 commit validator 的 serde/public API。
- 核对 canonical role/tool 状态机能被两家 Client adapter 表达；列出 M2 pending 所需的
  唯一 crate-private draft 接口，不提前暴露 partial。

**验证**：

- 运行全部 M1 聚焦测试并人工逐项映射 I1--I4；公共 API rustdoc 无缺口或模糊承诺。
- 依次通过 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、
  `cargo test --all --all-targets`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` 和
  `git diff --check`。

**完成记录（2026-07-13）**：

- 对照规范 §1--§4 完成 identity、immutable envelope、closed Turn 与 I1--I4 信任边界审计：
  Client `Message` 仍只有 provider-neutral role/content；五类 Conversation id 互不混淆且只
  接受外部值；system 独立保存在 config；所有 live Turn/message/meta getter 都是只读视图，
  public API 不存在 raw push、unchecked Turn constructor、`Deserialize` 或 mutable payload。
- 人工映射确认 I1 由 role sequence facts 与显式 pairing 双向核对，I2 由 canonical
  user→assistant→tool*→assistant 状态机保证，I3 由完整 Client 类型与 `TurnCompletion` 门保证，
  I4 由 conversation-wide Turn/Message/ToolCall identity 检查保证；所有失败在修改
  history/version 前返回，原子性测试逐例比较全结构。
- M2 可复用的唯一 crate-private draft 边界明确为 `turn::TurnData`（含
  `ToolPairingData`/`TurnCompletion`）和 `Conversation::commit_draft`；draft 只能提交到唯一
  `validation::validate_turn_data`，live `Turn` 还要求 sibling 无法构造的
  `ValidatedTurnData` certificate，未提前公开 partial 或第二条物化路径。
- 新增 Review 聚焦回归：把同一个经 validator 提交、覆盖 user text/image、assistant
  text/thinking、parallel tool use、多条 tool result 与多模态结果的 canonical Turn 交给
  Anthropic/OpenAI Responses 两家离线 request mapper；两者均成功并保持 system 单列、
  call/result 数量一致和源 payload 不变。同步修正两处把已完成 M1-3 写成未来工作的
  serde/validator rustdoc。
- 验证通过：`cargo fmt --all`；`cargo clippy --all-targets -- -D warnings`；
  `cargo test conversation`（34 passed、0 failed）；`cargo test --all --all-targets`
  （164 个库单测与 3 个离线集成测试 passed、7 ignored、0 failed，1800 秒硬上限内完成）；
  `cargo test --doc`（1 个正向与 6 个 compile-fail passed）；
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`；`git diff --check`。

---

## Milestone 2 — Pending 与 Cancel

### M2-1 [DONE] Tool result 完整状态模型前置修复

**前置依赖**：M1-R。

**上下文**：cancel 规范要求合成 `status=cancelled` 的 tool result。当前
`ToolResponse.status` 支持 `Ok/Error/Denied/Cancelled`，但消息中的
`ContentBlock::ToolResult` 只有 `is_error: bool`，会在持久化时丢失 Denied/Cancelled；
把状态塞进 `extra` 属于被禁止的 workaround。

**做什么**：

- 将 provider-neutral `ContentBlock::ToolResult` 改为以 `ToolStatus` 为唯一权威状态；消除
  可产生 `status`/`is_error` 矛盾的双事实来源。为 `ToolResponse` 与 tool-result block 提供
  明确的无损转换。
- 若需要兼容已有持久化 JSON，使用集中式 serde migration 接受旧 `is_error` 表示并映射到
  `Ok/Error`，冲突输入必须报错；新的 normalized 序列化只输出一个权威状态字段。
- 类级更新 Anthropic/OpenAI request mapper、response parser、fixtures、examples 与所有
  pattern match：Anthropic wire 的 `is_error`、OpenAI wire 的 completed/incomplete 只是
  adapter 映射，`Denied/Cancelled` 在 Conversation 数据中仍原样保留。
- response wire 无法区分 Denied/Cancelled 时只能归一为可证实的 `Error`，不得虚构更具体
  状态；provider 原始证据仍按现有 escape hatch 保留。

**验证**：

- 测试四种 `ToolStatus` 的 normalized serde 与 `ToolResponse` 转换；旧 `is_error` migration
  正反例及冲突拒绝；两家 request body 映射和真实 fixture 解析全部更新。
- 回归断言现有非流式、流式、tool round-trip 和跨 provider 测试无语义退化。
- 依次通过 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、model/adapter 聚焦
  测试、`cargo test --all --all-targets`、
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` 和 `git diff --check`。

**完成记录（2026-07-13）**：

- `ContentBlock::ToolResult` 现以四态 `ToolStatus` 为唯一权威结果；normalized JSON 始终输出
  `status`，其专用序列化边界会过滤 `extra` 中伪造的 modeled/legacy 键。集中式反序列化
  migration 将旧 `is_error=false/true` 映射为 `Ok/Error`，也兼容旧成功记录省略默认 false；
  等价双字段会被 canonicalize，矛盾双字段及 present-null/错误类型均明确拒绝。
- `ToolResponse` 补齐默认可省略的 result metadata，并实现到 `ContentBlock::ToolResult` 的
  消费式 `From` 与受检反向 `TryFrom`；call id、多模态 content、四态 status 和 extra 全部
  无损往返，非 result block 的分类错误会归还原 block。现有 message、Conversation fixture、
  normalization scenario 与 `tool_round_trip` 示例全部改用显式 `ToolStatus`，不再手工压成布尔。
- Anthropic request mapper 仅在最终 wire 边界把 `Ok` 映射为省略 `is_error`、其余三态映射为
  `is_error=true`；OpenAI Responses 把 `Ok` 映射为 `completed`、其余三态映射为
  `incomplete`。四态表驱动测试同时确认 modeled 字段覆盖 extra 且源 block/message 不变；
  两家 assistant response parser/真实 fixtures 不产生本地 tool-result，完整 adapter 回归确认
  text/thinking/tool-use 非流式与流式解析没有语义退化，旧 wire 布尔只会迁移为可证实的
  `Ok/Error`，不会虚构 `Denied/Cancelled`。
- 验证通过：`cargo fmt --all`；`cargo clippy --all-targets -- -D warnings`；
  `cargo test model::`（38 passed）；`cargo test adapter::`（60 passed）；
  `cargo test --all --all-targets`（174 个库单测与 3 个离线集成测试 passed、7 ignored、
  0 failed，1800 秒硬上限内完成，三个 example target 均编译）；`cargo test --doc`
  （1 个正向与 6 个 compile-fail passed）；
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`；`git diff --check`。

### M2-2 [DONE] `PendingMessage` 与 stream/non-stream 冻结边界

**前置依赖**：M2-1。

**上下文**：`PendingMessage` 不是 `Message`。它可以含未闭合 block 或 partial tool JSON，
只能在 Client `Accumulator` 成功 finish 后由外部提供 id 冻结；失败或 cancel 时 partial
必须可整体丢弃。

**做什么**：

- 在 `conversation/pending/message.rs` 定义非 serde 的 `PendingMessage`，内部持有唯一 Client
  `Accumulator`、生命周期状态和必要的响应 metadata；不复制 Accumulator 折叠逻辑。
- 提供按顺序 `push(StreamEvent)` 的受检入口；Client 的 block id、tool JSON 完整边界和
  `AccumulatorError` 原样分类进入 Conversation 错误链。发生 terminal error 后禁止继续
  当作有效 message 使用。
- `finish(MessageId)` 仅在 MessageStart/所有 BlockStop/MessageStop 完整后生成 immutable
  `ConversationMessage`，并返回 response usage、stop reason/extra 供 Turn meta 汇总；id 只在
  成功冻结时绑定。
- 为非流式 `Response` 提供同一验证/冻结语义，保证 stream fold 与 complete response 进入
  pending 后得到相同消息形状；assistant tool JSON 必须已经是完整 `Value`。
- partial state 不实现 serde，不暴露为 Client `Message`；drop/cancel 不执行隐式 finish。

**验证**：

- 用交错 text/reasoning/tool stream 冻结出正确 message；同一 fixture 的 stream 与
  non-stream 路径全结构一致。
- 覆盖 partial JSON、缺 BlockStop/MessageStop、错误事件、finish 两次、terminal 后 push 和
  cancel/drop；所有失败均不产生 `MessageId` 或 closed message，且不 panic。
- 依次通过 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、pending message
  聚焦测试、`cargo test --all --all-targets`、
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` 和 `git diff --check`。

**完成记录（2026-07-13）**：

- 新增非 serde、不可克隆的 `conversation::pending::PendingMessage`；streaming 状态唯一持有
  Client `Accumulator`，non-streaming 状态持有完整 `Response`，二者共用同一个受检
  response→`FrozenMessage` 冻结门。成功前不构造 `ConversationMessage`、不绑定调用方
  `MessageId`，partial block/tool JSON 也没有任何完整 `Message` getter 或 serde 逃生口。
- `FrozenMessage` 以只读 getter/消费式拆分返回 immutable message、`Usage`、normalized stop
  reason 与 response `extra`，供后续 PendingTurn 汇总且不修改 payload。两条输入路径统一拒绝
  非 assistant response；交错 text/reasoning/tool fixture 证明 stream fold 与 complete response
  冻结结果全结构一致，完整 tool input 保持为 `serde_json::Value`。
- 新增 streaming/complete/terminal/frozen 显式生命周期；任何 `AccumulatorError` 或失败 finish
  都原子丢弃 accumulator 并进入 terminal，后续 push/finish 明确拒绝；成功后第二次 finish 与
  后续 push 也明确拒绝。`cancel(self)`/普通 drop 只消费 partial，不调用 finish、解析残缺 JSON
  或生成 identity。
- 新增 `PendingMessageError` 并接入统一 `ConversationError`；共享只读保存原始
  `AccumulatorError` 以维持既有错误 Clone/Eq 边界，同时手写标准 source 链，调用方可直接按
  block id、JSON/lifecycle 或 stream provider error 分类，下层 `ClientError` 仍可继续追溯。
- 聚焦测试拆分为 fixture/success/errors，覆盖交错 block、stream/non-stream 等价、metadata、
  partial JSON、缺 BlockStop/MessageStop、error event、terminal 后 push/finish、finish 两次、
  非法 role、complete-response 错误推进及 cancel/drop；两个 compile-fail doctest 钉住
  partial 不可 serde、不可读取成 Client `Message`。
- 验证通过：`cargo fmt --all`；`cargo clippy --all-targets -- -D warnings`；
  `cargo test conversation::pending::message`（9 passed）；30 分钟硬上限内
  `cargo test --all --all-targets`（183 个库测试与 3 个离线集成测试 passed、7 ignored、
  0 failed，全部 example test target passed）；`cargo test --doc`（1 个正向与 8 个
  compile-fail passed）；`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`；`git diff --check`。

### M2-3 [DONE] `PendingTurn` 事务推进与多轮 tool 记账

**前置依赖**：M2-2。

**上下文**：一个 feed 可包含任意多次 tool round-trip；同一时刻只有一个 PendingTurn 和
一条可变 PendingMessage。open tool calls 只在 pending 合法，最终 commit 必须复用 M1 的
validator。

**做什么**：

- 实现 `Conversation::begin_turn`：要求没有 pending，接收外部注入的 `TurnId`、
  `MessageId` 和合法 user payload；从当前 head 创建 `PendingTurn`，不提前写 raw history。
- 建立显式状态转换：开始 assistant pending → stream/non-stream 冻结 → 扫描 ToolUse；
  调用方必须为每个 provider call id 提供唯一 `ToolCallId` 映射，随后登记 open calls。
- 实现追加 tool response：只接受与 open call 匹配的 `ToolResponse`/完整 tool-result block，
  以外部 `MessageId` 冻结为 tool message；parallel calls 可分批返回，但同一 call 只能闭合
  一次，全部闭合前禁止开始下一条 assistant。
- assistant 无 ToolUse 时把 pending 标为 ready-to-commit；带 ToolUse 时必须继续 tool
  round-trip。usage 合并进 `TurnMeta`，provider call id 与内部 ToolCallId 都保留。
- `commit_pending(meta)` 复用 M1 唯一 validator 并原子追加 closed Turn；任何 transition/
  validator 错误保持 committed history 不变，并保留可取消或修复的明确 pending 状态。

**验证**：

- 正向覆盖纯文本、串行两轮 tool、两个 parallel calls 分两条 result message 回答、
  stream/non-stream 混合和 usage 汇总；commit 后 open_calls 为空且 I1--I4 恒真。
- 负向覆盖重复 begin、未知/重复 result、缺少 ToolCallId 映射、open call 未全闭合就开始
  assistant/commit、final assistant 又含 ToolUse、id 冲突；断言 raw history 原子不变。
- 依次通过 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、pending turn/tool
  聚焦测试、`cargo test --all --all-targets`、
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` 和 `git diff --check`。

**完成记录（2026-07-13）**：

- `Conversation` 现持有唯一、非 serde 的 `PendingTurn`，公开 `begin_turn`、stream/non-stream
  assistant 启动/事件推进/冻结、tool-call 映射、tool response/result 追加与
  `commit_pending`；begin 会校验外部 `TurnId`/`MessageId`、user role/content 和当前 parent，
  pending 全程不提前写入 closed history。只读视图公开 frozen messages、phase、usage、
  response metadata 与 tool-call 状态，不暴露 active accumulator、partial JSON、mutable getter
  或 raw push。
- 显式状态机覆盖 `AwaitingAssistant → AssistantInProgress → AwaitingToolCallMappings →
  AwaitingToolResults` 的任意多轮往返，并只允许无 tool-use 的 assistant 进入
  `ReadyToCommit`。`ToolCallMapping` 必须与刚冻结的 provider call id 完整一一对应，内部
  `ToolCallId` 在 whole conversation 唯一；parallel calls 可按任意顺序、分多条 immutable
  tool message 闭合，但全部闭合前不能启动下一条 assistant 或 commit。
- 新增分类化 `PendingTurnError`，覆盖重复 begin、非法 user、阶段错误、缺失/多余/重复 mapping、
  重复 provider/framework id、未知/重复 result、非法 result block/nested content 与无 pending；
  所有 mapping/result 预检先完成再修改 pending。`commit_pending` 从 ready pending 构造
  data-only `TurnData` 并复用 M1 唯一 validator，验证/parent/version 失败时 committed state
  与 pending 均原样保留，成功后才一次性追加 Turn、推进 version 并清空 pending。
- 每次冻结的 assistant usage 聚合进最终 `TurnMeta`；新增可 serde 的 `TurnResponseMeta`，按
  message 保存 normalized stop reason 与 response-level provider metadata，避免多轮 extra
  互相覆盖。`PendingMessage` 继续不可克隆，因此 `Conversation` 不通过 clone/shared mutable
  accumulator 绕过唯一可变区；旧 M1 原子性测试改为 committed 全结构 snapshot 比较。
- 实现按 lifecycle/tool bookkeeping 拆分为 `pending/turn.rs` 与 `pending/turn/tool.rs`；12 个
  聚焦测试再按 begin/mapping/results/identity/commit 与 success 拆分，正向覆盖纯文本、串行
  两轮 tool、两个 parallel calls 分消息返回、stream/non-stream 混合、usage/meta 汇总，负向
  覆盖任务指定的全部 transition、mapping、result、final tool-use 与 id 冲突，并证明错误后可
  修正继续 commit、closed Turn 仍满足 I1--I4。
- 验证通过：`cargo fmt --all`；`cargo clippy --all-targets -- -D warnings`；
  `cargo test conversation::pending::turn`（12 passed）；1800 秒硬上限内
  `cargo test --all --all-targets`（195 个库测试与 3 个离线集成测试 passed、7 ignored、
  0 failed，全部 example targets passed）；`cargo test --doc`（1 个正向与 9 个 compile-fail
  passed）；`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`；`git diff --check`。

### M2-4 [DONE] Cancel 裂缝闭合与“cancel 后仍可 feed”

**前置依赖**：M2-3。

**上下文**：cancel 只能影响 pending。裂缝 A 是冻结的 tool use 尚无 result，裂缝 B 是
活跃 message 含 partial blocks/JSON。处理后必须回到明确一致状态，committed Turn 不能被
修改或污染。

**做什么**：

- 实现分类化 `CancelDisposition`，至少支持：`DiscardTurn`（原子丢弃全部 pending，回到
  最近 boundary）、`ResumeTurn`（丢 partial、为全部 open calls 追加 cancelled results，
  保留自洽 pending 供后续 assistant 继续）、`CommitTurn`（在上述闭合后追加调用方提供的
  完整无 tool-use assistant 终态，再走唯一 commit validator）。不允许直接把以 tool role
  结尾的 draft 当作 closed Turn。
- cancel 首先 drop 活跃 `PendingMessage`，绝不尝试 parse/保留半截 JSON；
  `ResumeTurn`/`CommitTurn` 对每个已冻结 open call 生成含 `ToolStatus::Cancelled` 和明确
  interruption 内容的结果，使用调用方提供的 message id，保持 provider/internal call id
  配对；`DiscardTurn` 则原子丢弃整个 pending，无需制造随后又丢弃的结果。
- 所有 disposition 必须原子：合成 id 冲突、状态非法或 commit 校验失败时返回分类错误，
  committed history 不变且不得产生半闭合 closed Turn。
- `DiscardTurn`/成功 `CommitTurn` 后可立即 `begin_turn`；`ResumeTurn` 可开始下一条 assistant、
  最终 commit 后再 begin 新 Turn。Conversation 永不进入 poisoned 状态。

**验证**：

- 覆盖纯文本流中途 cancel、三段 tool JSON 中途 cancel、tool 执行期间 cancel、多个 parallel
  open calls cancel，以及无 pending cancel；断言 partial 不进入任何完整消息。
- 对三种 disposition 分别验证 open call 闭合/丢弃语义、`Cancelled` 状态持久保留、
  committed history 从未被改写，并执行后续完整 feed→commit 证明会话可用。
- 失败注入覆盖重复合成 id、缺 id、非法 final message；每次均检查原子性和 I1--I4。
- 依次通过 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、cancel 聚焦测试、
  `cargo test --all --all-targets`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` 和
  `git diff --check`。

**完成记录（2026-07-13）**：

- 新增聚焦的 `pending/cancel.rs` 与 data-only `cancel/prepare.rs`，公开
  `CancelDisposition::{DiscardTurn, ResumeTurn, CommitTurn}`、`CancelledToolResult`、
  `CancelOutcome`、分类化 `CancelError` 与 `Conversation::cancel_pending`。`DiscardTurn` 对任意
  pending phase 直接整体丢弃；Resume 与 Commit 对每个 frozen open call 要求调用方显式给出
  provider call id、稳定 `ToolCallId` 和 result `MessageId`，既能在 mapping 前原子建立
  pairing，也会拒绝改变已注册的 framework id。
- synthetic result 固定生成完整 tool-role message，nested text 明确记录执行被中断，唯一权威
  `ToolStatus::Cancelled` 原样保留；每个 open call 使用独立外部 message id。已正常完成的
  parallel result 不会重写，只闭合仍 open 的 calls；active streaming/complete/terminal
  `PendingMessage` 从不被 finish、parse 或暴露，成功 Resume/Commit 时随状态替换整体 drop，
  partial text/reasoning/tool JSON 不进入任何完整 message。
- Resume 在全部 mapping、缺失/多余项及 conversation-wide identity 预检完成后，才一次性追加
  cancelled messages、闭合 bookkeeping 并回到 `AwaitingAssistant`。Commit 使用独立完整
  `Response` 经过同一 `PendingMessage` freeze 语义，明确拒绝再次产生 ToolUse，汇总既有与最终
  response usage/metadata 后构造 data-only `TurnData`；只有唯一 `commit_draft` I1--I4 validator
  成功才清空原 pending。状态、freeze、identity 或 validator 失败均保留 pending 与 committed
  history，可用原 response/id 修正重试，不产生半闭合 Turn。
- 新增 14 个按 success 及 state/identity/final-response error 分组的 cancel 状态机测试：
  覆盖纯文本流与三段 tool JSON 中途 cancel、mapping 前 parallel calls、tool 执行中部分完成、
  三种 disposition、无 pending、
  ReadyToCommit、重复 provider id、缺失/重复/未知 synthetic result、既有/新 framework id、
  pending/committed message id 冲突、非法 final role/tool-use/content，以及 commit validator
  失败原子性；逐项断言 `Cancelled` 持久化、partial 不落盘、I1--I4 与后续 feed→commit/新 Turn
  可用。README 与 crate/module rustdoc 同步公共 cancel 语义和 typed resume 示例。
- 验证通过：`cargo fmt --all`；`cargo clippy --all-targets -- -D warnings`；
  `cargo test conversation::pending::turn::tests::cancel`（14 passed）；
  `cargo test conversation::pending`（35 passed）；1800 秒硬上限内
  `cargo test --all --all-targets`（209 个库测试与 3 个离线集成测试 passed、7 ignored、
  0 failed，全部 example targets passed）；`cargo test --doc`（1 个正向与 9 个 compile-fail
  passed）；`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`；`git diff --check`。

### M2-R [DONE] Milestone 2 Review

**前置依赖**：M2-1 至 M2-4 全部完成。

**上下文**：pending/cancel 是唯一允许瞬时不满足 closed Turn 不变量的区域，也是最容易把
partial 或 dangling tool call 泄漏进历史的边界，需要在进入分支功能前做完整状态机审计。

**做什么**：

- 对照规范 §5 审查 pending 是唯一可变区、同一时刻最多一条 PendingMessage、冻结只发生在
  complete boundary，且 Accumulator 逻辑没有在 Conversation 复制第二份。
- 核对 tool result 四状态从 Client model 到 pending/cancel/adapter 的全链路无损；确认不靠
  `extra` 表达 Denied/Cancelled。
- 对 cancel 的每个状态裂缝和 disposition 做不变量审计，确认 committed history 始终有效、
  任何 cancel 路径后都存在明确的继续/丢弃路径。

**验证**：

- 运行完整 pending/cancel 状态机矩阵，特别复核“cancel 后仍可 feed”和 partial JSON 永不
  落盘；公共错误与 rustdoc 足以让调用方选择正确 transition。
- 依次通过 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、
  `cargo test --all --all-targets`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` 和
  `git diff --check`。

**完成记录（2026-07-13）**：

- 对照规范 §5 完成 pending 信任边界审计：`Conversation` 只有一个私有
  `Option<PendingTurn>`，其互斥 lifecycle enum 只有 `AssistantInProgress` 能持有一条
  `PendingMessage`；streaming state 直接复用唯一 Client `Accumulator`，Conversation 没有复制
  block/partial JSON 折叠逻辑。stream/non-stream 共用 response→`FrozenMessage` 门，id 只在
  complete freeze 成功后绑定；active/terminal state 不 serde、不暴露完整 `Message`，pending
  只读视图仅含已经冻结的 messages、metadata 和 tool bookkeeping。
- 新增模块化 M2 Review 回归，对五种公开 `PendingTurnPhase` 分别执行 Discard/Resume/Commit，
  覆盖 15 个 phase/disposition 组合：Discard 在全部 phase 整体丢弃；Resume/Commit 在四种
  non-final phase 闭合 frozen open calls 或替换 active message；`ReadyToCommit` 的非法二次
  Resume/Commit 分类拒绝且 pending/history 原子不变。每条成功及拒绝后的合法路径都继续完成
  当前或下一次 feed→commit，直接证明 cancel 后无 poisoned state。
- 增加 terminal stream error 回归：active accumulator 在 partial text 后接收 provider error，
  cancel 不 finish/parse 该状态，partial 未进入 frozen messages；Resume 后 replacement assistant
  和后续新 Turn 均成功 commit。原有三段 partial tool JSON、parallel 部分完成、mapping 前后、
  identity/validator 失败重试矩阵一并通过，closed history 始终只经唯一 I1--I4 门推进。
- 用同一 parallel tool batch 贯穿四态链路：`ToolResponse` 写入 Ok/Error/Denied，cancel 合成
  Cancelled；pending、closed Turn 与 normalized serde 均保留唯一权威 `ToolStatus`，没有借用
  `extra`/legacy `is_error`。同一 immutable history 随后由 Anthropic/OpenAI request mapper
  分别降级为 `is_error` 与 `completed|incomplete`，provider wire 构造前后源 facts 完全不变。
  同步人工复核 public phase/error/cancel rustdoc，transition、修复/丢弃选择和 source 分类已足够
  明确，无需扩大生产 API；阶段顺序和完成标准未变，因此未修改 `PLAN.md`。
- 验证通过：`cargo fmt --all`；`cargo clippy --all-targets -- -D warnings`；
  `cargo test conversation::pending::turn::tests::review -- --nocapture`（5 passed）；
  `cargo test conversation::pending`（40 passed）；1800 秒硬上限内
  `cargo test --all --all-targets`（214 个库测试与 3 个离线集成测试 passed、7 ignored、
  0 failed，全部 example targets passed）；`cargo test --doc`（1 个正向与 9 个 compile-fail
  passed）；`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`；`git diff --check`。

---

## Milestone 3 — Boundary 与分支历史

### M3-1 [DONE] 结构共享 raw history 与派生 `ToolCallIndex`

**前置依赖**：M2-R。

**上下文**：O(1) fork、逻辑 revert 和 raw 永不丢要求 history 不是可变 `Vec` 的深拷贝。
同时 call index 只是一项可重建加速结构，不能成为 pairing 的事实来源。

**做什么**：

- 在 `conversation/history.rs` 实现内部持久化/结构共享 history abstraction，保存所有 raw
  `Turn` 节点、parent 指针、当前 active lineage 与有效 tip；可使用经过文档化的 persistent
  collection 或等价 Arc 节点结构，但 clone/fork 路径不得遍历或复制历史。
- 保留从已 revert head 分出的旧 suffix 节点；active lineage 改道时旧 raw Turn 仍能按 id
  读取用于调试/持久化，但不再自动进入当前有效视图。
- 实现派生 `ToolCallIndex`，可按 internal/provider call id 定位 call/result；index 从 closed
  turns + pending 重建，发生 head/branch 变化时只反映当前有效 lineage 和当前 pending。
- history 的唯一写入口继续是通过 validator 的 commit；重复 id 检查覆盖所有 retained raw
  节点，不能在隐藏旧分支上复用 MessageId/TurnId。

**验证**：

- 构造长历史并检查 history clone 共享节点、无 message/turn deep clone；追加新 Turn 不改变
  已有节点内容。
- 测试 index 构建/增量更新/全量重建等价，parallel/serial calls 均可定位；隐藏分支的 call
  不出现在当前有效 index，但 raw 记录仍存在。
- 依次通过 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、history/index 聚焦
  测试、`cargo test --all --all-targets`、
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` 和 `git diff --check`。

**完成记录（2026-07-13）**：

- 新增 crate-private `History`/`HistoryNode`/`RawHistory`：closed Turn 以 parent-pointer
  `Arc` 节点和持久化 raw entry 链保留，`Arc<Lineage>` + active prefix 单列当前有效视图与
  tip。history clone 只复制 `Arc` handle，不遍历或重识别 Turn/message；从较短 active prefix
  追加时生成新 lineage，旧 suffix 仍在 raw scope 中并可经只读 `raw_turn(TurnId)` 定位。
- `Conversation` 的 committed storage 已从可变 `Vec<Turn>` 切换到唯一 history abstraction；
  commit 仍先经 I1--I4 validator，再原子追加 immutable node。parent 取 active tip，而
  Turn/Message/ToolCall 重复检查、pending mapping 和 cancel 合成 identity 均扫描全部 retained
  raw nodes，因此隐藏旧分支不能复用 id，派生 index 也没有成为事实来源。
- 新增公开只读 `ToolCallIndex`/`ToolCallLocation`：可按全局框架 `ToolCallId` 或可能跨 Turn
  重复的 provider call id 定位 call/result message；支持从 current-lineage closed turns +
  pending 全量重建。正常 pending freeze/mapping/result/cancel 与 commit 路径只替换 pending
  suffix 或增量追加 committed calls；head/branch 测试重建后隐藏 suffix 不进入有效 index。
- 聚焦测试覆盖 128 Turn 长历史的 clone 节点共享与 append 前缀不变、改道后 raw suffix 保留、
  隐藏 Turn/Message/ToolCall identity 拒绝，以及 parallel + serial calls 从 unmapped pending
  到 committed 的逐 transition 增量/重建等价；另覆盖跨 Turn provider id 多结果、可选
  provider id 的受检 anchor 解析和 cancel Resume/Commit/Discard 同步。同步更新 README 与
  crate 文档；阶段顺序和完成标准未变，故未修改 `PLAN.md`。
- 验证通过：`cargo fmt --all`；`cargo clippy --all-targets -- -D warnings`；
  `cargo test conversation::history -- --nocapture`（6 passed）；1800 秒硬上限内
  `cargo test --all --all-targets`（220 个库测试与 3 个离线集成测试 passed、7 ignored、
  0 failed，所有 example targets passed）；`cargo test --doc`（1 个正向与 9 个 compile-fail
  passed）；`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`；`git diff --check`。

### M3-2 [DONE] 受检 `Boundary`、version 与 stale/ABA 防护

**前置依赖**：M3-1。

**上下文**：Boundary 不是裸 usize。它只能指向 Turn 边界，并且要拒绝跨 Conversation、
过期 version 和 pending 时的切割，防止同一位置在历史变化后被误接受。

**做什么**：

- 在 `conversation/boundary.rs` 定义字段私有的 `Boundary`，绑定来源 `ConversationId`、active
  lineage 位置/锚点和生成时 structural version；只由 `valid_boundaries`、
  `boundary_after(TurnId)` 返回。
- 明确定义起始 boundary（零 Turn）和每个 closed Turn 后 boundary；已逻辑 revert 时仍可为
  当前 active lineage 的未来 suffix 生成新 boundary 以支持 redo，但 fork child 不得越过其
  fork ceiling。
- 所有消费 Boundary 的操作统一校验 owner、version、锚点、范围和 pending 状态；结构变化
  后递增 version，使旧 Boundary 即使 index 再次相同也返回 `StaleBoundary`。
- `Boundary` 可作为 API token serde/debug，但反序列化值仍必须交给 Conversation 校验，不能
  自证合法。

**验证**：

- 测试 empty/多 Turn 的完整 boundary 列表与 `boundary_after`；zero、head、future redo
  boundary 均有明确含义。
- 负向覆盖跨 conversation、stale version、同 index ABA、未知 Turn、越过 fork ceiling、
  有 pending 时操作和伪造 serde token；错误分类稳定且不改变状态。
- 依次通过 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、boundary 聚焦测试、
  `cargo test --all --all-targets`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` 和
  `git diff --check`。

**完成记录（2026-07-13）**：

- 新增字段私有、可 `Copy`/debug/serde 的 `Boundary`，稳定绑定签发
  `ConversationId`、完整 Turn 数量、前置 `TurnId` anchor 与 structural version；公共 API
  只提供只读 getter，token 只能由 `valid_boundaries`/`boundary_after` 从 Conversation 事实
  签发。serde `deny_unknown_fields` 且反序列化只恢复不可信声明，compile-fail doctest 钉住
  外部不能直接构造字段。
- 新增统一只读 resolver 与分类 `BoundaryError`：固定按 owner、version、pending、backing
  range、fork ceiling、anchor 校验，并接入 `ConversationError`。zero、多 Turn、当前 head 与
  同 lineage 的 future redo suffix 均有明确位置；unknown raw identity、detached branch 与
  fork parent suffix 分别返回稳定错误，所有拒绝路径均保持 history/pending/index/version
  全结构不变。
- `History` 现将 active head、addressable lineage ceiling 和共享 backing allocation 分离；
  root revert 后仍可为 redo suffix 签发新 token，测试 child 共享 immutable prefix 时只暴露
  fork ceiling 以内边界且不复制 message storage。现有原子 commit 继续作为当前结构变化推进
  version，旧 token 即使位置与 anchor 不变也稳定返回 `StaleBoundary`，为后续受检 head 变化
  复用同一 version domain。
- 聚焦测试按 positive/negative/serde 模块拆分，覆盖 empty/multi、zero/head/future redo、
  direct lookup、cross-conversation、stale/同位置 ABA、pending、unknown/detached Turn、越过
  fork ceiling、伪造 range/zero/missing anchor、稳定 serde shape 与 unknown field 拒绝；同步
  更新规范 §9、README 和 crate/module rustdoc。阶段顺序与完成标准未变，故未修改 `PLAN.md`。
- 验证通过：`cargo fmt --all`；`cargo clippy --all-targets -- -D warnings`；
  `cargo test conversation::boundary -- --nocapture`（12 passed）；1800 秒硬上限内
  `cargo test --all --all-targets`（232 个库测试与 3 个离线集成测试 passed、7 ignored、
  0 failed，所有 example targets passed）；`cargo test --doc`（1 个正向与 10 个 compile-fail
  passed）；`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`；`git diff --check`。

### M3-3 [DONE] 逻辑 `head`、revert/redo 与 revert 后分支

**前置依赖**：M3-2。

**上下文**：revert 只能移动 head，不能删除 Turn、Message、artifact 或 id。移动后视图和
派生 index 以 head 为上界；从旧 head 再追加应形成新 parent 路径而非覆盖旧 raw suffix。

**做什么**：

- 实现 `revert_to(Boundary)`：拒绝 pending，验证 token 后移动逻辑 head、更新 version 并
  重建/更新有效 `ToolCallIndex`；返回包含 old/new head 的可观测 outcome，不物理删除数据。
- 通过重新获取的较后 Boundary 实现 redo；redo 只沿当前 active lineage，不能跳入已经改道
  的 detached branch。
- 从 reverted head `begin_turn`/commit 时，新 Turn 的 parent 必须是该 head 对应 Turn；active
  lineage 切换到新 suffix，旧 suffix 留在 raw store，但不参与当前 boundaries/view/index。
- 提供只读 raw/debug 查询与 current-lineage 查询，清晰区分“保留”与“当前有效”，不暴露
  修改入口。

**验证**：

- 覆盖多次 revert→redo、revert 到 zero、revert 后新 commit、再次 revert，以及 tool index
  随 head 改变；所有 raw id/payload 始终存在且不变。
- 断言旧 boundary 在 version 变化后失效，重新生成后可操作；detached suffix 不泄漏进当前
  view/index，parent tree 可重建两条路径。
- 依次通过 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、revert/redo 聚焦
  测试、`cargo test --all --all-targets`、
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` 和 `git diff --check`。

**完成记录（2026-07-13）**：

- 新增公开只读 `Conversation::head`、受检 `revert_to(Boundary)` 与 `RevertOutcome`。Backward
  revert 和同一 addressable lineage 上的 forward redo 共用 M3-2 resolver；真实移动先完成
  owner/version/pending/range/anchor 校验、下一 structural version 计算与有效前缀
  `ToolCallIndex` 重建，再一次性切换 head/index/version。返回的 old/new head 均按新 version
  重新签发，可直接以 old head redo；当前 head 目标是显式、保留 version 的 no-op，version
  耗尽则分类为 `NonAtomicHeadMove` 且全结构不变。
- `History` 将逻辑 `active_len` 的有效前缀、含 redo suffix 的 addressable lineage 和 append-only
  raw scope 保持分离；`turns()`、新增 `lineage_turns()`、`raw_turns()`/`raw_turn(id)` 分别暴露
  head-clipped current view、可 redo 当前路径与 retained raw debug 只读视图。head 移动不删除或
  重分配任何 Turn/message；派生 index 始终只含 head 以内 committed calls 与当前 pending。
- 从 reverted head 再次提交会以该 head 的 Turn 为 parent 生成 replacement suffix，并把旧
  suffix 留在 raw parent tree；旧 suffix 随即从 current lineage、boundaries 和工具索引中隔离，
  但 Turn/message/tool-call identity 仍在全部 raw 分支范围内禁止复用。既有 M3-1 retention
  测试改为走公开受检 revert API，移除了测试专用的未校验 head mutation hook。
- 新增 7 个 revert/redo 聚焦回归，覆盖多次 backward/forward、zero、中间 head、current-head
  no-op、旧 token stale/新 token 可用、pending/foreign/forged/detached 拒绝、version exhaustion、
  tool index clipping/rebuild、raw payload 与共享 message storage 不变、revert 后新 commit、再次
  revert/redo，以及两条 parent 路径重建；所有拒绝路径逐项比较 head/history/raw/pending/index/
  version 快照。同步更新 README、Conversation/crate rustdoc；阶段计划未变化，故未修改
  `PLAN.md`。
- 验证通过：`cargo fmt --all`；`cargo clippy --all-targets -- -D warnings`；
  `cargo test conversation::boundary -- --nocapture`（19 passed）；1800 秒硬上限内
  `cargo test --all --all-targets`（239 个库测试与 3 个离线集成测试 passed、7 ignored、
  0 failed，所有 example targets passed）；`cargo test --doc`（1 个正向与 10 个 compile-fail
  passed）；`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`；`git diff --check`。

### M3-4 [DONE] O(1) `fork_at` 与共享 immutable 历史

**前置依赖**：M3-3。

**上下文**：fork 是从合法 Turn boundary 创建新 Conversation metadata/head，并共享之前的
immutable message/turn；不 clone prefix、不重分配历史 id。父子分支随后必须独立推进。

**做什么**：

- 实现 `fork_at(Boundary, new_conversation_id)`，要求无 pending、boundary 有效且新 id 由
  调用方提供；生成 `ForkOrigin { parent, fork_point }`，child head/fork ceiling 位于目标点。
- fork 只 clone 结构共享 handle 与 O(1) 元数据；历史 `Turn`/`ConversationMessage` 使用相同
  Arc/节点和相同 id。不得按旧 `DESIGN.md` 方向性文字重新分配 shared message id。
- child 使用自己的 version/Boundary owner；父 boundary 不能直接用于 child。父子后续 commit、
  revert、cancel 和 index 更新互不修改对方，child 首个新 Turn parent 指向共享 boundary
  Turn。
- child 的逻辑 raw/debug/persistence 可见集合只含 fork boundary 的祖先与 child 自己的新
  分支；实现可共享更大的底层 store，但父分支在 fork 点之后的 suffix 不能成为 child 的
  boundaries、snapshot 或可观察事实。
- 为复杂度契约增加可判定内部测试（pointer equality + clone counter/不遍历断言），避免只在
  文档声称 O(1)。

**验证**：

- 大历史 fork 测试断言 shared nodes `ptr_eq`、message/turn clone counter 不增长、全部历史
  id 不变；child 创建本身不随历史长度执行线性遍历。
- 分别推进父/子并比较 lineage、origin、head、raw payload 和 index；跨分支 Boundary/id 冲突
  均被拒绝，原 Conversation 不变。
- 依次通过 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、fork 聚焦测试、
  `cargo test --all --all-targets`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` 和
  `git diff --check`。

**完成记录（2026-07-13）**：

- 新增公开 `ForkOrigin` 与 `Conversation::origin()`，并实现
  `Conversation::fork_at(boundary, new_conversation_id)`。fork 先复用统一 Boundary resolver
  校验 owner、version、pending、range、fork ceiling 与 anchor；child 使用调用方提供的独立
  `ConversationId`、从 0 开始的 structural version 和自己的 Boundary owner，origin 保留父
  Conversation id 与父签发的 fork point，父子 token 不能交叉消费。重复使用父 id 会返回
  分类 `ForkError::DuplicateConversationId`，所有拒绝路径保持父 Conversation 全结构不变。
- 将 `History::shared_prefix` 提升为生产 crate-private fork 原语：child 只 clone
  `Arc<Lineage>` 等共享 handle 与 O(1) 元数据，`Turn`/`ConversationMessage` storage 和全部
  已有 id 原样共享；child raw/debug/persistence 可见集合仅包含 fork boundary 祖先与 child
  后续本地分支，父 suffix 不进入 child `turns()`、`lineage_turns()`、`raw_turns()`、
  `boundary_after` 或 snapshot 可观察事实。
- 重构 `ToolCallIndex` 的 committed 部分为共享 backing + 可见 turn/entry scope，pending 仍是
  独立 suffix；`revert_to` 与 `fork_at` 都只调整 scope，不全量扫描历史。branch commit 会从
  当前有效 prefix 生成新 index backing，旧 suffix call 不泄漏到有效 index 或 `Debug` 输出；
  父子后续 commit、revert、cancel 和 pending index 更新互不修改对方。
- 新增 fork 聚焦回归：公开 API 测试覆盖 origin、child-owned boundaries、父 suffix ceiling、
  父子独立推进、parent/child raw payload 与 index 隔离、foreign/parent/child boundary 拒绝、
  pending 阻止 fork 和重复 child id；内部 history 测试覆盖 128 Turn 大历史 fork 的
  `Arc::ptr_eq`、共享节点/message storage、raw base ceiling、index backing sharing 与
  child 创建不克隆 materialized lineage。旧 shared-prefix boundary fixture 改为走真实
  `fork_at`，避免保留第二条 child 构造路径。
- 同步更新 README、crate/conversation rustdoc 和相关测试文案；阶段顺序和完成标准未变化，故未
  修改 `PLAN.md`。
- 验证通过：`cargo fmt --all`；`cargo clippy --all-targets -- -D warnings`；
  `cargo test fork -- --nocapture`（5 passed）；1800 秒硬上限内
  `cargo test --all --all-targets`（243 个库测试与 3 个离线集成测试 passed、7 ignored、
  0 failed，所有 example targets passed）；`cargo test --doc`（1 个正向与 10 个
  compile-fail passed）；`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`；`git diff --check`。

### M3-R [DONE] Milestone 3 Review

**前置依赖**：M3-1 至 M3-4 全部完成。

**上下文**：Boundary、logical head 与结构共享共同决定 revert/fork 的正确性和复杂度；单看
某一个 API 不能证明 raw retention、分支隔离及 stale token 防护同时成立。

**做什么**：

- 对照规范 §7--§9 审查两级 identity、parent tree、Boundary 受检性、logical head、redo 和
  fork origin；确认任何路径都未物理删除 raw history。
- 检查 O(1) fork 是实现与测试保证而非深 clone 后的口头描述；确认 shared history 使用同一
  MessageId/TurnId 且 child 只有新 metadata/id。
- 审查 index 的派生属性、stale/ABA 错误和 pending 时的 boundary 禁止规则，为 Projection
  提供稳定的 checked Turn range 基础。

**验证**：

- 运行 branch/revert/fork 组合矩阵并检查 parent tree、raw retention、active view 与 index；
  公共 API 不泄漏内部结构可变性。
- 依次通过 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、
  `cargo test --all --all-targets`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` 和
  `git diff --check`。

**完成记录（2026-07-13）**：

- 对照规范 §7--§9 完成 M3 审查：Conversation/Turn/Message/ToolCall 两级 identity 仍由外部
  注入且共享历史不 re-id；raw history 通过 parent-pointer/`Arc` 节点保留，逻辑 head 只裁剪
  当前有效视图；`Boundary` 字段私有、serde token 不自证合法，消费统一校验 owner、version、
  pending、range、fork ceiling 与 anchor；`ForkOrigin` 只记录父 Conversation 与父签发 cut，
  child 使用独立 owner/version 域。
- 审查确认 O(1) fork 有实现和测试保证：`History::shared_prefix` 只 clone 共享 handle 与
  O(1) 元数据，`Turn`/`ConversationMessage` storage、`TurnId` 与 `MessageId` 原样共享；child
  raw/debug/persistence 可见范围只含 fork 点祖先与 child 本地 suffix，父 suffix 不进入 child
  `turns()`、`lineage_turns()`、`raw_turns()`、boundary 或 index 可观察事实。
- 审查确认 `ToolCallIndex` 仍是可从 current-lineage closed turns + pending 重建的派生缓存：
  revert 只移动 committed scope，fork 只共享并裁剪 committed backing，branch commit 从有效
  prefix 生成新 suffix；detached/父 suffix call 不泄漏到 active view 或 provider/framework
  lookup，identity 唯一性仍以 retained raw facts 而非 index 为事实来源。
- 新增 `conversation::boundary::tests::review` 组合矩阵回归：同一场景串联 parent revert 后改道、
  从共享中间 boundary fork child、父子各自推进、child revert/redo 和 parent pending 时
  `validate_boundary`/`fork_at`/`revert_to` 拒绝；逐项断言 parent tree、raw retention、active
  view、fork ceiling、shared message storage、index rebuild 等价和 pending 禁止规则。阶段顺序、
  依赖与完成标准未变化，因此未修改 `PLAN.md`。
- 验证通过：`cargo fmt --all`；`cargo clippy --all-targets -- -D warnings`；
  `cargo test conversation::boundary::tests::review -- --nocapture`（1 passed）；
  `cargo test conversation::boundary -- --nocapture`（23 passed）；1800 秒硬上限内
  `cargo test --all --all-targets`（244 个库测试与 3 个离线集成测试 passed、7 ignored、
  0 failed，所有 example targets passed）；`cargo test --doc`（1 个正向与 10 个 compile-fail
  passed）；`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`；`git diff --check`。

---

## Milestone 4 — Projection 与 Compaction

### M4-1 [DONE] `Projection`、`Span`、`Artifact` 与受检覆盖范围

**前置依赖**：M3-R。

**上下文**：projection 是 raw history 上的可序列化 overlay。span 只能覆盖完整 Turn，
artifact 必须带 provenance；运行时 Boundary version token 不能未经解析直接长期充当事实。

**做什么**：

- 在 `conversation/projection/` 定义 `Projection`、`Span::Raw`、`Span::Compacted`、
  `Artifact`、`StrategyRef` 和 token accounting/provenance。Artifact 用 `ArtifactId` 标识并
  承载一个或多个完整 Client `Message` 作为渲染内容，不修改 raw ConversationMessage。
- 定义 `CheckedTurnRange`：只能由同一 Conversation 当前有效的 start/end Boundary 解析得到，
  要求有序、非空（需要时显式允许 zero-length）、不超过 head 且不含 pending；内部持久化
  稳定 Turn anchors/id，后续使用时重新对照 lineage，避免 ephemeral version 失效。
- Projection span 必须有序、无重叠，完整描述 raw/compacted 片段；compacted span 保存 covers、
  artifact id 和 produced_by，artifact provenance 保存输入范围、策略版本与 tokens before/
  after。
- 所有字段可 serde 但构造受检；拒绝跨 Conversation boundary、反向范围、切入 detached
  branch、缺失 artifact 和重复 ArtifactId。

**验证**：

- serde/构造测试覆盖 raw、单层 compacted、多个 tiered artifact 和 provenance；稳定 range
  在 version 改变后按 Turn anchors 正确重验证。
- 负向覆盖跨 owner、越 head、覆盖 pending、反向/重叠 span、未知 Turn/artifact 和 detached
  branch；失败不改变现有 projection。
- 依次通过 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、projection model
  聚焦测试、`cargo test --all --all-targets`、
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` 和 `git diff --check`。

**完成记录（2026-07-13）**：

- 新增 `conversation::projection` 模块并公开 `Projection`、`Span::{Raw, Compacted}`、
  `CheckedTurnRange`、`Artifact`、`ArtifactProvenance`、`StrategyRef` 与
  `TokenAccounting`。Artifact 使用外部 `ArtifactId`，持有一条或多条完整 Client `Message`
  作为渲染内容，并通过 provenance 记录输入 Turn range、策略引用与压缩前后 token usage；
  不修改 raw `ConversationMessage` 或 closed `Turn`。
- `CheckedTurnRange` 只能从同一 Conversation 当前有效的 start/end `Boundary` 受检创建；创建时
  拒绝跨 owner、pending、反向、空 range 和越过当前 head 的 redo suffix。持久化形状保存
  `ConversationId`、Turn 位置与稳定 `TurnId` anchor，不保存一次性 structural version；后续
  `validate_checked_turn_range` 会按当前 lineage 重新核对 owner、pending、head、anchor、
  unknown/detached Turn，而不会把反序列化 range 当作自证事实。
- `Projection::new` 对 supplied spans/artifacts 做完整校验：span 必须从 0 到当前 head 连续、
  有序、无重叠且不留 gap；compacted span 必须引用已提供 artifact，且 artifact provenance 的
  covers 与 `produced_by` 必须和 span 一致；重复 `ArtifactId`、空 artifact messages、缺失
  artifact、未知/脱离当前分支的 Turn anchor 均分类拒绝。serde 可恢复声明，但进入
  Conversation 使用前仍需重新校验。
- `Conversation` 现持有只读 `projection()`；新建、成功 commit 和 fork child 默认维护 all-raw
  projection，fork child 的 projection owner 与范围只覆盖自己的 fork prefix。M4-1 未提前实现
  M4-2 `effective_view` 或 M4-3 `apply_compaction`；阶段顺序与完成标准未变化，故未修改
  `PLAN.md`。
- 新增 8 个 projection 聚焦测试，覆盖 raw 默认 projection、raw+compacted+raw serde
  round-trip、多个 compacted/tiered artifact、provenance/token accounting、range 在
  structural version 改变后按 Turn anchor 重验证，以及跨 owner、pending、反向、越 head、
  gap、overlap、incomplete、missing/duplicate artifact、provenance mismatch、empty artifact、
  unknown Turn、detached branch，以及 artifact/projection serde 路径的本地 shape 拒绝。
- 验证通过：`cargo fmt --all`；`cargo clippy --all-targets -- -D warnings`；
  `cargo test conversation::projection -- --nocapture`（8 passed）；1800 秒硬上限内
  `cargo test --all --all-targets`（252 个库测试与 3 个离线集成测试 passed、7 ignored、
  0 failed，所有 example targets passed）；`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`；
  `git diff --check`。

### M4-2 [DONE] `effective_view`、head clipping 与 pending 隔离

**前置依赖**：M4-1。

**上下文**：发给 Client 的是 system + projection 后的完整消息，不是 raw log。视图必须以
head 为上界；若 revert 落进一个 compacted cover，不能使用包含未来 Turn 信息的完整摘要。

**做什么**：

- 定义 Client-ready `EffectiveView`，单列 system prompt 与有序完整 `Message`；
  `Conversation::effective_view()` 遍历 head 以内 spans，Raw 渲染原 Turn messages，Compacted
  渲染 artifact messages。
- 默认 Projection 等价于全部可见 raw turns；raw history、message identity 和 pairing 不因
  生成视图而 clone/mutate（返回 payload clone 仅用于构造 Client 请求时须有明确边界）。
- 实现 head clipping：完整位于 head 前的 compacted span 可用；head 落在 cover 中时，该 span
  对可见前缀回退为 raw turns，严禁摘要泄漏 head 后内容；redo 到完整 cover 后可再次使用
  artifact。
- pending 永远不进入 committed effective view。另提供只读 `pending_context`，只返回 pending
  中已冻结的完整 messages；活跃 partial 单独保持不可见。

**验证**：

- 覆盖纯 raw、raw+compacted+raw、多个 tier、zero head、head 在 span 前/后/内部、
  revert→redo 和 fork child ceiling；断言消息顺序/system 正确且无未来内容泄漏。
- pending 测试确认已冻结消息只经显式 pending_context 出现、partial 永不出现、projection
  不能覆盖 pending。
- 依次通过 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、effective view
  聚焦测试、`cargo test --all --all-targets`、
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` 和 `git diff --check`。

**完成记录（2026-07-13）**：

- 新增并导出 `EffectiveView` 与 `PendingContext`；`Conversation::effective_view()` 现在单列
  system prompt，并把 committed projection 渲染为有序完整 Client `Message` payload。视图生成
  只在 Client-request 边界 clone payload，不暴露或修改 `ConversationMessage` identity、
  pairing、raw Turn storage 或 projection facts。
- `effective_view` 遍历当前 projection spans 并以 logical head 为上界：Raw span 渲染 head
  以内 raw Turn messages；完整位于 head 前的 Compacted span 渲染 artifact messages；当 head
  落入 compacted cover 内时，该可见前缀自动回退 raw Turns，避免摘要包含 head 后未来 Turn。
  redo 到完整 cover 后会再次使用 artifact；zero head 与 fork child ceiling 都只渲染各自可见
  范围。
- 新增 `Conversation::pending_context()`，committed `effective_view` 永不包含 pending；调用方
  需要 in-flight 上下文时必须显式读取 pending context。该 context 只返回 pending 中已冻结的
  完整 payload（包括 user、已冻结 assistant/tool messages），active `PendingMessage`
  accumulator、partial text/reasoning/tool JSON 始终不可见。
- 新增 6 个 M4-2 聚焦回归，覆盖纯 raw 默认 projection、raw+compacted+raw、多个 compacted
  tier、zero head、head 在 compacted span 前/内/后、revert→redo、fork child ceiling，以及
  pending ready/active-streaming partial 隔离；同步更新 README、crate rustdoc 与
  conversation 模块 rustdoc。阶段顺序与完成标准未变化，故未修改 `PLAN.md`。
- 验证通过：`cargo fmt --all`；`cargo clippy --all-targets -- -D warnings`；
  `cargo test conversation::projection -- --nocapture`（14 passed）；1800 秒硬上限内
  `cargo test --all --all-targets`（258 个库测试与 3 个离线集成测试 passed、7 ignored、
  0 failed，所有 example targets passed）；`cargo test --doc`（1 个正向与 10 个 compile-fail
  passed）；`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`；`git diff --check`。

### M4-3 [DONE] 原子 `apply_compaction` 与 tiered/consolidated 更新

**前置依赖**：M4-2。

**上下文**：compaction 只替换 overlay，不删除 raw。计划需要既能只压新 raw tail，也能把
旧 summary 与新 raw 合并重写；每一步都必须在当前完整 Turn boundary 上验证。

**做什么**：

- 定义可序列化 `CompactionPlan`/`CompactionStep`，能寻址 raw range 或已有 span range，引用
  `StrategyRef` 并接收外部生成的 Artifact；plan 本身不持有 client/closure。
- 实现 `apply_compaction` 的两阶段原子流程：先验证所有 target、artifact covers、顺序、
  重叠、head/version、pending 和 provenance，再一次性生成新 Projection。任何一步失败保留
  旧 projection/artifacts。
- 支持 tiered（旧 compacted span 保留，只替换 raw tail）与 consolidate（旧 summary spans +
  raw tail 由新 artifact 覆盖）两类操作；被替换 artifact 仍作为 provenance/raw audit 数据
  保留，不被物理删除。
- compaction 只能覆盖 `[0, head)` 的 closed Turn；pending 时只允许记录“待执行”意图，不得
  实际 apply。soft-limit 触发可推迟，turn 内 hard-limit 不在本层处理。

**验证**：

- 测试首次压缩、连续 tiered、summary-of-summaries consolidate、部分 raw tail、revert 穿过
  压缩点与 redo；每次比较 raw turns/id/payload 在 apply 前后完全一致。
- 负向覆盖 stale plan、artifact covers 不一致、重叠/越界 target、未知 strategy/artifact、
  pending apply 和多 step 中途失败；断言 projection 原子不变。
- 依次通过 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、apply compaction
  聚焦测试、`cargo test --all --all-targets`、
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` 和 `git diff --check`。

**完成记录（2026-07-13）**：

- 新增可序列化 `CompactionPlan`、`CompactionStep` 与 `CompactionTarget`，plan 只保存
  Conversation owner、structural version、head、目标 range、`StrategyRef` 和外部生成的
  `Artifact` 数据，不携带 client、closure、registry handle 或 runtime strategy 实例。
  raw target 可在 Turn 边界切分当前 raw span；span target 必须对齐当前 projection span
  边界，用于 consolidate 旧 summary spans 与 raw tail。
- 新增 `Conversation::apply_compaction(&CompactionPlan)` 两阶段原子流程：先验证 plan
  owner/version/head、pending、一组 step 的顺序与重叠、target 与当前 projection 类型/边界、
  artifact presence、duplicate/unreferenced artifact、provenance range 与 strategy 一致性，
  再在临时状态中生成新 `Projection` 并一次性替换；成功后推进 structural version，任何失败
  都保持旧 projection、artifacts、raw history、head、index 与 version 不变。
- 支持首次 raw range 压缩、连续 tiered raw tail 压缩、部分 raw tail 切分，以及
  summary-of-summaries consolidate；consolidate 后被替换 artifact 继续留在 projection
  artifacts 中作为 provenance/audit 数据。`effective_view` 仍按 head clipping 防止 revert
  落入 compacted cover 时泄漏未来摘要；raw `Turn`、message id 和 payload 在 apply 前后逐项
  相等。
- 修正 commit 后 projection 维护：新 Turn 提交不再无条件重置 all-raw projection，而是保留
  当前有效 overlay 并追加/合并新 raw tail；若从 reverted head 内部提交，新分支只保留当前
  head 可见且 anchor 仍匹配的 overlay/artifact，半个 compacted cover 回退为 raw prefix。
- 新增 M4-3 聚焦测试 5 个，覆盖首次压缩、连续 tiered、summary-of-summaries consolidate、
  部分 raw tail、commit 后 raw tail 保留、revert 穿过压缩点与 redo，以及 stale plan、
  pending apply、raw target 命中 compacted span、span target 切 span、缺失 artifact、
  provenance range/strategy mismatch、overlapping multi-step、中途失败、unreferenced artifact
  和 empty plan 的原子拒绝。
- 同步 README、crate rustdoc 与 conversation 模块 rustdoc 的当前能力描述；阶段顺序与完成
  标准未变化，故未修改 `PLAN.md`。
- 验证通过：`cargo fmt --all`；`cargo clippy --all-targets -- -D warnings`；
  `cargo test conversation::projection -- --nocapture`（19 passed）；1800 秒硬上限内
  `cargo test --all --all-targets`（263 个库测试与 3 个离线集成测试 passed、7 ignored、
  0 failed，所有 example targets passed）；`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`；
  `git diff --check`；额外 `cargo test --doc`（1 个正向 doctest 与 10 个 compile-fail doctest
  passed）。

### M4-4 [DONE] Compaction strategy/trigger 扩展点与数据/行为分离

**前置依赖**：M4-3。

**上下文**：总体设计要求 strategy 与 trigger 解耦并保持 dyn-safe；但具体 summarizer、模型
client 与调度属于外部运行时，不能序列化进 Conversation 或把 Agent loop 拉入本层。

**做什么**：

- 定义 `#[async_trait]` dyn-safe `CompactionStrategy`，输入只读 spans/effective context 与
  `CompactCtx`，返回受检前的 artifact draft；外部通过 `StrategyRef`/显式 id 关联实例。
- 定义同步 `CompactionTrigger`，只在 Turn boundary 观察 immutable Conversation/Usage 并返回
  data-only `CompactionPlan` 或 `DeferredUntilBoundary`；trigger 不直接修改 projection。
- 明确 runtime strategy/trigger/client handle 不 serde；Conversation 只保存 `StrategyRef`、
  plan、artifact 和 provenance。没有 registry 时返回可观测的 unresolved error，不做 fallback。
- 提供 mock strategy/trigger 测试工具或示例，证明 tiered/consolidated 可使用不同策略引用，
  但不实现真实 LLM summarizer、budget loop 或 tool registry。

**验证**：

- dyn trait object 测试覆盖 mock async strategy、不同 trigger、deferred pending 与 boundary
  执行；data plan serde round-trip 后可由相同 StrategyRef 解析。
- serde 测试确认 trait object/client 不进入 snapshot；缺失/错误 strategy reference 明确失败，
  不默默采用其他算法。
- 依次通过 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、strategy/trigger
  聚焦测试、`cargo test --all --all-targets`、
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` 和 `git diff --check`。

**完成记录（2026-07-13）**：

- 新增 `conversation::projection::strategy` 运行时扩展点模块并从 `conversation` 顶层导出：
  `#[async_trait]` dyn-safe `CompactionStrategy`、只读 `CompactionStrategyResolver`、
  同步 `CompactionTrigger`、`CompactionInput`、`CompactCtx`、`ArtifactDraft`、
  `CompactionTriggerOutcome` 和可 serde 的 `DeferredUntilBoundary`。strategy 只接收只读 spans、
  `EffectiveView` 与 data-only ctx，返回未持久化 artifact draft；artifact id、covers 与
  `StrategyRef` 由 ctx 绑定成 provenance，运行时对象不能改写持久化事实。
- 新增分类化 `CompactionError` 并接入 `ConversationError`，覆盖无 registry/缺失 strategy 的
  `UnresolvedStrategy`、resolver 返回错误实例的 `StrategyReferenceMismatch` 和 strategy 自身
  `StrategyFailed`；`run_compaction_strategy`/`materialize_compaction_plan` 不使用既有 artifact
  作为 fallback，缺失或错误 `StrategyRef` 会显式失败。`StrategyRef` 增加稳定 display，错误和
  测试断言可读。
- `CompactionPlan::with_artifacts` 支持 trigger 先产出不含 artifacts 的 data-only plan intent，
  异步 strategy 后续按相同 owner/version/head/steps 物化 artifacts；
  `Conversation::evaluate_compaction_trigger` 在 pending 时不调用 trigger，直接返回 `DeferredUntilBoundary`，
  在完整 Turn boundary 只以 immutable `&Conversation` 与 `Usage` 调用 trigger，trigger 不能直接
  修改 projection。
- 新增 5 个 M4-4 聚焦测试，覆盖 mock async strategy trait object、plan serde round-trip 后按同一
  `StrategyRef` 解析并 apply、raw tiered 与 span consolidated 两种 trigger 使用不同策略引用、
  pending deferred 且 runtime trigger 未被调用、无 registry/缺失 strategy/错误 strategy ref/
  strategy failure 明确分类失败，以及 runtime trigger/strategy/client marker 不进入 data plan 或
  materialized artifacts 的 serde 输出；空 draft 仍由 projection artifact 校验拒绝。
- 同步 README、crate rustdoc 与 conversation 模块 rustdoc 的当前能力描述；具体 LLM summarizer、
  budget loop、registry 实现、Agent loop、Tool registry 和多 agent 编排仍保持在本 crate 范围外。
  阶段顺序和完成标准未变化，故未修改 `PLAN.md`。
- 验证通过：`cargo fmt --all`；`cargo clippy --all-targets -- -D warnings`；
  `cargo test conversation::projection::tests::strategy -- --nocapture`（5 passed）；
  `cargo test conversation::projection -- --nocapture`（24 passed）；1800 秒硬上限内
  `cargo test --all --all-targets`（268 个库测试与 3 个离线集成测试 passed、7 ignored、
  0 failed，所有 example targets passed）；`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`；
  `git diff --check`。

### M4-R [DONE] Milestone 4 Review

**前置依赖**：M4-1 至 M4-4 全部完成。

**上下文**：projection 与 raw history、head、pending 和运行时策略四个边界交叉；独立 Review
用于确认摘要 overlay 没有退化为破坏性 truncate 或泄漏未来内容。

**做什么**：

- 对照规范 §6/§6.1 审查 raw/projection 分离、Turn-boundary covers、pending 排除、head 上界和
  soft/hard-limit 职责；确认 compaction 从未删除/改写历史。
- 核对 effective_view 在 revert 落入 compacted span 时不泄漏未来摘要，redo 后语义恢复；
  tiered/consolidated provenance 足以调试 summary-of-summaries。
- 审查 strategy/trigger 的 dyn-safe/data-only 边界，确认没有把 Agent loop、registry 或具体
  summarizer 偷渡进 Core。

**验证**：

- 运行 projection/compaction/revert/fork 组合矩阵，检查视图、raw、artifact 与 provenance；
  公共 API 只能通过 checked range 和原子 apply 改 projection。
- 依次通过 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、
  `cargo test --all --all-targets`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` 和
  `git diff --check`。

**完成记录（2026-07-13）**：

- 对照规范 §6/§6.1 完成 M4 审查：projection 仍是 raw history 上的非破坏性 overlay；
  `CheckedTurnRange` 只覆盖完整 Turn boundary；`effective_view` 以 logical head 为上界；
  pending 不进入 committed projection/effective view，只能通过显式 `pending_context` 读取已冻结
  payload；soft-limit 只产生可推迟的 data-only intent，turn 内 hard-limit 仍属 Agent loop 层职责。
- 新增 `conversation::projection::tests::review` 组合矩阵回归：同一父会话串联首次 tiered
  compaction、第二段 raw tail compaction、summary-of-summaries consolidate、revert 进入
  compacted cover、redo、从 cover 内 fork child，以及 pending 时 apply 拒绝；逐项断言 raw
  Turn/message id/payload 不变、旧 tier artifact 继续作为 provenance/audit 数据保留、consolidated
  artifact 的 covers/strategy/token accounting 可追踪、head 落入 cover 时只渲染可见 raw 前缀且不
  泄漏未来摘要，redo 后 compacted rendering 恢复。
- 组合矩阵同时确认 fork child 只含 fork ceiling 以内 raw prefix，不继承父 projection artifact、
  父 suffix 或摘要内容；pending active 时 `apply_compaction` 返回分类
  `ProjectionError::PendingTurn`，旧 projection/raw history 原子不变，committed `effective_view`
  不含 pending user，而 `pending_context` 只显式暴露已冻结 user payload。
- strategy/trigger 审查确认 runtime `CompactionStrategy`/`CompactionTrigger`、resolver、client handle
  与 Agent loop 均未进入 Conversation/Core 持久化事实；`CompactionPlan`/`CompactionStep`、artifact
  与 provenance 仍是 data-only，并通过 `StrategyRef` 关联外部实现。阶段顺序、依赖和完成标准未
  变化，因此未修改 `PLAN.md`。
- 验证通过：`cargo fmt --all`；`cargo clippy --all-targets -- -D warnings`；
  `cargo test conversation::projection::tests::review -- --nocapture`（1 passed）；
  `cargo test conversation::projection -- --nocapture`（25 passed）；1800 秒硬上限内
  `cargo test --all --all-targets`（269 个库测试与 3 个离线集成测试 passed、7 ignored、
  0 failed，所有 example targets passed）；`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`；
  `git diff --check`。

---

## Milestone 5 — Serde 与持久化

### M5-1 [DONE] Boundary 一致点 `ConversationSnapshot`

**前置依赖**：M4-R。

**上下文**：pending 默认不持久化。snapshot 必须包含恢复有效视图所需的全部事实，同时
排除 Accumulator、派生 index、策略实例和其他 runtime 资源。

**做什么**：

- 定义 versioned `ConversationSnapshot` schema，包含 id/config、所有 retained raw closed
  turns/messages、active lineage/head、structural version、fork origin/ceiling、projection、
  artifacts/provenance；格式有显式 schema version 以便后续 migration。
- 实现 `Conversation::snapshot()`：仅当 `pending == None` 且位于 committed Boundary 时成功；
  有 pending 返回分类错误，不自动 cancel、finish 或丢弃。
- snapshot 通过稳定 id/parent anchors 表达结构共享，不按每个 fork 深复制同一 message payload；
  同一 snapshot 内重复引用只序列化一个事实记录。
- 明确排除 `PendingTurn`、`PendingMessage`、Accumulator、ToolCallIndex、Arc/lock、client/registry
  handle 和 strategy/trigger object；只保存 data-only StrategyRef。

**验证**：

- snapshot serde round-trip 覆盖线性历史、revert 后 detached suffix、fork origin、多个
  artifacts 和 projection；检查共享 message/turn 事实只出现一次。
- 有 text/tool partial、open call 或 ready-to-commit pending 时均拒绝 snapshot，且原状态不变；
  runtime-only 字段不出现在 JSON。
- 依次通过 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、snapshot 聚焦测试、
  `cargo test --all --all-targets`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` 和
  `git diff --check`。

**完成记录（2026-07-13）**：

- 新增 `conversation::persistence` 模块并公开 versioned
  `ConversationSnapshot`、`ConversationSnapshotHistory` 与
  `CONVERSATION_SNAPSHOT_SCHEMA_VERSION`。snapshot 记录 id/config、structural version、
  retained raw closed Turn facts、当前 addressable lineage、logical head、fork ceiling、
  `ForkOrigin`、projection spans 及 retained artifacts/provenance；raw Turn 通过
  validator-facing data shape 写入，每个 retained Turn/message fact 在同一 snapshot 中只出现一次。
- 新增 `Conversation::snapshot()` 和分类 `SnapshotError::PendingTurn`。snapshot 只在
  `pending == None` 的 committed consistency point 成功；存在 active text/tool partial、
  open call 或 ready-to-commit pending 时均拒绝，不自动 finish、cancel、discard，也不改变
  Conversation 的 history/head/projection/index/pending/version。
- snapshot serde 形状明确排除 `PendingTurn`、`PendingMessage`、Accumulator、`ToolCallIndex`、
  `Arc`/lock、client/registry handle 和 runtime strategy/trigger object；projection 内只保留
  data-only `StrategyRef`、artifact messages、covers 和 token accounting。M5-1 未实现 restore
  或 DB-neutral rows，后续仍须按 M5-2/M5-3 重新校验事实并重建派生 index。
- 新增 6 个 persistence snapshot 聚焦测试，覆盖线性 text+tool history round-trip、revert 后
  detached raw suffix 与 current lineage 分离、fork child origin/ceiling 且不包含父 suffix、
  compaction artifact/provenance round-trip、runtime-only JSON key 缺席，以及 active partial、
  open call、ready-to-commit pending 的原子拒绝。
- 同步 README、crate docs 与 conversation 模块 rustdoc 的当前能力描述；阶段顺序和完成标准
  未变化，故未修改 `PLAN.md`。
- 验证通过：`cargo fmt --all`；`cargo clippy --all-targets -- -D warnings`；
  `cargo test conversation::persistence -- --nocapture`（6 passed）；1800 秒硬上限内
  `cargo test --all --all-targets`（275 个库测试与 3 个离线集成测试 passed、7 ignored、
  0 failed，所有 example targets passed）；`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`；
  `git diff --check`。

### M5-2 [DONE] 受检 restore 与派生索引重建

**前置依赖**：M5-1。

**上下文**：直接 derive `Deserialize` 到 live Conversation 会绕过 commit 门。恢复必须先进入
data snapshot，再验证全部事实和 projection，最后构建 runtime history/index。

**做什么**：

- 实现 `Conversation::restore(snapshot)`/`TryFrom<ConversationSnapshot>`，按顺序校验 schema
  version、全局 id 唯一、parent 存在且无环、Turn I1--I4、active lineage、head/ceiling、fork
  origin、projection ranges/artifacts/provenance；任一错误返回带路径的 `RestoreError`。
- 只在全部校验成功后建立结构共享 runtime nodes，恢复 logical head/version，并从事实数据
  重建 ToolCallIndex；不恢复 pending。
- 比较重建 index 与全量扫描结果，检测 snapshot 中任何冗余派生字段并拒绝/忽略其成为事实
  来源。
- 设计明确的 schema-version/migration 入口；当前未知未来版本拒绝而非猜测字段含义。

**验证**：

- 正向执行 snapshot→JSON→snapshot→restore，断言 raw/current lineage/head/origin/projection/
  artifacts/index 全结构等价。
- 系统注入损坏数据：duplicate id、missing/cyclic parent、非法 Turn、head 不在 lineage、错误
  fork point、重叠 span、missing artifact、错误 covers 和未知 schema version；全部明确拒绝且
  不产生半恢复 Conversation。
- 依次通过 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、restore/corruption
  聚焦测试、`cargo test --all --all-targets`、
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` 和 `git diff --check`。

**完成记录（2026-07-13）**：

- 新增公开 `Conversation::restore(snapshot)` 与 `TryFrom<ConversationSnapshot>`，通过
  schema-version 分发入口恢复当前 v1 snapshot；未知未来版本明确返回带路径的
  `RestoreError::UnsupportedSchemaVersion`，不猜测字段含义。`ConversationError` 新增
  `Restore` 分类，`RestoreError` 覆盖 schema、count、raw Turn id、parent missing/cycle、
  disconnected raw tree、lineage/head/ceiling、fork origin、Turn validator、projection 与派生
  index mismatch，所有错误都携带 JSON-like path。
- restore 不直接反序列化 live `Conversation`/`Turn`。raw `TurnData` 先逐条复用唯一
  I1--I4 `validate_turn_data` 门，再校验 retained parent graph 存在、无环且属于同一 root，
  active lineage 的 parent 顺序、head/fork ceiling 和 fork origin boundary owner/anchor；全部
  通过后才调用 crate-private `History::from_restored` 建立 parent-pointer runtime nodes，恢复
  logical head、structural version、fork origin，并保证 pending 为空。
- 新增 projection restore-time 校验，针对完整 addressable lineage/fork ceiling 重新检查 range
  owner、Turn anchors、artifact messages/provenance、span gap/overlap 和完整覆盖；因此合法的
  revert/head-clipping snapshot 可恢复，而 owner/anchor/provenance 损坏会在 restore 阶段拒绝。
  snapshot schema 仍通过 `deny_unknown_fields` 拒绝 `tool_call_index` 等冗余派生字段，restore
  只从 closed facts 重建 `ToolCallIndex` 并与独立全量 scan 比较，不把派生字段作为事实来源。
- 新增 restore/corruption 聚焦测试：正向覆盖 snapshot→JSON→snapshot→restore、`TryFrom`、
  raw/current lineage/head/version/origin/projection/artifacts/effective_view/index 全结构等价，
  以及 fork child 在 compacted span 内 revert 后的恢复；损坏数据覆盖未知 schema version、
  duplicate raw Turn id、非法 Turn、missing/cyclic parent、unknown lineage turn、head 越界、
  fork origin self-parent/owner/anchor 错误、projection owner/anchor 错误、overlap span、
  missing artifact、错误 covers 和冗余 derived field 拒绝。
- 同步更新 persistence 与 conversation 模块 rustdoc；阶段计划和依赖结构未变化，故未修改
  `PLAN.md`。
- 验证通过：`cargo fmt --all`；`cargo test conversation::persistence -- --nocapture`
  （12 passed）；`cargo clippy --all-targets -- -D warnings`；30 分钟硬上限内
  `cargo test --all --all-targets`（281 个库测试与 3 个离线集成测试 passed、7 ignored、
  0 failed，所有 example targets passed）；`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`；
  `git diff --check`。

### M5-3 [DONE] DB-neutral parent-tree row 映射

**前置依赖**：M5-2。

**上下文**：本 crate 不绑定数据库驱动，但需要可落库的稳定记录：conversation/turn/message
以 parent pointer 和 sequence 表达；message/turn immutable、只 INSERT 不 UPDATE，fork 不复制
共享历史行。

**做什么**：

- 在 `conversation/persistence/rows.rs` 定义 DB-neutral `ConversationRecord`、`TurnRecord`、
  `MessageRecord`、`ToolPairingRecord`、`ArtifactRecord`、Projection records 及必要关联；明确 PK/FK
  字段、parent_turn_id、message seq、owner/origin 和 schema version。
- 实现 snapshot/history 与 rows 的确定性分解和受检重组；payload 可用 typed fields 或稳定 JSON，
  但恢复必须走 M5-2 validator，不能由 row 顺序暗示合法性。
- fork 导出只新增 child ConversationRecord 和 child 新 Turn rows，共享祖先按稳定 id 引用；提供
  可判定的 insert set，不为共享 message 生成新 id/重复 UPDATE。
- 文档明确 annotation/评分另表引用 MessageId，不能更新 immutable MessageRecord；不实现 SQL、
  migration runner 或具体数据库连接。

**验证**：

- 线性/tool/fork/projection 数据执行 snapshot→rows（打乱读取顺序）→snapshot→restore，检查
  parent/seq/pairing 和 effective ordering 正确。
- fork 测试断言 shared ancestor Turn/Message rows id/payload 仅一份，child 只增加 metadata/
  suffix；生成的 mutation 集只有 INSERT，无历史 UPDATE。
- 缺行、重复 PK、错误 FK/seq/cycle 和 orphan artifact rows 明确失败。
- 依次通过 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、row mapping 聚焦测试、
  `cargo test --all --all-targets`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` 和
  `git diff --check`。

**完成记录（2026-07-13）**：

- 新增 `conversation::persistence::rows` 并公开 DB-neutral row DTO：
  `ConversationRecord`、`ConversationTurnRecord`、`ConversationLineageTurnRecord`、
  `TurnRecord`、`MessageRecord`、`ToolPairingRecord`、`ProjectionRecord`、
  `ProjectionSpanRecord`、`ArtifactRecord`、`ConversationRows` 与
  `ConversationRowInsertSet`。row schema 明确保存 conversation owner、origin、structural
  version、head/fork ceiling、parent Turn pointer、message/pairing/projection/artifact dense
  sequence、稳定 PK/FK 和 projection/artifact provenance；不引入 SQL、migration runner 或
  具体数据库连接。
- `ConversationSnapshot::to_rows`/`ConversationRows::from_snapshot` 将 snapshot facts
  确定性分解为 immutable global facts 与 per-Conversation association rows；
  `ConversationRows::into_snapshot`/`ConversationSnapshot::from_rows` 会在忽略读取顺序的情况下
  重新按显式 sequence 分组，检查 owner、schema、duplicate PK、sequence gap、缺失 FK、orphan
  rows 与 projection data shape，然后只生成 data snapshot，恢复 live Conversation 仍必须继续
  走 M5-2 `Conversation::restore` validator。
- 新增 insert-only diff：`ConversationRows::insert_set_against` 会先验证两边 row set，再只返回
  缺失 rows；若同一 PK 已存在但 immutable fact 不同，则返回 `RowMappingError::InsertConflict`
  而不是描述 UPDATE。fork child 相对 parent 的导出会插入 child conversation/raw/lineage/
  projection association rows 和 child 本地 suffix facts，已存在的共享 ancestor Turn/Message/
  ToolPairing rows 按稳定 id 引用且不会复制或 re-id。
- 新增分类化 `RowMappingError` 并导出；README、crate docs、conversation/persistence rustdoc
  同步说明 row mapping 仍是 data-only 边界，annotation/评分应另表引用 `MessageId`，不能更新
  immutable `MessageRecord.payload`。阶段顺序和完成标准未变化，故未修改 `PLAN.md`。
- 新增 4 个 row mapping 聚焦测试并扩展 persistence 测试到 16 个：覆盖线性 + tool +
  projection 的 snapshot→rows→serde→打乱读取顺序→snapshot→restore、fork child insert-only
  diff 不复制 shared ancestor payload、duplicate PK、missing FK、message seq gap、missing
  message rows、missing/foreign artifact rows，以及 parent cycle 由 restore validator 明确拒绝。
- 验证通过：`cargo fmt --all`；`cargo clippy --all-targets -- -D warnings`；
  `cargo test conversation::persistence -- --nocapture`（16 passed）；1800 秒硬上限内
  `cargo test --all --all-targets`（285 个库测试与 3 个离线集成测试 passed、7 ignored、
  0 failed，所有 example targets passed）；`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`；
  额外 `cargo test --doc`（1 个正向 doctest 与 10 个 compile-fail doctest passed）；
  `git diff --check`。

### M5-4 [DONE] 存盘→恢复→`effective_view` 端到端一致性

**前置依赖**：M5-3。

**上下文**：局部 serde round-trip 不足以证明恢复语义。验收必须覆盖 tool pairing、logical
head、fork 和 compaction 的组合，并以实际 Client-ready effective view 为最终判据。

**做什么**：

- 建立模块化 integration fixture：多 Turn + serial/parallel tools，apply tiered/consolidated
  compaction，revert 到压缩范围内再 redo，并 fork 后分别推进父子。
- 对父子每个一致点同时走 JSON snapshot 与 DB-neutral rows 两条路径，恢复为新 Conversation；
  比较 system、effective messages、raw facts、head/boundaries、origin、projection/provenance、usage
  和 rebuilt ToolCallIndex。
- 验证保存/恢复不调用网络、随机源、时钟或 runtime registry；所有 id/timestamp fixture 显式
  注入，结果可重复。
- 增加 pending snapshot 拒绝后 cancel→commit/discard→成功 snapshot 的完整流程。

**验证**：

- 核心断言为两条持久化路径恢复前后 `effective_view` 全结构一致，且 revert 落入摘要时仍不
  泄漏未来内容；父子共享历史 id 不变、后续推进互相隔离。
- fixture/每个 test 均在 1 分钟内完成，无 sleep、真实 endpoint 或 flaky 时间依赖。
- 依次通过 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、persistence integration
  聚焦测试、`cargo test --all --all-targets`、
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` 和 `git diff --check`。

**完成记录（2026-07-13）**：

- 新增模块化端到端验收 `conversation::persistence::tests::e2e`，复用现有 persistence fixture
  并把复杂场景拆到 `src/conversation/persistence/tests/e2e.rs`，避免继续膨胀主测试文件。fixture
  全部使用显式 `ConversationId`/`TurnId`/`MessageId`/`ToolCallId` 和 caller-supplied
  timestamp/source，不访问网络、随机源、时钟、runtime registry 或真实 endpoint。
- 新增 `snapshot_and_rows_restore_effective_view_across_compaction_revert_fork_and_tools`：构造
  多 Turn 会话，覆盖 serial 两轮 tool、parallel tool 分批返回、tiered raw compaction、
  summary-of-summaries consolidate、revert 落入 compacted cover 后 redo，以及从 cover 内 fork
  后父子分别追加本地 suffix。每个一致点同时走 JSON snapshot 与 DB-neutral rows（含 rows
  serde 和打乱读取顺序）两条路径恢复为新 `Conversation`。
- 端到端断言恢复前后 `system`、`effective_view` messages、raw/current lineage facts、
  `head`/valid boundaries、fork origin、projection spans/artifact provenance、turn usage 和
  rebuilt `ToolCallIndex` 全结构一致；额外断言 head 落入 compacted cover 时只渲染可见 raw
  prefix、不泄漏未来摘要或未来 Turn，redo 后 artifact rendering 恢复，fork child 不继承父
  summary/父 suffix，父子后续推进互相隔离且共享历史 id 不变。
- 新增 `pending_snapshot_rejection_can_be_followed_by_cancel_commit_or_discard_then_restore`：
  覆盖 active partial snapshot 拒绝后 `CancelDisposition::DiscardTurn` 成功回到 committed
  consistency point，以及 open tool call snapshot 拒绝后 `CancelDisposition::commit_turn` 合成
  `ToolStatus::Cancelled` result、追加 final assistant 并成功 commit；两条路径随后均可 snapshot、
  rows rebuild、restore，并保持 partial 不落入 committed effective view。
- 验证通过：`cargo fmt --all`；`cargo test conversation::persistence -- --nocapture`
  （18 passed）；`cargo clippy --all-targets -- -D warnings`；1800 秒硬上限内
  `cargo test --all --all-targets`（287 个库测试与 3 个离线集成测试 passed、7 ignored、
  0 failed，所有 example targets passed）；`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`；
  `git diff --check`。

### M5-R [TODO] Milestone 5 Review

**前置依赖**：M5-1 至 M5-4 全部完成。

**上下文**：持久化是另一条可能绕过 public commit API 的输入路径；Review 必须把 snapshot、
rows、restore validator 与 live effective view 作为一条完整信任边界审查。

**做什么**：

- 对照规范 §10 审查 serde/data 边界、pending 一致点、parent-tree rows、immutable insert-only
  语义、外部 id/time 和 runtime resource 排除。
- 确认 restore 不存在绕过 I1--I4/Boundary/Projection 校验的入口，所有 derived index 都从事实
  重建；未知 schema 明确失败。
- 人工追踪一次 fork+compaction 会话的 JSON/rows，确认共享祖先没有复制或重分配 id，恢复后
  effective view 与原对象一致。

**验证**：

- 运行全部 corruption 与存盘→恢复→effective_view 测试；检查 public persistence rustdoc 对
  数据库集成方足够明确且不承诺未实现的 driver。
- 依次通过 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、
  `cargo test --all --all-targets`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` 和
  `git diff --check`。

---

## Milestone 6 — 跨功能验收与文档

### M6-1 [TODO] Conversation 状态机组合验收

**前置依赖**：M5-R。

**上下文**：单模块测试可能漏掉 transition 组合。需要从公共 API 驱动 Conversation，在每个
可观察步骤检查 closed invariants，并覆盖 cancel、branch、projection 和 restore 的交互。

**做什么**：

- 新增确定性状态机/表驱动 integration tests，只通过公共 API 执行 begin、stream freeze、
  tool results、commit、cancel、boundary、revert/redo、fork、compaction、snapshot/restore。
- 每次 transition 后扫描所有 closed Turn 验证 I1--I4、message immutability、parent/head、
  effective index 和 projection；失败操作还要断言原子不变。
- 覆盖至少：parallel calls 中途 cancel 后新 feed；compacted history 内 revert 后 fork；父子
  分别 compaction/restore；stale boundary 与坏 snapshot 恢复后继续使用原会话。
- 保持测试模块化，长 fixture/helper 拆分文件；不引入 Agent loop/tool registry 模拟器，只用
  显式事件和结果驱动 Core。

**验证**：

- 全部组合场景结束后均能再次完成一个纯文本 Turn，证明无 poisoned 状态；任何时刻 closed
  history 都满足 I1--I4，raw message payload hash/id 不变。
- 单个状态机 case 少于 1 分钟，失败输出包含操作序列便于复现。
- 依次通过 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、conversation state
  machine 聚焦测试、`cargo test --all --all-targets`、
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` 和 `git diff --check`。

### M6-2 [TODO] 两家 Client request mapper 兼容性验收

**前置依赖**：M6-1。

**上下文**：Conversation 的 role/tool 语法目标是同时可交给 Anthropic Messages 与 OpenAI
Responses。验收应在同一 effective view 上调用两家现有请求构造器，不能在 Conversation
断言层写 provider 特判。

**做什么**：

- 构造包含 system、多轮、parallel tool、cancelled tool result、reasoning 和 compaction
  artifact 的 Conversation；从同一个 `EffectiveView` 组装两份仅 adapter 不同的
  `ChatRequest`。
- 使用本地 dummy endpoint 调用两家 `build_request`，断言都接受 canonical role/content
  序列、system 单列、call id 配对完整；只在 adapter wire 断言 helper 中处理协议字段差异。
- 对 `Denied/Cancelled` tool status 验证 Conversation 事实保留，同时两家 wire 按各自能表达的
  error/incomplete 语义映射；回放后不产生 orphan result。
- 增加非法 Conversation 数据无法通过 public API 构造的回归，避免靠 adapter 最后报错来
  维持 Core 不变量。

**验证**：

- 两家 adapter 对同一 view 构造请求成功且无网络；统一断言 role/content/tool pairing，wire
  专属断言局限在 adapter helper。
- 现有 Client integration/fixtures 全部回归通过，不因 Conversation 引入 provider 泄漏或
  Client API 破坏。
- 依次通过 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、跨 adapter 聚焦测试、
  `cargo test --all --all-targets`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` 和
  `git diff --check`。

### M6-3 [TODO] Conversation 示例、README 与 crate 文档

**前置依赖**：M6-2。

**上下文**：实现完成后，crate 不再只是 Client 层；文档必须清楚展示 identity 注入、
pending/commit/cancel、合法 Boundary、fork、effective view 和 persistence，一并保持 Agent/
registry/multi-agent 非目标边界。

**做什么**：

- 更新 `src/lib.rs` crate docs 与根 `README.md` 的架构、模块、状态和用法；保留 Client endpoint
  文档并把历史计划/验证链接指向归档，当前实施入口继续指向根 `PLAN.md`/`TODO.md`。
- 新增可离线运行的 Conversation 示例：显式 deterministic ids，完成 user→assistant tool use→
  tool result→final assistant→commit，演示 cancel 后继续 feed、valid Boundary/fork、compaction
  后 effective view，以及 snapshot→restore 一致性。
- 示例只模拟 normalized Client responses/events，不访问真实 endpoint；避免把 Agent loop、tool
  registry 或 compaction summarizer 冒充为本层功能。
- 为所有公共 Conversation 类型/错误/方法补齐 rustdoc 与最小可运行代码示例，说明 pending
  不能 snapshot、Boundary 可能 stale、id/time 外部注入和 fork 共享语义。

**验证**：

- `cargo run --example conversation_core` 成功并断言关键状态；所有 rustdoc examples 编译，
  README 链接有效且历史/当前计划语义正确。
- 使用 `rg` 审查陈旧“Client 层当前计划/任务”引用；不改动仍正确指向当前根计划的入口。
- 依次通过 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、示例与 doc tests、
  `cargo test --all --all-targets`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` 和
  `git diff --check`。

### M6-R [TODO] Milestone 6 / Conversation Core 总 Review

**前置依赖**：M6-1 至 M6-3 全部完成。

**上下文**：这是全部 Conversation Core 任务完成后的总验收，也是创建项目完成 tag 前最后
一道门；完成记录必须以实际全量结果为准，不能用各里程碑局部通过替代。

**做什么**：

- 逐节回溯 `docs/conversation-core.md` §0--§11 与 `PLAN.md`：确认 immutable message、Turn
  definition/I1--I4、pending/cancel、identity、Boundary/head/revert/fork、Projection、
  compaction、serde/persistence 均有实现、测试和公共文档。
- 确认 O(1) fork、raw non-destructive、cancel 后可 feed、Projection 只覆盖完整 Turn、
  存盘→恢复→effective_view 一致这五项高风险承诺都有独立可判定测试，不是注释或 workaround。
- 审查模块大小、重复逻辑、warning/TODO、公共可变性和错误 source 链；确认职责边界没有混入
  Agent loop、Tool registry、数据库 driver 或多 agent 编排。
- 将根 `TODO.md` 的 M6-R 标为 `[DONE]` 并填写全量验证记录；确认所有任务标题均已 `[DONE]`
  后按项目完成规则创建 Git tag `endtag`。

**验证**：

- 运行所有 Conversation 聚焦/组合/持久化/adapter 兼容测试与示例；人工核对每条规范约束的
  代码与测试位置。
- 依次通过 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、
  `cargo test --all --all-targets`、`cargo test --doc`、
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`、`git diff --check`；所有 case 少于 1 分钟，
  完整 suite 少于 30 分钟。

---

## Milestone 7 — 交接：归档 Conversation 计划并起草 Agent 层计划

### M7-1 [TODO] 归档 Conversation 计划并为 Agent 层编写新的 PLAN.md / TODO.md

**前置依赖**：Conversation Core 全部里程碑（M1-R 至 M6-R）已 `[DONE]`。本任务是纯文档/计划
交接，不写实现代码，也不改动 `src/` 下任何 Rust 文件。

**上下文**：Conversation 层实现收尾后，项目重心转入 `DESIGN.md` §1.3 的 Agent 层。该层的
规范性设计已落在 [`docs/agent-layer.md`](docs/agent-layer.md)（Agent 三层拆分
`AgentSpec`/`AgentState`/`AgentLoop` + `RunContext`、pivot/reconfig 两级边界、垂直功能
API-first、plan「计划板」/ blackboard「聊天群」、简单 agent + fork 编排原则）。当前根目录的
`PLAN.md`、`TODO.md` 只服务已完成的 Conversation Core，须整体归档，再为 Agent 层新建同名
计划文件。归档惯例参照现有
[`docs/archive/2026-07-13-client-layer/`](docs/archive/2026-07-13-client-layer/)（该目录内
存放上一阶段的 `PLAN.md` 与 `TODO.md`）。

**做什么**：

- **归档现有计划**：
  - 新建目录 `docs/archive/2026-07-13-conversation/`。
  - 用 `git mv` 把根目录当前的 `PLAN.md` 与 `TODO.md`（即本 Conversation Core 计划与任务清单）
    移动到该目录下，保留文件名不变、保留 git 历史。
  - 检查并修正因移动而失效的相对链接：被归档文件内指向 `docs/…`、`DESIGN.md`、`src/…` 的相对
    路径需相应加一层 `../../`（例如 `docs/conversation-core.md` → `../../docs/conversation-core.md`，
    `DESIGN.md` → `../../DESIGN.md`）；被移动文件之间的互相引用（`PLAN.md`↔`TODO.md`）保持同目录相对名。
  - 全仓库检索其他文件对根 `PLAN.md`/`TODO.md` 的引用（`rg -n 'PLAN\.md|TODO\.md' --glob '!target/*'`），
    对仍应指向「已归档 Conversation 计划」的历史性引用更新为新归档路径；对应指向「当前根计划」的
    入口留给下一步新建的文件承接。

- **为 Agent 层新建 `PLAN.md`**（放回仓库根目录，覆盖整份 Agent 层实现规划）：
  - 格式与语气对齐被归档的 Conversation `PLAN.md`：包含「范围与非目标」「规范优先级与已定关键决策」
    「里程碑总览（表格）」「建议目录与公共 API 边界」「测试策略与完成门」「每阶段结束的 Review」等小节。
  - 规范性输入指向 `docs/agent-layer.md` 与 `DESIGN.md` §1.3；显式声明复用 Conversation 层已落地
    的 `Boundary`（step / turn 两级）、committed log + pending + projection、cancel 闭合等地基，不重造。
  - 把 `docs/agent-layer.md` §8「新增需求」逐条落进「已定关键决策」：conversation 需新开
    「step 边界注入 user 消息」入口、loop 可暂停/恢复（`LoopCursor` + conversation 一起序列化）、
    `RunContext` 贯穿三层。
  - 里程碑划分需体现依赖顺序，建议自底向上：先 `AgentSpec`/`AgentState`/`RunContext` 数据骨架与
    `AgentLoop` 步进模型（feed→`AgentEvent` stream），再 pivot（step 边界注入 user 消息）、
    reconfig（turn 边界 skill/tool/prompt 变更）、审批与 cancel 贯穿，随后垂直功能（skill/mcp、
    plan、blackboard、agent 调度原语），最后跨功能验收 + 文档 + 总 Review。以文件锚定的实际设计为准，
    不臆造 `docs/agent-layer.md` 未包含的机制。

- **为 Agent 层新建 `TODO.md`**（放回仓库根目录），任务须满足以下全部要求：
  - **按实现顺序编号**：`Mx-y` 形式，`M1-1` 表示 milestone 1 的第一个任务，`M1-2`、`M2-1` 依此类推，
    顺序即真实依赖顺序。
  - **标题带 `[TODO]` 标记**：每个任务标题形如 `### M1-1 [TODO] 简述`，与被归档 TODO.md 一致，供
    coding agent 识别未完成任务（完成后改为 `[DONE]`）。
  - **每个任务自带足够上下文**：沿用 `前置依赖 / 上下文 / 做什么 / 验证` 四段结构；「上下文」引用
    `docs/agent-layer.md` 的具体小节与 `src/conversation/` 中将被复用/扩展的具体类型（如
    `Conversation::begin_turn`、`PendingTurn`、`Boundary`），让实现者无需反复搜索代码库。
  - **每个任务定义完整验证条件**：至少包含针对性单测/集成测试描述，以及命令序列
    `cargo fmt --all` → `cargo clippy --all-targets -- -D warnings` → 聚焦测试 →
    `cargo test --all --all-targets` → `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` →
    `git diff --check`；单测 < 1 分钟、全量 < 30 分钟的时限约束照旧。
  - **每个里程碑末尾加一个独立 review 任务**：编号 `Mx-R`、标题带 `[TODO]`，职责是核对本阶段设计
    约束、公共 API 封装、错误边界、测试与 rustdoc 的正确性与完整性，并确认下一阶段前置条件真实满足；
    review 任务不得替代实现任务。
  - 顶部保留与被归档 TODO.md 一致的「通用约束」说明段与指向新 `PLAN.md`、`docs/agent-layer.md` 的引用。

- 完成后把本任务标题的 `[TODO]` 改为 `[DONE]` 并补写完成记录（列出归档路径、新文件里程碑数量与
  任务数量）。

**验证**：

- `git status` 显示：`docs/archive/2026-07-13-conversation/PLAN.md`、
  `docs/archive/2026-07-13-conversation/TODO.md` 为移动而来（`git log --follow` 保留历史），
  根目录 `PLAN.md`、`TODO.md` 为新建的 Agent 层版本。
- `rg -n 'PLAN\.md|TODO\.md' --glob '!target/*'` 审查：无指向根 `PLAN.md`/`TODO.md` 的失效引用；
  被归档文件内的相对链接经手工点击/`ls` 核对均有效。
- 新根 `PLAN.md` 覆盖 `docs/agent-layer.md` 的全部主要决策，里程碑总览表存在且标注依赖顺序；
  新根 `TODO.md` 每个任务标题含 `[TODO]`、编号连续、含四段结构与完整验证命令，且每个里程碑均以
  独立 `Mx-R` review 任务收尾。
- Markdown 无断链、无残留 `Conversation Core` 专属措辞误入 Agent 层文件；`git diff --check` 通过。
- 本任务仅改动 Markdown 与目录结构，不触碰 `src/`、`Cargo.toml`、`tests/`；`cargo fmt --all` 与
  `cargo test --all --all-targets` 相对归档前无新增改动或失败。
