# 执行计划

## 当前约束

- 输出与进度记录使用中文。
- `TODO.md` 是任务排序、验收要求和完成状态的唯一权威来源。
- 本次只完成 `TODO.md` 中第一个标题未带 `[DONE]` 的任务，然后停止。
- 在选择任务前不做开放式历史问题排查。
- 若当前任务被具体缺陷、未建模前置条件或测试失败阻塞，则先修复该阻塞；若无法在本次直接修复，则把最小前置任务插入 `TODO.md` 并提交后停止。
- 完成任务后必须更新 `TODO.md` 的标题 `[DONE]` 与完成记录，并提交 Git。

## 初始步骤

1. 读取 `TODO.md`，按文件顺序定位第一个标题未带 `[DONE]` 的任务，包括 `*R` review 任务。
2. 查看最近提交信息，只判断它是否明确提到与该任务直接相关的未完成问题。
3. 读取当前任务要求、依赖、验收条件和相关计划上下文；仅在需要理解阶段边界时参考 `PLAN.md`。
4. 检查工作区状态，避免覆盖用户已有改动。

## 实施步骤

1. 根据首个未完成任务读取相关源码、测试和文档。
2. 制定针对该任务的最小实现方案；若发现必须新增前置任务，更新 `TODO.md`、记录原因、提交并停止。
3. 进行小步代码修改，每次关键方向变化或关键步骤完成时更新本文件。
4. 为任务行为补充或调整聚焦测试，避免用狭窄用例绕开真实规格。

## 验证步骤

1. 先运行 `cargo fmt --all`。
2. 再运行 `cargo clippy --all-targets -- -D warnings`。
3. 最后运行 `cargo test --all --all-targets`，完整测试设置不超过 30 分钟超时。
4. 对任何未被后续任务明确安排的失败测试，必须修复或在 `TODO.md` 中插入正确顺序的前置任务；不能在失败未处理时标记当前任务完成。

## 收尾步骤

1. 更新 `TODO.md`：在当前任务标题前加 `[DONE]`，补全完成记录、验证记录和必要说明。
2. 仅当阶段级计划、依赖或完成标准变化时才更新 `PLAN.md`。
3. 复查 `git status`，确认提交包含本次任务相关改动以及本次按要求维护的 `memory/claude_plan.md`。
4. 使用清晰任务编号提交 Git。
5. 提交后停止，不继续处理下一个任务。

## 当前状态

- 已写入初始执行计划。
- 已读取 `TODO.md` 并定位首个未完成任务：`M7-1 [TODO] 归档 Conversation 计划并为 Agent 层编写新的 PLAN.md / TODO.md`。
- 最近提交为 `1e22d45 [M6-R] Complete conversation core review`，未发现与 `M7-1` 冲突的未完成实现事项。
- 当前工作区只有本计划文件改动；后续将执行纯 Markdown/目录结构交接，不触碰 `src/`、`Cargo.toml` 或 `tests/`。
- 已创建 `docs/archive/2026-07-13-conversation/`，并用 `git mv` 将原根 `PLAN.md`、`TODO.md` 移入该目录。
- 读取 Agent 设计后确认：pivot 注入按 `docs/agent-layer.md` §4.1 / `DESIGN.md` §1.3 采用 `user` 消息；skill/tool/system reconfig 只在 turn 边界走配置路径，不复用 message 注入入口。
- 已新建根 `PLAN.md` 与 `TODO.md`：Agent 层计划包含 6 个里程碑；任务清单包含 26 个 `[TODO]` 任务，其中 6 个为独立 review。
- 已更新 `README.md` 当前阶段入口，并将归档 `TODO.md` 中 `M7-1` 标为 `[DONE]`。
- 验证已完成：markdown 链接存在性检查通过，`cargo fmt --all` 通过，`git diff --check` 通过；因仅改 Markdown/目录结构，未重跑 clippy、全量测试和 rustdoc。
- 已提交归档移动：`5b94288 [M7-1] Archive conversation planning docs`，其中 `PLAN.md` 与 `TODO.md` 以 rename 形式进入 `docs/archive/2026-07-13-conversation/`。
- 下一步：提交新根 Agent `PLAN.md`/`TODO.md`、README 引用更新和本进度文件。

## M7-1 具体计划

1. 读取 `docs/agent-layer.md`、`DESIGN.md` 的 Agent 层相关内容和全仓 `PLAN.md`/`TODO.md` 引用。
2. 新建归档目录并用 `git mv` 移动当前根 `PLAN.md`、`TODO.md` 到 `docs/archive/2026-07-13-conversation/`。
3. 修正归档文件中因移动失效的相对链接，并更新全仓仍应指向 Conversation 历史计划的引用。
4. 根据 `docs/agent-layer.md` 与 `DESIGN.md` §1.3 编写新的根 `PLAN.md`。
5. 根据新计划编写新的根 `TODO.md`，确保任务编号连续、每项四段结构、每个里程碑含独立 review 任务和完整验证命令。
6. 在归档后的 Conversation `TODO.md` 中将 `M7-1` 标为 `[DONE]` 并补充完成记录。
7. 执行 Markdown 链接/引用审查、`git diff --check`；由于本任务仅文档改动，若确认未改编译输出，则按任务说明跳过完整 Rust 套件并在完成记录说明。
8. 复查状态并提交本次交接改动。
