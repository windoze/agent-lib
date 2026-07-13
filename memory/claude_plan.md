# 执行计划

## 当前约束

- 以 `TODO.md` 为唯一任务顺序和完成状态来源。
- 只完成第一个标题未带 `[DONE]` 的任务，完成后提交并停止。
- 在开始任何代码检查、构建或测试前，先维护本文件作为可审阅的计划与进度记录。
- 记录可公开的执行计划、关键决策和进度；不记录私有推理链。

## 初始步骤

1. 读取 `TODO.md`，按文件顺序找到第一个标题未带 `[DONE]` 的任务。
2. 查看该任务的正文、依赖、验证要求和完成记录；必要时只读取与该任务直接相关的 `PLAN.md` 或源码上下文。
3. 检查当前 git 状态，确认是否存在未提交改动；对用户已有改动只读取和配合，不回退。
4. 若最新提交明确提到与当前任务直接相关的未完成问题，将其纳入当前任务或作为前置任务写入 `TODO.md`。

## 执行步骤

1. 根据任务要求定位相关模块和测试。
2. 做最小但完整的实现，不通过缩小范围或临时绕过规避规格问题。
3. 为当前任务新增或更新聚焦测试；若发现阻塞当前任务的规格缺口，先修复或在 `TODO.md` 插入最小前置任务后停止。
4. 运行验证，顺序为：
   - `cargo fmt --all`
   - `cargo clippy --all-targets -- -D warnings`
   - `cargo test --all --all-targets`，完整测试超时不超过 30 分钟
5. 若验证发现未明确排期的失败测试，修复或在 `TODO.md` 中排入合适任务，不能把当前任务标记完成。
6. 任务完成后，在 `TODO.md` 对任务标题加 `[DONE]`，更新完成记录；只有阶段级计划改变时才更新 `PLAN.md`。
7. 提交所有与本次任务相关的改动，提交信息包含任务编号和简明说明。
8. 停止，不继续下一个任务。

## 进度记录

- 已建立本计划文件，下一步读取 `TODO.md` 识别当前任务。
- 已读取 `TODO.md`，首个未完成任务是 `M6-1 [TODO] Conversation 状态机组合验收`。

## 当前任务：M6-1 Conversation 状态机组合验收

### 任务目标

- 新增确定性状态机/表驱动 integration tests，只通过公共 API 驱动 Conversation。
- 覆盖 begin、stream freeze、tool results、commit、cancel、boundary、revert/redo、fork、
  compaction、snapshot/restore 等组合 transition。
- 每次 transition 后验证 closed history 不变量、message immutability、parent/head、
  effective index 和 projection；失败操作需验证原子不变。
- 至少覆盖：
  - parallel calls 中途 cancel 后新 feed；
  - compacted history 内 revert 后 fork；
  - 父子分别 compaction/restore；
  - stale boundary 与坏 snapshot 恢复失败后原会话继续可用。

### 计划步骤

1. 检查最新提交与 git 状态，确认是否存在直接影响 M6-1 的未完成问题或未提交改动。
2. 阅读 `src/conversation` 的现有测试结构，优先复用 persistence/projection/pending fixtures，
   保持新增组合验收模块化，避免复制大段 fixture。
3. 设计一个公共 API 驱动的状态机测试 helper：
   - 提供 deterministic ids 与 normalized `Response`/`StreamEvent` 构造；
   - 在每个 transition 后检查 I1--I4、parent/head、index lookup、projection/effective view；
   - 记录操作序列，便于失败输出复现。
4. 实现 M6-1 要求的组合场景测试：
   - parallel tool calls cancel/resume 或 cancel/commit 后继续 feed；
   - compaction 后 revert 到 cover 内并 fork，确认无未来摘要泄漏；
   - parent/child 各自 compaction、snapshot/rows restore 后保持隔离；
   - stale boundary、伪造/损坏 snapshot restore 失败后原 Conversation 仍可继续纯文本 Turn。
5. 运行格式化、clippy、聚焦测试、全量测试、rustdoc 和 diff check。
6. 通过后将 `TODO.md` 的 M6-1 标题改为 `[DONE]` 并补完成记录。
7. 提交本次所有相关改动并停止。

### 当前进度

- 已新增 `tests/conversation_state_machine.rs`，使用 crate 公共 API 构造 deterministic
  Conversation 状态机组合验收。
- 测试覆盖 parallel tool cancel/resume、stream freeze、compaction/revert/fork、父子独立
  compaction + JSON/rows restore、stale boundary 和坏 snapshot restore 失败后的继续使用。
- 已修正测试 helper 对 pending 状态下 Boundary / projection range 消费规则的断言：pending 时
  这些消费入口应分类拒绝，而不是成功验证。
- 验证已通过：`cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、
  `cargo test --test conversation_state_machine -- --nocapture`（3 passed）。
- 已通过 1800 秒硬上限内 `cargo test --all --all-targets`：287 个库测试、3 个
  `capability_escape_hatches` 集成测试、3 个新增 `conversation_state_machine` 集成测试通过；
  7 个真实 endpoint 测试按环境要求 ignored；examples test targets 编译通过。
- 已通过 `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` 与 `git diff --check`。
- 已将 `TODO.md` 中 `M6-1` 标题改为 `[DONE]`，并补写完成记录。
- 根据任务的模块化要求，已把状态机场景入口保留在 `tests/conversation_state_machine.rs`，
  并将 fixture/helper 与断言分别拆到 `tests/conversation_state_machine/support.rs` 和
  `tests/conversation_state_machine/assertions.rs`。
- 拆分后已重新通过：`cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、
  `cargo test --test conversation_state_machine -- --nocapture`（3 passed）。
- 拆分后已重新通过 1800 秒硬上限内 `cargo test --all --all-targets`、
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` 与 `git diff --check`。
