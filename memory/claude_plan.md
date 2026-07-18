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
