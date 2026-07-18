# M5-1 执行计划：扩展 `AgentParts` 覆盖 external、协作和交互状态

## 任务性质
功能任务。当前 `Agent::into_parts` / `AgentParts` 丢失了 interaction handler、
external delegates、retained external sessions、collaboration 配置与 live state。
需要扩展 `AgentParts` 让 `into_parts` 不再静默 drop 这些仍有语义价值的状态。

## 现状分析（已完成 code walk）
- `Agent` (src/facade/agent.rs:133) 缺失字段：interaction_handler / external_agents /
  last_external_sessions / collab(CollabState: config + live Arc<Mailbox/Blackboard/Plan>)。
- `RetainedExternalSession` 目前 `pub(crate)`，data-only 无句柄/凭据。
- `Mailbox`/`Blackboard`/`Plan`/`Collaboration`/`InteractionHandler`/`ManagedExternalDelegate` 均 public。

## 设计
1. `RetainedExternalSession` 改 `pub` + 在 facade/mod.rs 重导出。
2. `AgentParts` 新增 public 字段：interaction_handler / external_agents /
   retained_external_sessions / collaboration / mailbox / blackboard / plan。
   （collab 用 config + 三个 live 句柄暴露，镜像 Agent 访问器，不泄漏 pub(crate) CollabState。）
3. `Agent::into_parts` 填充新字段（析构 self.collab）。
4. snapshot.rs 补 imports；更新 AgentParts rustdoc + Debug；说明 into_parts 非完整 restore API。

## 测试（src/facade/agent/tests.rs）
- into_parts_carries_interaction_handler
- into_parts_carries_collaboration_state（dispatcher topology）
- into_parts_carries_external_delegates（claude_code）
- retained sessions：字段存在、fresh 为空；live 填充需真实 drive → 记录验证边界

## 验证命令
- cargo fmt --all
- cargo clippy --all-targets -- -D warnings
- cargo clippy --all-targets --features "external-claude-code external-codex external-opencode" -- -D warnings
- cargo test -p agent-lib --lib facade::agent::
- RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace

## 进度
- [x] 实现代码改动（RetainedExternalSession pub + 重导出；AgentParts +7 字段；into_parts 填充；rustdoc/Debug）
- [x] 新增测试（5 个 into_parts_* 全绿）
- [x] fmt/clippy(default+external)/test(facade::agent 49、lib 878)/doc 全绿
- [x] TODO.md M5-1 标 [DONE] + 完成记录
- [x] commit（待执行）

## 结论
into_parts 不再静默 drop interaction handler / external delegates / retained sessions / collab；
collab 以 config+live 句柄暴露可接管；retained session 非空内容需真实 drive，单测覆盖字段搬移与空态，
验证边界已记录。无 spec 偏差，无新增前置任务。
