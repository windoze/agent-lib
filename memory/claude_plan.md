# 当前 invocation 执行计划

## 目标与约束

- 以 `TODO.md` 为任务顺序、需求、依赖、验证和完成状态的唯一权威来源。
- 本次只完成首个标题未带 `[DONE]` 的任务；完成并提交后立即停止，不进入下一任务。
- 不做开放式历史问题扫描；只处理会阻塞当前任务、使其指定行为无效，或由本次改动直接引入的问题。
- 若遇到无法在当前任务内正确解决的具体前置阻塞，只添加最少的前置任务、保持当前任务未完成、提交任务表变更后停止。
- 不采用规避规范的窄化实现、临时 shim 或测试绕行。

## 分步执行计划

1. 读取 `TODO.md`，按标题中的 `[DONE]` 标记识别首个未完成任务，并完整提取其需求、依赖、验收标准和完成记录要求。
2. 检查最新 Git 提交说明与当前工作区状态：
   - 仅判断最新提交是否明确提到与当前任务直接相关的未完成问题；
   - 保护用户已有改动；若是中断后恢复同一任务，则将所有当前未提交文件纳入最终原子提交；
   - 不开展无边界的历史缺陷排查。
3. 阅读当前任务直接涉及的设计文档、源码和测试，建立需求到实现/测试的对应关系；确认是否存在必须先解决的真实前置阻塞。
4. 若无阻塞，按模块边界以小而集中的补丁完整实现当前任务，并同步补充必要的单元/集成测试和文档；每个关键阶段后复读改动并更新本文件进度。
5. 先运行与改动直接相关的快速测试，定位并修复问题。任何发现的失败都按 `TODO.md` 的测试失败策略处理，不忽略既有失败。
6. 按规定顺序执行最终验证：
   - `cargo fmt --all`
   - `cargo clippy --all-targets -- -D warnings`
   - `cargo test --all --all-targets`（最长 30 分钟）
   - 当前任务若要求额外验证（例如文档构建），也一并执行。
7. 验证全部通过后，在 `TODO.md` 中给当前任务标题添加 `[DONE]`，填写准确的实现与验证完成记录；只有阶段级计划确实变化时才更新 `PLAN.md`。
8. 审查最终 diff、任务范围和 Git 状态，确认没有遗漏、秘密或无关改动；使用包含任务编号的清晰消息提交所有本次应纳入的改动。
9. 确认提交成功、工作区状态符合预期，记录最终提交和验证结果，然后停止，不处理下一个任务。

## 当前进度

- [x] 在执行其他命令前建立本计划。
- [x] 识别首个未完成任务：`M1-3 I1--I4 validator 与原子 commit 门`。
- [x] 检查最新提交及工作区状态。
- [x] 完成实现与针对性测试。
- [x] 通过格式化、严格 lint、完整测试及任务专项验证。
- [x] 更新 `TODO.md`：M1-3 标题已改为 `[DONE]` 并写入完成记录。
- [ ] 提交本次任务并停止。

## 决策记录

- 已读取 `TODO.md` 并确定当前任务；后续不会越过 M1-3 处理 M1-R 或 M2。
- 本文件记录可复核的计划、事实、决策与结果，不记录模型私有的逐字思维链。
- 初始工作区除本文件外干净；最新提交 `b1721a8`（M1-2）没有声明额外未完成故障，
  只把 DTO→live `Turn` 的唯一 validator 明确交给当前 M1-3，故无需新增前置任务。

## 当前任务验收映射（M1-3）

- 建立分类化 `ConversationError` / `CommitError`，覆盖 id 重复、provider call 配对错误、
  role/block 与首尾状态错误、未完成 content、parent 错误及非原子提交相关分类。
- 实现 canonical Turn 状态机：外部 user 起始，assistant/tool 轮次及 parallel tool results
  严格闭合，最终为不含 tool use 的 assistant，closed Turn 禁止 system。
- 将 content block 中的 provider call id 与显式 `ToolPairing` 双向核对，拒绝 orphan、
  dangling、重复消费与跨 Turn 引用。
- 让内存 draft 与 serde `TurnData` 共同通过唯一 validator，禁止 pending/partial 状态进入
  live `Turn`。
- 实现空 `Conversation` 和 crate-private draft commit；先在临时状态完整校验，成功后一次性
  推进 history/version，任何失败都保持原对象全结构不变。
- 正向测试：纯文本、单次 tool、串行多轮、parallel calls，并逐项验证 I1--I4。
- 负向表驱动测试：全部指定错误类别，并对失败前后 Conversation 做全结构相等断言。

## 已识别的实现顺序

1. 审查 M1-2 的 `Turn`/DTO、Conversation 模块导出及 content/message/tool 模型。
2. 对照规范文档中 I1--I4 与 commit/restore 章节，固定 validator 的精确语义。
3. 先定义错误与受检 draft/DTO 转换边界，再实现纯 validator，最后接入原子 Conversation
   commit，避免出现第二条 unchecked 构造路径。
4. 用模块内测试覆盖 crate-private 入口，用公共 API/compile-fail 文档测试钉住外部不可绕过
   的边界。

## 规范审阅后的接口决策

- 新增公开、只读的 `Conversation`：由外部 `ConversationId` 与 `ConversationConfig` 创建，
  暴露 id/config/closed turns/version getter；不在 M1-3 提前公开 raw commit。
- `Conversation::commit_draft` 保持 crate-private。它先构造完整的提交计划，validator 成功
  且 version 可推进后才修改 `turns` 与 `version`，从而让所有错误路径保持对象全结构相等。
- `ConversationError` 负责操作层失败，`CommitError` 负责候选 Turn 的分类化语义失败；
  version 溢出单列为无法原子推进的错误。
- `validation` 是唯一把 `TurnData` 转成 live `Turn` 的入口。validator 生成字段不可由其他
  模块构造的 validated token；`Turn` 只消费该 token，不提供 raw crate-private constructor。
- canonical role/block 采用两家 adapter 的公共可表达子集：User 只含 text/image，
  Assistant 只含 text/thinking/tool-use，Tool 只含 tool-result，System 禁止；tool-result 的
  内层输出只允许 text/image。
- DTO 增加默认不出现在 closed serde 形状中的显式 completion marker。完整 Client
  `Message` 仍允许合法 JSON `null`；只有 marker 为 complete 才可提交，因此 pending JSON
  不能靠写成 `Value::Null` 绕过 I3。

## 基线验证

- `cargo test conversation`：通过（15 passed，1 个需真实凭据的 normalization 测试按既有
  配置 ignored；0 failed）。

## 实现进度记录

- 已新增公开分类错误 `ConversationError` / `CommitError`，细分 turn/message/tool-call id、
  provider call、role/block、首尾、partial、parent、pairing reference 与原子 version 推进。
- 已新增 `Conversation` 空实例、只读 getter、私有 history/version 和 crate-private
  `commit_draft`；所有可失败检查发生在修改 state 之前。
- 已实现唯一 `validation` 门：canonical user→assistant→tool*→assistant 状态机，显式 pairing
  与 content 双向核对，conversation-wide I4、cross-turn reference 及完整性 marker 校验。
- `Turn` 现在只能消费 validator 私有字段的 certificate；旧 M1-2 fixture 也已改走 validator，
  不再直接写 live `Turn` 字段。
- 测试已拆分为 fixture/positive/negative/atomic 模块，覆盖纯文本、单次/串行/并行调用、
  serde 同门恢复、完整 JSON null、全部指定负例及每个错误路径的全结构原子性。
- 为保持 M1-2 `provider_call_id: Option<String>` 的既定模型，缺省 provider id 只在
  call/result message anchors 唯一确定同一 content id 时接受；歧义输入分类拒绝，closed
  pairing 保留原始 `None`。正反例均已覆盖。
- 按文件规模要求把 validator 拆为 completion/identity、role/content sequence、pairing
  三个聚焦模块；负向测试再按 state/content/identity/pairing/serde 拆分，重构后聚焦测试
  保持通过。
- README 与 crate docs 已更新当前能力；新增 compile-fail 文档测试钉住只读 history。

## 当前验证结果

- `cargo check --lib`：通过，0 warnings。
- `cargo test conversation`：最终通过（33 passed，0 failed；1 个真实 endpoint 测试
  ignored）。
- `cargo test conversation::validation -- --nocapture`：通过（17 test functions，0 failed）。
- `cargo clippy --all-targets -- -D warnings`：通过，0 warnings。
- `cargo test --all --all-targets`：通过（163 个库单测与 3 个离线集成测试 passed，
  7 ignored，0 failed；30 分钟硬上限内完成）。
- `cargo test --doc`：通过（1 个正向与 6 个 compile-fail）。
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`：通过。
- 实现验证后仅更新 `TODO.md`/本进度文件；按策略无需因此重跑完整 suite。
- 最终 `git diff --cached --check`：通过；暂存清单共 22 个文件，均属于 M1-3 实现、测试、
  README/TODO 与本进度记录，未包含 `PLAN.md`、`PROMPT.md` 或无关改动。
