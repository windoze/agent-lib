# 执行计划

## 当前状态

- 入口要求：以 `TODO.md` 为唯一任务排序与完成状态来源，每次只完成第一个未标记 `[DONE]` 的任务，然后提交并停止。
- 本文件会记录可审查的执行思路、步骤计划、关键进展和计划变更；不作为 `TODO.md` 或 `PLAN.md` 的替代来源。
- 在读取仓库状态和任务文件之前，尚未确认当前第一个未完成任务。

## 初始步骤

1. 读取 `TODO.md`，严格按标题是否带 `[DONE]` 判断第一个未完成任务。
2. 查看最近提交信息，只有在最近提交明确提到与该任务直接相关的未完成问题时，才把它纳入当前任务或作为前置项写入 `TODO.md`。
3. 针对当前任务读取必要的 `PLAN.md`、相关源码、测试和文档，避免开放式历史问题扫查。
4. 判断任务是否可以作为既有 `P*-Txx` / `P*-TxxR` 单元完成；除非出现具体不可绕过的前置阻塞，否则不拆分任务。
5. 按任务要求实现变更；若发现阻塞当前任务的规格缺口或测试失败，优先修复，或把最小前置任务插入 `TODO.md` 后提交并停止。
6. 运行验证，顺序为 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、必要时再运行 `cargo test --all --all-targets`，完整测试超时不超过 30 分钟。
7. 更新 `TODO.md`：完成时必须在任务标题前加 `[DONE]`，并补充完成记录；只有阶段级计划真的变化时才更新 `PLAN.md`。
8. 提交所有与当前任务相关的变更；如果是恢复未提交任务，则把当前未提交文件一并纳入提交。
9. 停止，不继续处理下一个任务。

## 当前任务

- 第一个未完成任务：`M1-R [TODO] Milestone 1 Review`。
- 任务性质：审查 Milestone 1 已完成的 `AgentSpec`、`AgentState`、`RunContext`、`LoopCursor`
  是否符合 `docs/agent-layer.md` §1、§8 与 `PLAN.md` 的关键决策；确认 runtime/data 边界、
  唯一活动 `Conversation`、外部注入 id/时间、子 context 继承父 budget/cancel/trace，以及未提前暴露
  unchecked loop 推进路径。
- 当前任务验证范围：全部 M1 聚焦测试、格式化、严格 clippy、全量测试、rustdoc、diff check；
  同时人工映射 serde/runtime 分离边界并更新 `TODO.md` 完成记录。
- 是否需要更新 `PLAN.md`：默认否，除非 review 发现阶段级计划、依赖或完成标准需要调整。

## 本任务步骤

1. 查看最近提交信息，仅纳入与 `M1-R` 直接相关的未完成问题。
2. 读取 `PLAN.md`、`docs/agent-layer.md` §1/§8，以及 M1 相关源码与测试。
3. 审查公开 API 与 serde/data shape，确认 runtime handles 不进入持久化形状，`AgentState`
   只持有唯一活动 `Conversation`，`RunContext` 子 context 不能绕过父级限制。
4. 梳理 M2 `AgentLoop` 可依赖的 public / crate-private API 清单，确认没有 unchecked loop 推进入口。
5. 如发现阻塞 M1-R 的规格缺口，优先修复；若需要新增前置任务，则插入 `TODO.md`、提交并停止。
6. 通过验证后，把 `M1-R` 标题标记为 `[DONE]`，补充完成记录与 review 结论，提交本次变更并停止。

## 进展记录

- 已定位当前任务为 `M1-R [TODO] Milestone 1 Review`。
- 最近提交 `3a19a94 [M1-3] Add agent state and loop cursor` 未声明与 M1-R 直接相关的未完成问题。
- 已读取 `PLAN.md` 与 `docs/agent-layer.md` §1、§8，以及 M1 的 `agent` 源码和测试。
- 已发现并修正 `docs/agent-layer.md` 开头与 §8 中同 §4.1/§4.2、`PLAN.md` 关键决策不一致的文字：
  pivot 注入入口只用于 `user` 消息，skill/tool set/system prompt 变更属于 turn-boundary reconfig。
- 源码审查结论：`AgentSpec` 是字段私有 data-only 配置；`AgentState` 持有唯一 `Conversation`、
  只暴露只读 getter 和受检队列/cursor transition，serde 通过 `Conversation::snapshot`/`restore`；
  `AgentRuntimeHandles` 独立于 state serde；`RunContext::derive_child` 继承父 cancel、共享预算 ledger
  并记录 sub-agent trace parent。
- M2 可依赖的当前 API：public 的 `AgentSpec`/`AgentState`/`LoopCursor`/`QueuedPivot`/
  `QueuedReconfig`/`RunContext` 及其只读 getter 和受检方法；crate-private 的 `LoopCursor::validate`、
  `can_transition_to`、`QueuedPivot::validate`、`QueuedReconfig::validate` 与 `AgentState::from_record`
  只服务 serde/状态校验。当前未暴露 `AgentLoop`、`feed`、unchecked cursor 恢复或 mutable
  conversation/pending 推进入口。
- 验证通过：`cargo fmt --all`；`cargo clippy --all-targets -- -D warnings`；
  `cargo test agent::`；`perl -e 'alarm 1800; exec @ARGV' cargo test --all --all-targets`；
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`；`git diff --check`。
- 已更新 `TODO.md`：`M1-R` 标题已标记 `[DONE]`，完成记录包含 review 结论、M2 API 清单、
  文档修正和验证结果。
- 最终 `git diff --check` 已通过。下一步提交本轮变更并停止。
