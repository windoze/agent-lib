# 当前任务执行计划

> 本文件记录可审计的执行计划、决策依据和进度，不记录模型的私密逐字推理过程。

## 当前状态

- 状态：已完整读取 `TODO.md`，已选定本次唯一任务，尚未检查实现或运行构建/测试。
- 当前任务：`M2-4 Cancel 裂缝闭合与“cancel 后仍可 feed”`。
- 前置依赖：`M2-3` 已由 `TODO.md` 明确标记 `[DONE]`；本次不进入后续 `M2-R`。
- 任务选择原则：以 `TODO.md` 中标题未带 `[DONE]` 的第一项为本次唯一执行任务。
- 范围原则：仅处理该任务，以及会阻塞或使该任务指定行为失效的直接问题；不进行开放式历史缺陷扫描。

## 分步执行计划

1. 完整读取 `TODO.md`，定位第一项标题未带 `[DONE]` 的任务，记录其要求、依赖、验收命令与完成记录格式。
2. 检查最新提交信息是否明确提到与该任务直接相关的未完成问题；同时检查工作区状态，识别并保护已有未提交改动。
3. 只读取完成当前任务所需的 `PLAN.md`、设计文档、源码和测试，建立任务与现状之间的差距清单。
4. 若发现一个具体、无法绕过且未被跟踪的前置阻塞：在 `TODO.md` 中插入最少数量的前置任务、补明依赖，必要时才更新阶段级 `PLAN.md`，提交该记录后停止。
5. 若无阻塞：按任务原定边界完整实现；采用小而聚焦的补丁，修改后复读相关代码，避免改变无关文件或覆盖用户改动。
6. 补充或调整测试与必要文档，覆盖任务规定的正常路径、边界条件、错误路径和不变量；不以窄化测试或特殊分支规避规格。
7. 按要求依次验证：`cargo fmt --all`，然后 `cargo clippy --all-targets -- -D warnings`，最后在不超过 30 分钟的超时约束下运行 `cargo test --all --all-targets`；再执行任务条目指定的额外验证和文档构建（若有）。任何未被后续任务明确安排的失败都必须修复或登记为前置任务。
8. 验证全部通过后，在 `TODO.md` 对当前任务标题加 `[DONE]` 并写明实现与验证记录；只有阶段计划发生实质变化时才更新 `PLAN.md`。
9. 复查差异、工作区状态与任务完成条件，确认本次只完成一个任务，且没有遗漏意外变更（包括 `PROMPT.md`）。
10. 使用包含任务编号的清晰提交信息提交本次全部应纳入的改动；核对提交成功和工作区状态，然后停止，不开始下一任务。

## 进度日志

- 已完成：在运行任何仓库命令之前建立本计划文件。
- 已完成：完整读取 `TODO.md`，确认此前任务标题均为 `[DONE]`，首个未完成标题为 `M2-4 [TODO]`。
- 当前验收重点：三种 `CancelDisposition`；活跃 partial 必须整体丢弃；Resume/Commit 必须以
  `ToolStatus::Cancelled` 原子闭合所有已冻结 open calls；三种成功路径之后均能继续 feed；
  失败路径保持 committed history 与 pending 原子一致；最终复用唯一 Turn validator。
- 已完成：检查最新提交与工作区。`HEAD=c451136 [M2-3]` 未声明相关遗留问题；开始时工作区
  干净，当前仅本计划文件是新增改动。
- 已完成：读取规范 §5、阶段计划和现有 pending/validator/API/测试夹具。确认 cancel 必须覆盖
  `AssistantInProgress`、尚未 mapping 的冻结 tool-use、已 mapping 的 open calls，以及已有
  部分 parallel result 的状态。
- 实现决策：新增强类型取消结果输入，显式携带 provider call id、外部 `ToolCallId` 和外部
  result `MessageId`。对未 mapping 的 call，它原子建立 mapping；对已 mapping 的 call，它
  必须与既有内部 id 精确匹配。缺项、多项、未知项和任何 identity 冲突都分类拒绝。
- 原子性决策：先从只读 pending 构造 data-only 候选和固定的 interruption result（状态为
  `Cancelled`，内容明确说明执行被取消），不读取或 finish 活跃 accumulator。`ResumeTurn`
  仅在全部校验完成后一次性追加候选并把状态置回 `AwaitingAssistant`；`CommitTurn` 使用独立
  完整 `Response` 冻结最终 assistant、构造完整 `TurnData` 并先通过唯一 `commit_draft`
  validator，成功后才清空原 pending。失败时原 partial/pending 与 committed history 均不变。
- 测试计划：按 success/error 拆分 cancel 聚焦测试，覆盖三种 disposition、纯文本 partial、
  三段 tool JSON partial、mapping 前取消、执行中取消、parallel 部分完成后取消、无 pending、
  重复/缺失/未知结果、call/message id 冲突、非法最终 role/tool-use/content、validator 原子失败，
  并在每种成功路径后完成新的 feed→commit。
- 已完成：新增 `pending/cancel.rs`，公开 `CancelDisposition`、`CancelledToolResult`、
  `CancelOutcome`、`CancelError` 与 `Conversation::cancel_pending`；接入 pending 内部的单步
  Resume 应用点和原有唯一 `commit_draft` validator，没有新增 raw history 写入口。
- 已完成：新增 14 个模块化 cancel 测试，覆盖三种 disposition、两类 partial、mapping 前后、
  parallel 部分完成、四态结果持久化、pending/history 原子性、conversation-wide identity、
  重复 provider id、非法 final response 与 validator 失败后重试；初次聚焦运行 12 项通过，
  随后补充的 2 项将纳入最终聚焦运行。
- 已完成：严格 clippy 首次指出 `CancelDisposition` variant 尺寸差异；未压制 lint，改为 boxed
  低频 Commit 载荷并提供 `commit_turn` 构造器。修正后 `cargo clippy --all-targets -- -D warnings`
  已通过。README、crate docs 与 conversation 模块说明已同步 cancel 语义和示例。
- 已完成一次全量验证：`cargo fmt --all`；严格 clippy；14 个 cancel 聚焦测试；35 个 pending 测试；
  1800 秒硬上限内全量测试（209 个库测试与 3 个离线集成测试通过，7 个真实 endpoint 测试
  ignored，example targets 全部编译）；doc tests（1 个正向、9 个 compile-fail）；
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`；`git diff --check`。
- 复查调整：暂存审查发现新增实现/错误测试单文件分别达到 532/525 行。按模块化约束将
  data-only cancellation preparation 拆到 `pending/cancel/prepare.rs`（主模块与 prepare 各约
  270 行），并将错误测试拆成 state/identity/final-response 三个文件。语义与测试集合不变，
  但这是编译输入变化，因此上一轮绿色结果只作历史记录，必须重新完整验证。
- 已完成：`TODO.md` 中仅将 `M2-4` 标为 `[DONE]` 并写入实现/测试记录；下一项 `M2-R` 保持
  `[TODO]`。阶段级依赖、顺序和完成标准未变化，因此未修改 `PLAN.md`。
- 已完成结构拆分后的最终验证：format、严格 clippy、14 个 cancel、35 个 pending、1800 秒
  上限全量测试（209 个库测试 + 3 个离线集成测试 passed、7 ignored、examples 编译）、
  doc tests（1 正向 + 9 compile-fail）及 `-D warnings` rustdoc 全部通过。
- 当前状态：`M2-4` 为 `[DONE]`、`M2-R` 为 `[TODO]`；仅完成记录因最终模块布局做了 Markdown
  修正，未再改变编译输入，尚未创建提交。
- 已完成：重新暂存全部本任务改动；staged whitespace check 通过；文件清单仅含本任务的
  18 个实现、测试、README/TODO/计划记录文件；主实现与测试错误域均已拆至聚焦文件。
- 下一步：使用 `[M2-4]` 描述性消息提交；确认提交与工作区状态后停止，不执行 `M2-R`。
