# 当前执行计划

说明：用户要求记录“思考过程”，但我不会写入私密推理链路；本文件记录可审计的执行依据、决策点和逐步计划。

## 目标

按 `TODO.md` 的权威顺序完成第一个标题未带 `[DONE]` 的任务，完成后更新任务记录、运行必要验证、提交 Git，然后停止。

## 初始步骤

1. 读取 `TODO.md`，只识别第一个未完成任务，不做开放式历史问题扫描。
2. 查看最近提交信息；如果最近提交明确提到与当前任务直接相关的未完成问题，将其纳入当前任务或作为前置任务记录到 `TODO.md`。
3. 阅读当前任务相关的 `PLAN.md`、设计文档、源码和测试，限定在完成该任务所需范围内。

## 执行策略

1. 优先按现有架构和命名实现，不改变任务边界，不为了方便拆分任务。
2. 如果发现阻塞当前任务的真实前置缺口，先把最小必要前置任务插入 `TODO.md` 并提交后停止。
3. 如果可以完成当前任务，则实现完整行为，并补充覆盖指定约束、边界和回归风险的测试。
4. 发现测试失败时，按失败测试策略处理：修复或在 `TODO.md` 中安排明确前置任务，不留下未排期失败。

## 验证计划

1. 先运行 `cargo fmt --all`。
2. 再运行 `cargo clippy --all-targets -- -D warnings`。
3. 最后运行 `cargo test --all --all-targets`，完整测试设置不超过 30 分钟超时。
4. 若本轮只改文档或注释且没有影响编译输出，则可复用最近绿色结果，并在完成记录中说明跳过原因。

## 收尾计划

1. 在 `TODO.md` 中给完成任务标题加 `[DONE]` 前缀，并补全完成记录。
2. 仅当阶段级计划、依赖或完成标准变化时才更新 `PLAN.md`。
3. 检查 `git status`，确认本轮相关修改被纳入提交；若是恢复未完成任务，则将所有未提交文件一并提交。
4. 使用清晰任务编号提交信息提交。
5. 提交后停止，不继续下一个任务。

## 进度记录

- 已创建本执行计划，下一步读取 `TODO.md` 并识别第一个未完成任务。
- 已读取任务标题列表：第一个未完成任务是 `M3-4 [TODO] O(1) fork_at 与共享 immutable 历史`。
- 最近提交为 `[M3-3] Implement logical head and revert branching`，与当前 M3-4 属于同一里程碑顺序衔接；目前未发现需要在选择任务前插入的直接未完成问题。
- 下一步读取 M3-4 正文、`PLAN.md` 对 Milestone 3 的描述和 `docs/conversation-core.md` 中 fork/history/boundary 相关规范。
- 已读取 M3-4 正文及相关规范，确认本轮不需要修改阶段计划。
- 现有 `History` 已具备共享 `Arc<Lineage>`、fork ceiling 和 scoped raw base 的测试路径；生产实现计划复用该结构，避免复制 prefix 或重新分配历史 id。

## M3-4 具体执行计划

1. 新增公开 `ForkOrigin`，字段私有，包含 `parent: ConversationId` 与 `fork_point: Boundary`，提供只读 getter、`Clone/Copy/Debug/PartialEq/Eq/Serialize/Deserialize` 和 rustdoc。
2. 给 `Conversation` 增加 `origin: Option<ForkOrigin>` 字段、`origin()` getter，并确保 `Conversation::new` 为 `None`。
3. 把 `History::shared_prefix` 从测试专用提升为 crate-private 生产方法，使 `fork_at` 可 O(1) clone 共享 handle，并把 child raw 可见集合限制为 fork 点祖先。
4. 新增 `boundary::fork` 模块，实现 `Conversation::fork_at(boundary, new_conversation_id) -> Result<Conversation, ConversationError>`：先解析 boundary，拒绝 pending/foreign/stale/越界；成功生成 child，child id/version/boundary owner 独立，origin 记录父 id 与原 boundary。
5. child 的 index 通过共享 committed backing 与 O(1) scope 设定指向 fork 点有效 prefix；父 Conversation 不变。
6. 补充 fork 聚焦测试：大历史共享指针和 raw/lineage ceiling、父子独立推进、父 boundary 不能用于 child、child boundary 不能用于 parent、父 suffix 不进入 child raw/boundaries/snapshot 可见集合、pending 阻止 fork 且保持父状态不变。
7. 更新 README/lib/conversation rustdoc 和 TODO 完成记录；只有 phase 计划改变时才改 `PLAN.md`，当前预计不需要。
8. 验证顺序：`cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、fork 聚焦测试、`cargo test --all --all-targets`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`、`git diff --check`。

## 当前执行进度

- 已实现 `ForkOrigin`、`Conversation::origin()` 和 `Conversation::fork_at(...)`。
- 已将 `History::shared_prefix` 提升为生产 crate-private fork 原语。
- 已重构 `ToolCallIndex`：committed 部分使用共享 backing，并通过可见 turn/entry 上限支持 O(1) head/fork 裁剪；pending 继续作为独立 suffix。
- 已把 `revert_to` 从索引全量 rebuild 改为 O(1) scope 裁剪。
- 已为 `ToolCallIndex` 实现只打印当前可见 entries 的自定义 `Debug`，避免 fork child 通过调试输出看到共享 backing 中的父 suffix call，并补充回归断言。
- 已新增 boundary fork 行为测试和 history fork 内部共享测试；`cargo test conversation::boundary -- --nocapture` 与 `cargo test conversation::history -- --nocapture` 均通过。
- 已完成最终验证：`cargo fmt --all`；`cargo clippy --all-targets -- -D warnings`；`cargo test fork -- --nocapture`（5 passed）；`timeout 1800 cargo test --all --all-targets`（243 个库测试与 3 个离线集成测试 passed，7 ignored，examples passed）；`cargo test --doc`（1 个正向与 10 个 compile-fail passed）；`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`；`git diff --check`。
- 已将 `TODO.md` 的 M3-4 标题标记为 `[DONE]` 并写入完成记录；`PLAN.md` 无阶段级变化，未修改。
- 下一步检查工作树和 diff，暂存本轮相关文件并创建 `[M3-4]` 提交。
