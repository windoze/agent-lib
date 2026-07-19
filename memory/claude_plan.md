# 当前执行计划

## 约束

- 以 TODO.md 为任务顺序、要求、依赖、验证和完成记录的唯一权威来源。
- 只完成第一个标题未标记 `[DONE]` 的任务，完成后停止。
- 若遇到阻塞当前任务的真实前置问题，优先修复；无法直接修复时，在 TODO.md 中插入最小必要前置任务并停止。
- 不使用规避、降级或改变规格的方式推进任务。
- 变更完成后按要求更新 TODO.md，必要时才更新 PLAN.md。
- 代码变更需要先格式化和 lint，再运行相关测试；完整测试套件超时时间不超过 30 分钟。
- 完成或阻塞记录都需要提交 Git commit。

## 步骤

1. 读取 TODO.md，定位第一个标题未带 `[DONE]` 的任务。
2. 检查最新提交是否明确提到与该任务直接相关的未完成问题；若有，将其纳入当前任务或作为前置任务记录。
3. 读取该任务相关文档、源码和测试，确定任务边界与验证要求。
4. 按任务要求做最小且完整的实现；若发现必须先修的具体阻塞问题，更新 TODO.md 和本计划后提交并停止。
5. 运行 `cargo fmt --all`。
6. 运行 `cargo clippy --all-targets -- -D warnings`，如果涉及外部适配器特性，再运行对应 feature 的 clippy。
7. 运行任务要求的相关测试；若无更窄验证足够，则运行 `cargo test --all --all-targets`，超时不超过 30 分钟。
8. 修复所有观察到且未被明确排期的失败测试；若不能在当前任务中修复，按规则在 TODO.md 插入前置任务并停止。
9. 将当前任务标题加 `[DONE]`，更新完成记录，说明实现内容和验证命令。
10. 检查 git 状态和 diff，提交所有本轮相关变更，提交信息包含任务编号和简短说明。
11. 停止，不开始下一个任务。

## 当前状态

- 已读取 TODO.md，首个未完成任务为 `M9-3 [TODO] 性能小项批`。
- 最新提交 `[M9-2] Polish facade and model APIs` 未提示与 M9-3 直接相关的未完成事项。
- 已完成 M9-3 主要代码改动：trace 节点 id 索引化、plan add_task 虚拟插入环检测、History message id 索引、rows insert diff 借用校验、Agent 运行期工具/声明共享 Arc 切片、OpenAI stream terminal 校验后释放 item 缓存。
- 已补轻量回归/计数断言：trace 1000 节点记录、plan 坏快照环防御、history 512 turn message id 重复检测。
- 已完成验证：`cargo fmt --all`、默认与 external feature clippy、定向 history 回归、默认全量测试、external feature 全目标测试、rustdoc 门禁均通过。
- 已更新 TODO.md，将 M9-3 标记 `[DONE]` 并写入完成记录；已同步 `docs/review-2026-07.md` 的 M-CONV-4 与性能清单状态。
- 下一步：检查 diff / status / log，提交本轮变更后停止。

## 当前任务 M9-3 初步执行计划

1. 检查最新提交与当前 worktree 状态，确认是否有直接关联的未完成事项或并发变更。
2. 阅读 M9-3 涉及的代码点：trace 节点去重、collab plan 环检测、facade run 工具声明拷贝、conversation message id 查找、rows diff 校验、OpenAI stream normalizer raw 保留。
3. 对每个小项做最小正确处理：能低风险优化的直接实现并补测试/计数断言；较大且会影响进度的项按任务要求显式记录暂不优化理由，必要时在 TODO.md 单列后续任务。
4. 更新相关文档或完成记录，运行格式化、lint、测试和 doc 验证。
5. 将 M9-3 标记 `[DONE]`，写入完成记录并提交。
