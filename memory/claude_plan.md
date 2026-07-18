# M3-3 执行计划：让 restore 优先使用 snapshot 中的协作内容

## 任务（TODO.md M3-3）
更新 `AgentRestoreBuilder::build` / `CollabState::restore`，实现冲突策略：
**snapshot 内容为准，topology 只作为兼容旧 snapshot 的 provision hint**。
- snapshot 有 mailbox/blackboard/plan 内容 → 优先从 snapshot 恢复（即使 topology 未启用）。
- snapshot 缺内容但 topology 要求启用 → 才建空组件。
- snapshot 内容与 topology 冲突 → snapshot 权威；恢复后 `config` 需拓宽以保持一致。
- 恢复后 agent 能继续执行协作工具 / delegate workflow。
- 写入文档（docs/facade-api.md §15.2）。

## 现状调研（M3-2 已落地）
- `src/facade/collab.rs` `CollabState::restore(config, ids, mailbox, blackboard, plan)`：
  **当前是 topology 权威**——`config.*_enabled()` 决定是否建，snapshot 只补内容。
  冲突时（config 未启用但 snapshot 有内容）会丢弃 snapshot 内容 → 需要改成 snapshot 权威。
- `CollabState.config` 被 `Agent::collaboration()`（agent.rs:455）、Debug（:203）读取；
  `mailbox()/blackboard()/plan()` 访问器和 `CollabBridge::from_state` 读取 Option 原语。
  → 若 snapshot 恢复了 topology 未声明的原语，必须同步拓宽 `config`，避免
  `collaboration()` 与 `mailbox()` 不一致。
- `Collaboration` 有 const builder `.mailbox()/.blackboard()/.plan()` 可拓宽 flag。
- `AgentRestoreBuilder::build`（snapshot.rs:793）用 `resolve(None, delegation, ...)` 从
  topology 派生 config，再传 snapshot slices 给 `CollabState::restore`。

## 实现步骤
1. `src/facade/collab.rs` 改写 `CollabState::restore`：
   - 每个原语：`snapshot.map(from_snapshot).or_else(|| config.*_enabled().then(空建))`。
   - 用恢复结果拓宽 effective `config`（原语 is_some → 置对应 flag）。
   - 更新 doc 注释，说明 snapshot 权威 / topology 作为旧 snapshot provision hint。
2. `docs/facade-api.md` §15.2 增加协作 restore 冲突策略说明段落。
3. 更新 snapshot.rs `build()` 注释（原“substrate flags 决定 which”改为 snapshot 权威）。
4. 测试（`facade::agent::snapshot::tests`，snapshot_tests.rs 追加）：
   - 冲突：base agent（无 delegate，config 空）用手工带 mailbox 内容的 snapshot restore →
     mailbox 恢复且 `collaboration().mailbox_enabled()` 为真、内容可读、续 seq。
   - 旧格式兼容：用不含 mailbox/blackboard/plan/artifacts 字段的 JSON 反序列化为
     AgentSnapshot（`#[serde(default)]`），restore 成功、得到空协作底座。
   - （round-trip seq / blackboard offset 续操作 M3-2 已覆盖，确认保留。）

## 验证
- cargo fmt --all
- cargo clippy --all-targets -- -D warnings
- cargo test -p agent-lib --lib facade::agent::snapshot
- cargo test -p agent-lib --lib facade::collab
- cargo test -p agent-lib --lib agent::collab（回归）
- cargo test --all --all-targets
- RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace

## 状态：完成
M3-3 完成：`CollabState::restore` 改为 snapshot 权威（topology 仅作 provision hint），恢复后
拓宽 config 保持一致；facade-api.md §15.2 写入冲突策略与旧格式兼容说明；新增 4 个测试
（2 facade::agent::snapshot 冲突/旧格式，2 facade::collab restore 语义）。fmt/clippy/targeted/
full/doc 全绿，TODO.md 标记 [DONE]。

---

# M3-4：明确并实现顶层 artifact snapshot 策略

## 分析
- `AgentSnapshot.artifacts: Vec<ArtifactRef>` 顶层字段，capture 时恒写 `Vec::new()`（snapshot.rs:173）。
- restore 路径从不读取 `snapshot.artifacts`（只读 per-delegate `ExternalDelegateSnapshot.artifacts`）。
- `CollabState` 无独立 artifact store（collab.rs:236-239 明确：artifact store 只是 config flag，
  delegate artifact refs 已收进 `RunOutput.artifacts`）。artifact 现有真实来源：
  1. per-run：`RunOutput.artifacts`（瞬时，每次 run 后 surface）；
  2. per-external-delegate：`ExternalDelegateSnapshot.artifacts`（来自 `RetainedExternalSession.artifacts`，
     持久化进 snapshot 且 restore 时按 delegate 恢复）。
- 结论：无稳定 facade-level artifact store，不应伪造聚合语义 → 采用 **选项 2：保留兼容字段**。

## 决策：顶层 artifacts = 保留兼容字段（reserved compatibility field）
- 字段保留在 `AgentSnapshot`，带 `#[serde(default)]`，capture 恒为空，restore 完全忽略。
- 权威 artifact 来源明确为 per-run `RunOutput.artifacts` 与 per-external-delegate
  `ExternalDelegateSnapshot.artifacts`；顶层字段不作为行为来源。

## 实现步骤
1. `src/facade/agent/snapshot.rs`：
   - 更新模块级 doc（第 22-24 行“reserved for a later milestone”）与 struct/字段 doc（73-74、115-118），
     明确改为“保留兼容字段，非行为来源，artifacts 现由 RunOutput + ExternalDelegateSnapshot 持有，
     restore 忽略顶层字段”。
   - 更新 `capture` doc 说明 artifacts 恒空且为何（无稳定聚合 store）。
2. `docs/facade-api.md` §15.2 第 1016 行：把“见后续里程碑”改为定稿：明确顶层 artifacts 为保留兼容字段，
   调用方应从 `RunOutput.artifacts`（per-run）与 external delegate snapshot（持久 per-delegate）读取 artifacts，
   restore 不读取顶层字段。
3. `docs/refine.md` 条目 2：把 artifact 数据来源三问（127-130、137）标注为已决策：顶层 artifacts 保留兼容字段、
   不聚合，权威来源为 RunOutput + external delegate snapshot。
4. 测试（`src/facade/agent/snapshot_tests.rs` 追加 2 个）：
   - 序列化兼容：capture 得到的顶层 artifacts 为空，serde round-trip 后字段仍存在且为空。
   - restore 独立性：手工把非空 artifacts 嫁接到 snapshot（伪造旧/外部写入），restore 成功且顶层
     artifacts 不泄漏进行为（restored agent 无处可查询到它；per-delegate 语义不受影响）。

## 验证
- cargo fmt --all
- cargo clippy --all-targets -- -D warnings
- cargo test -p agent-lib --lib facade::agent::snapshot
- cargo test -p agent-lib --lib facade::collab（回归）
- RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
- （仅文档 + 测试改动，若无编译产物行为变化仍跑上述目标验证）

## 状态：完成
M3-4 完成：顶层 artifacts 定为保留兼容字段（capture 恒空、restore 忽略），文档定稿真实 artifact
来源（RunOutput + external delegate snapshot），新增 2 个测试。fmt/clippy/targeted/doc 全绿，
TODO.md 标记 [DONE]。

---

# M3-5 执行计划：Review 协作状态 snapshot 和 restore

## 任务（TODO.md M3-5）
Review 任务，检查范围：
- mailbox/blackboard/plan snapshot 类型是否 data-only、serde、兼容旧格式。
- `AgentSnapshot::capture` 是否读 live 状态而非 topology 默认值。
- `AgentRestoreBuilder` 是否优先用 snapshot 内容。
- artifact 策略在代码/文档是否一致。
- retained external session snapshot 是否未被本阶段改坏。
- 手工复核 docs/refine.md “协作状态 snapshot/restore” 条目状态，补充修复说明。

## 代码复核结论（已读源码确认）
- MailboxSnapshot/BlackboardSnapshot/PlanSnapshot 均 data-only（无 lock/handle）、
  derive Serialize/Deserialize；facade 层 AgentSnapshot.mailbox/blackboard/plan/artifacts
  带 #[serde(default)]，旧格式反序列化安全。✓
- capture 从 collab.mailbox/blackboard/plan 读 live snapshot()，artifacts 恒空（保留兼容字段）。✓
- CollabState::restore = snapshot 权威 + topology provision hint，恢复后拓宽 config。✓
- AgentRestoreBuilder::build 把 snapshot slices 传给 restore；external session 用
  snap.session/artifacts/status，未被改坏。✓
- 代码与文档 artifact 策略一致（顶层保留字段，权威来源 RunOutput + ExternalDelegateSnapshot）。✓

## 待办
1. 更新 docs/refine.md §2：把“协作状态运行时可用但 snapshot/restore 仍丢弃数据”条目状态
   标注为“已修复（M3-1..M3-4）”，补充当前修复说明（capture 读 live、restore snapshot 权威、
   artifact 保留字段策略），保持问题描述历史但明确现状。
2. 运行验证命令：fmt / clippy --all-targets -D warnings / 三个 targeted lib 测试 / cargo doc。
3. TODO.md 标记 M3-5 [DONE] 并填完成记录。
4. commit。

## 状态：完成
M3-5 完成：代码逐项复核（M3-1..M3-4 一致落地）+ 更新 docs/refine.md §2 标注已修复并补充修复结果
小节；fmt/clippy/三 targeted 测试/doc 全绿；TODO.md 标记 [DONE]。

---

# M4-1 执行计划：提供可直接装配默认 session handler 的 builder API

## 任务（TODO.md M4-1）
在 `ManagedExternalAgentBuilder` 上增加清晰 API，用于构造 agent 并自动装配默认
session handler，取代“先 build 再回填 builder”的绕路。

## 现状（src/facade/external.rs）
- `ManagedExternalAgentBuilder::build()`（681）校验 mode 后产出 `ManagedExternalAgent`，
  handler 为 None。
- `default_external_session_handler(&ManagedExternalAgent)`（809）异步 probe 并返回
  `Arc<RegistryExternalSessionHandler>`，但需要一个已构造的 agent → 调用方还要再
  `.session_handler(handler).build()`，体验绕。
- `drive_external`（1382）无 handler 直接 fail。
- 默认 feature 下 `build_default_registry` 命中 catch-all → fail-fast「enable feature」错误
  （runtime_feature_disabled），不含 secret。

## 设计
新增 `ManagedExternalAgentBuilder::build_with_default_session_handler(self) -> Result<ManagedExternalAgent, FacadeError>`（async）：
- 若 builder 已手工 `.session_handler(..)` → 直接 `self.build()`（honor 手工 handler，
  不触发 probe），保证不破坏自定义 handler 路径。
- 否则 `build()` 后调用 `default_external_session_handler(&agent)`，把返回 handler
  装到 agent 上返回。probe/feature 错误行为与现有 default handler 完全一致（非 secret）。
- capabilities 的 Probed 视图留给 M4-4（本任务只做 handler 装配）。

## 测试（facade::external tests）
1. 默认 feature（无 external-codex）：`codex().build_with_default_session_handler()`
   → Err(ExternalAgent{name:"codex", message contains "external-codex"})，不含 secret。
   （用 `#[cfg(not(feature="external-codex"))]` 守卫）
2. 手工 handler：`codex().session_handler(fake).build_with_default_session_handler()`
   → Ok，且 `session_handler().is_some()`；说明手工 handler 短路 probe、feature 无关。
3. 手工 `.session_handler(..).build()` 仍可 build（回归，已有 drive_external 测试覆盖，
   补一个直接断言）。
4. 启用 feature 的 probe 装配路径需真实 CLI，无法离线单测 → 在任务记录说明，靠
   feature clippy 覆盖编译。

## 验证
- cargo fmt --all
- cargo clippy --all-targets -- -D warnings
- cargo test -p agent-lib --lib facade::external
- （feature 编译）cargo clippy --all-targets --features "external-claude-code external-codex external-opencode external-acp" -- -D warnings

## 状态：完成
M4-1 完成：新增 `build_with_default_session_handler` 一步式 builder API（手工 handler 短路、否则 probe 装配默认 handler），3 个测试全绿，fmt/clippy(默认+全 feature)/doc/full 全绿，TODO.md 标记 [DONE]。

---

# M4-2 执行计划：修正 README managed external quick start

## 任务（TODO.md M4-2）
文档任务：让 external quick start 示例不再构造出「没有 session handler、build 后即可 run」的
`ManagedExternalAgent`，改用 M4-1 新增的 `build_with_default_session_handler().await?`。

## 现状调研
- `README.md` §4（143-176）：`ManagedExternalAgent::codex()...build()?` → `.external_agent(..)`
  → `agent.run_full(..)`，是「build 后即可 run 但无 handler」示例（会在运行时 fail）。
  尾注 175-176 提示手工 `default_external_session_handler(&agent)` 接 `.session_handler(..)`。
- `docs/facade-api.md`：§11.1（696-708）与 §17.3（1156-1198）同样 `.build()?` 后挂 external
  并 run_full，属同类违规示例。§11.2（751-770）已解释 handler 注入与 default handler。
- `docs/managed-external-agent.md`：设计文档，无 facade 构造 snippet；§21 M9（1562-1575）
  指向 `examples/support/managed.rs` 手工 scoped wiring（推荐的 managed 全手工路径）。
- 新 API：`ManagedExternalAgentBuilder::build_with_default_session_handler(self) -> Result<..>`
  （async，始终编译；默认 feature 下 fail-fast 非 secret 「enable external-* feature」错误）。

## 编辑计划
1. README.md §4 quick start：`.build()?` → `.build_with_default_session_handler().await?`；
   示例旁补说明：默认 crate build 不启用 CLI adapter，此调用需对应 `external-*` feature +
   本机 CLI login，否则 fail-fast（非 secret）；指向可运行示例。更新尾注保留手工
   `.session_handler(..)` 自定义 handler 路径说明。
2. docs/facade-api.md §11.1、§17.3：外部构造 `.build()?` → `.build_with_default_session_handler().await?`，
   保证不再出现 build 后即可 run 但无 handler 装配示例；§11.2 default handler 说明补一句
   指向 ergonomic 一步式 API。
3. docs/managed-external-agent.md：加简短「facade 构造」说明（在 §21 M9 examples 或 §1 附近），
   指出 ergonomic `build_with_default_session_handler()` 构造 + 需要 handler/feature/CLI login，
   与 README 一致；examples 仍是推荐的手工 scoped-wiring 路径，术语对齐。
4. 检查 examples/ managed examples 仍是推荐路径（手工 scoped wiring），术语一致，无需改代码。

## 验证
- cargo fmt --all（仅文档改动，无代码，但保持流程）
- cargo check --examples
- cargo clippy --all-targets --features "external-claude-code external-codex external-opencode external-acp" -- -D warnings
- grep 复核：docs 中不再有 build-then-run-without-handler 外部示例。
- 仅文档改动、无编译产物行为变化 → 不重跑全量 test 套件（复用上次绿）。

## 状态：完成
M4-2 完成：README / facade-api.md / managed-external-agent.md 的 external quick start 改用一步式
`build_with_default_session_handler().await?`，消除「build 后即可 run 但无 handler」示例并补默认 build/feature/CLI login 说明；examples 手工 scoped wiring 仍为推荐路径。fmt/check --examples/feature clippy 全绿，仅文档改动复用上次 full test 绿，TODO.md 标记 [DONE]。
