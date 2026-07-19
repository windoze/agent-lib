# 当前执行计划

## 约束

- 以 `TODO.md` 为唯一任务顺序与完成状态来源。
- 本轮只完成第一个未标记 `[DONE]` 的任务，然后停止。
- 如遇到阻塞当前任务的具体前置问题，先在 `TODO.md` 中插入最小必要前置任务并提交，然后停止。
- 不用历史问题泛扫替代当前任务选择。
- 不记录隐藏推理链；本文件记录可审阅的执行计划、决策与进度。

## 步骤

1. 读取 `TODO.md`，定位第一个标题未带 `[DONE]` 的任务。
2. 查看最近提交信息，判断是否明确提到与该任务直接相关的未完成事项。
3. 阅读当前任务相关的代码、文档与测试，确认验收要求和依赖。
4. 以最小正确改动实现任务；如任务本身无法继续，则更新 `TODO.md` 记录前置任务并停止。
5. 按要求运行格式化、lint 和相关测试；若发现未排期失败测试，修复或在 `TODO.md` 中排入当前任务完成前必须处理的任务。
6. 更新 `TODO.md`，在完成任务标题前添加 `[DONE]` 并补充完成记录；仅当阶段计划实际变化时更新 `PLAN.md`。
7. 检查工作区差异，提交本轮全部相关更改。
8. 停止，不进入下一个任务。

## 进度

- 已写入初始执行计划。
- 已读取 `TODO.md` 并定位首个未完成任务：`M8-3 [TODO] M8 review：收敛收口`。
- 最近提交标题为 `[M8-2] Consolidate external process helpers`；下一步检查完整提交信息是否包含与 M8-3 直接相关的未完成事项。

## 当前任务计划：M8-3 review 收口

1. 检查最近提交完整信息，确认是否有直接相关的未完成问题需要纳入 M8-3。已完成：最近提交只有 `[M8-2] Consolidate external process helpers`，无未完成事项说明。
2. 抽查 M8-1/M8-2 关键收敛点：公共 HTTP/SSE/request/helper 模块、external process 共享模块，以及重复实现是否仍有残留。已完成：`src/adapter/common/` 与 `src/agent/external/process/` 在场并已被 adapter 调用；重复实现 grep 只剩公共模块与 runtime-specific 薄封装。
3. 核对 `docs/managed-external-agent.md` 与 `AGENTS.md` 是否已描述新增公共模块位置；如缺失则补文档。已完成：补充 `adapter/common/`、`agent/external/process/`，并把旧 `process_group` 路径更新为 `process::group`。
4. 按任务要求运行全量门禁：`cargo fmt --all`、默认 clippy、external features clippy、`cargo test --all --all-targets`、rustdoc。已完成且全部通过；额外运行 `cargo test --features "external-claude-code external-codex external-opencode external-acp" --all-targets` 也通过。
5. 更新 `TODO.md`，将 M8-3 标题标为 `[DONE]` 并写入完成记录。已完成。
6. 检查工作区差异，提交本轮变更，然后停止。正在进行。
