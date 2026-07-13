# 当前任务执行计划

## 状态

- 当前处于初始化阶段；尚未执行任何仓库读取、构建、测试或 Git 命令。
- `TODO.md` 是任务选择、顺序、依赖、验收与完成记录的唯一事实来源。
- 本次调用只处理 `TODO.md` 中标题未以 `[DONE]` 开头的第一个任务；完成或登记阻塞前置任务并提交后立即停止。
- 本文件记录可复核的执行依据、关键判断、计划、进度和验证结果。它不记录模型的私密逐字思维链。

## 执行依据与边界

1. 首先读取 `TODO.md`，按文档顺序定位第一个未完成任务，不做开放式历史缺陷扫描，也不跳过 review（`*R`）任务。
2. 读取该任务明确引用的设计、代码和测试；随后查看工作区状态与最新提交，只判断是否存在与当前任务直接相关的未完成事项或上次中断遗留。
3. 既有缺陷仅在阻塞当前任务、使当前任务指定行为失效，或由本次修改直接引入回归时纳入当前范围。无关改动属于用户资产，必须保留。
4. 默认完整实现现有任务，不因规模或不便拆分。只有出现无法绕开的具体前置缺口时，才在 `TODO.md` 中加入最少数量的前置任务、明确依赖、提交并停止。
5. 不以缩窄表示、特例、shim 或测试规避代替规范实现；同一根因明确影响一类场景时，修复整个已识别类别并补充覆盖。
6. `PLAN.md` 只在阶段级顺序、依赖、假设或完成标准发生变化时更新，不作为日常执行日志。

## 分步计划

1. **选择任务**
   - 完整读取 `TODO.md`，找到第一个标题未带 `[DONE]` 的任务，摘录其要求、依赖、验证命令和完成记录模板。
   - 检查最新提交信息及工作区状态，确认是否为中断任务续作，并识别仅与当前任务直接相关的遗留问题。
   - 将选定任务、范围判断与初始证据更新到本文件。

2. **建立实现上下文**
   - 阅读任务直接引用的 `PLAN.md`/`DESIGN.md` 章节、相关源文件和现有测试。
   - 搜索公共 API、调用点、序列化边界与测试覆盖，形成具体修改清单；如发现真正的阻塞前置缺口，按规则更新 `TODO.md` 后提交并停止。

3. **实现当前任务**
   - 使用多个小而聚焦的补丁逐步修改，补齐模块/函数用途注释，并在每个关键步骤后复读受影响区域。
   - 保持 provider-neutral、identity、不可变边界、validator/原子性等项目既有设计约束；不覆盖或回退用户的无关改动。
   - 每完成一个关键步骤或改变方案，就更新本文件的进度、决策和下一步。

4. **测试与修复**
   - 先运行与当前任务最直接相关的定向测试并修复失败。
   - 按要求依次运行 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets`（完整测试最长 30 分钟），以及任务指定的额外验证/文档构建。
   - 任一未被后续任务明确排期的失败都必须在本次修复，或作为最小前置/后续任务写入正确顺序；不得在仍有未处理失败时把当前任务标为完成。

5. **记录完成状态**
   - 只有实现和全部要求的验证通过后，才把当前任务标题显式改为 `[DONE]`，填写实际变更、测试命令与结果、重要设计决策的完成记录。
   - 仅在阶段计划真实变化时更新 `PLAN.md`；同步更新本文件为“等待提交”。

6. **最终审查与提交**
   - 检查 `git diff`、`git status` 和任务要求的一致性，确认没有秘密、临时产物、规避实现或遗漏文件。
   - 使用包含任务编号的清晰提交信息提交；若这是中断任务续作，将当前全部未提交文件（包括意外变更的 `PROMPT.md`）纳入同一提交，不擅自回退。
   - 核验提交后工作区状态与提交摘要，在本文件记录提交号；然后停止，不开始下一个任务。

## 进度日志

- 初始化：已建立执行计划。
- 任务选择：已完整读取 `TODO.md`；按标题顺序确认首个未完成任务是
  `M2-R [TODO] Milestone 2 Review`，其前置 `M2-1` 至 `M2-4` 均已标记 `[DONE]`。
- 本次范围：对照 `docs/conversation-core.md` §5 审计 pending 唯一可变区、单一 active
  `PendingMessage`、complete-only freeze、Client `Accumulator` 单一实现、四态 tool result
  从 model 到 adapter/cancel 的无损事实链，以及三种 cancel disposition 在全部生命周期裂缝
  下的原子性和后续可继续 feed；根据审计补充必要回归、错误/rustdoc，并完成规定的全量验证。
- 停止边界：只有 M2-R 审计、必要修复、验证、`TODO.md` 完成记录和提交；不开始 `M3-1`。
- 下一步：检查工作区与最新提交是否存在 M2-R 相关续作遗留，然后读取规范 §5、pending/cancel
  实现与测试矩阵，建立逐项审计表。

## M2-R 初步审计（进行中）

- 仓库状态：开始时仅本文件有修改；最新提交 `012386e [M2-4] Implement atomic pending
  cancellation`，没有声明与 M2-R 直接相关的未完成问题。
- 唯一可变区：`Conversation` 只有一个私有 `Option<PendingTurn>`；`PendingTurnState` 是互斥枚举，
  只有 `AssistantInProgress(PendingMessage)` 分支能持有 active message；`PendingMessageState` 的
  streaming 分支直接持有 Client `Accumulator`，Conversation 没有复制折叠实现。
- 冻结边界：stream 只经 `Accumulator::finish` 产生完整 `Response`，stream/non-stream 再共用
  assistant role 检查和 `FrozenMessage` 构造；id 仅在成功 freeze 时绑定。partial 不 serde，
  active state 没有 message getter，`PendingTurn` 只公开 frozen messages。
- tool 四态：model 以 `ToolStatus` 为唯一事实，legacy `is_error` 只在集中 migration 中出现；
  `ToolResponse` 转换、pending append 和 cancel synthetic result 都保留显式状态；Anthropic/
  OpenAI 仅在最终 request mapper 分别降级成 `is_error`/`completed|incomplete`。
- cancel 原子性：Resume/Commit 先从 frozen facts 构造 data-only candidate，完成 identity/mapping/
  freeze/validator 检查后才替换 pending 或 commit；Discard 只整体 take。active partial 从不被
  finish/parse，失败路径保留原 pending 与 committed history。
- 已识别的 review 证据缺口（不是实现 blocker）：
  1. 增加 terminal accumulator error → cancel → 新 feed → commit 回归，直接证明 poisoned
     active message 不会毒化 Conversation。
  2. 增加四态 `ToolResponse`/cancel → pending → closed Turn → serde → 两家 request mapper
     的全链路回归，证明 `Denied/Cancelled` 不靠 `extra` 且 wire 降级不改写事实。
  3. 增加紧凑 phase/disposition 审计矩阵，覆盖 Discard 对全部 phase，以及 Resume/Commit 在
     非 final phase 可恢复、`ReadyToCommit` 明确拒绝的公共契约。
- 下一步：复读测试公共夹具和 adapter request shape，设计小型 review 模块并分小补丁接入。

## M2-R 实施与聚焦验证

- 已新增独立 `pending::turn::tests::review` 模块，并接入 5 个离线 review 测试：
  - 对五种 `PendingTurnPhase` 分别执行 Discard/Resume/Commit，覆盖 15 个组合；Discard 全部
    接受，Resume/Commit 在四种非 final phase 成功，`ReadyToCommit` 分类拒绝且 pending/history
    原子不变；每条成功或拒绝后的合法路径都继续完成新 feed。
  - 构造含 partial text 的 active accumulator，再注入 provider error 进入 terminal；Resume
    整体丢弃 terminal/partial 后，同一 Turn 用 replacement assistant 完成并可继续下一 Turn。
  - 同一 parallel tool batch 通过 `ToolResponse` 写入 Ok/Error/Denied，cancel 合成 Cancelled，
    逐层核对 pending、closed Turn、normalized serde；再由 Anthropic/OpenAI request mapper
    分别降级到 `is_error` 与 `completed|incomplete`，并断言源 facts 未改变。
- 生产代码无需修改：新增证据验证了现有唯一 accumulator、freeze、cancel data-only prepare、
  validator commit 和四态 adapter 边界，未发现 spec mismatch、workaround 或未排期失败。
- 已通过：`cargo fmt --all`；`cargo clippy --all-targets -- -D warnings`；
  `cargo test conversation::pending::turn::tests::review -- --nocapture`（5 passed，0 failed）。
- 下一步：运行完整 `conversation::pending` 聚焦组；通过后进入 30 分钟上限的全量 suite、doc
  tests、`-D warnings` rustdoc 与 diff 审查。

## 最终验证与记录

- 为避免 673 行单一 review 测试文件，已将 cancel phase/terminal 审计与四态 adapter 链路拆为
  `review.rs`（404 行）和 `review/status_chain.rs`（279 行）；只有测试组织变化，语义不变。
- 拆分后的最终验证全部通过：
  - `cargo fmt --all`；
  - `cargo clippy --all-targets -- -D warnings`；
  - `cargo test conversation::pending::turn::tests::review -- --nocapture`：5 passed；
  - `cargo test conversation::pending`：40 passed；
  - `perl -e 'alarm 1800; exec @ARGV' cargo test --all --all-targets`：214 个库测试和 3 个
    离线集成测试 passed，7 个真实 endpoint 测试按设计 ignored，0 failed，全部 examples 编译；
  - `cargo test --doc`：1 个正向和 9 个 compile-fail passed；
  - `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`：passed。
- 已将 `TODO.md` 的 `M2-R` 标题显式改为 `[DONE]` 并填写审计、回归和最终验证记录；`M3-1`
  保持 `[TODO]`。阶段计划未变化，未修改 `PLAN.md`。
- 下一步：执行最终 whitespace/diff/API 范围审查，提交 `[M2-R]` 变更，核验提交与干净工作区后
  停止；不执行 `M3-1`。
